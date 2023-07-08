use actix_web::{
    web::{self, Data},
    HttpRequest, HttpResponse,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use tokio::sync::RwLock;

use crate::common::middleware::Authorized;
use crate::server::db::StateDB;

use super::ServerServiceResponse;

pub const FIELD_AIRCRAFT_ICAO: &'static str = "aircraft_icao";
pub const FIELD_CALLSIGN: &'static str = "callsign";
pub const FIELD_TAIL: &'static str = "tail";

pub const ROUTE: &'static str = "/api/flight/";
pub const VALID_FIELDS: [&'static str; 3] = [FIELD_AIRCRAFT_ICAO, FIELD_CALLSIGN, FIELD_TAIL];

#[derive(Debug, Deserialize)]
struct FlightParams {
    field: Option<String>,
    value: Option<String>,
    since: DateTime<Utc>,
}

#[derive(FromRow)]
struct FlightSummaryRow {
    ts: DateTime<Utc>,

    icao_addr: Option<String>,
    callsign: Option<String>,
    tail: Option<String>,

    gs_id: u32,

    signal: f64,
    freq_mhz: f64,

    latitude: f64,
    longitude: f64,

    altitude: Option<u32>,

    prev_latitude: Option<f64>,
    prev_longitude: Option<f64>,
}

#[derive(Debug, Serialize)]
struct FlightSummary {
    icao: Option<String>,
    callsign: Option<String>,
    tail: Option<String>,

    last_heard: DateTime<Utc>,
    last_signal: f64,
    last_freq_mhz: f64,

    last_gs_id: u32,

    coords: (f64, f64),
    altitude: Option<u32>,

    prev_coords: Option<(f64, f64)>,
}

#[derive(Serialize)]
struct FlightSummaryResponse {
    ok: bool,
    body: Vec<FlightSummary>,
}

#[derive(FromRow)]
struct FlightDetailRow {
    ts: DateTime<Utc>,

    icao_addr: Option<String>,
    callsign: Option<String>,
    tail: Option<String>,

    gs_id: u32,

    signal: f64,
    freq_mhz: f64,

    latitude: f64,
    longitude: f64,

    altitude: Option<u32>,
}

#[derive(Debug, Serialize)]
struct FlightDetail {
    icao: Option<String>,
    callsign: Option<String>,
    tail: Option<String>,

    ts: DateTime<Utc>,

    signal: f64,
    freq_mhz: f64,

    gs_id: u32,

    coords: (f64, f64),
    altitude: Option<u32>,
}

#[derive(Serialize)]
struct FlightDetailResponse {
    ok: bool,
    body: Vec<FlightDetail>,
}

pub async fn get(req: HttpRequest, _: Authorized) -> HttpResponse {
    let default_field = String::from("callsign");

    let params = match web::Query::<FlightParams>::from_query(req.query_string()) {
        Ok(x) => x,
        Err(e) => {
            return HttpResponse::InternalServerError().json(ServerServiceResponse {
                ok: false,
                message: Some(format!("Failed to get query params: {}", e.to_string())),
            })
        }
    };

    let state_db = req
        .app_data::<Data<RwLock<StateDB>>>()
        .unwrap()
        .read()
        .await;
    if let Some(db) = state_db.db_pool() {
        let field = params.field.as_ref().unwrap_or(&default_field);
        if !VALID_FIELDS
            .iter()
            .any(|&x| field.to_lowercase().as_str() == x)
        {
            return HttpResponse::BadRequest().json(ServerServiceResponse {
                ok: false,
                message: Some(format!(
                    "{} is not a valid field, expected: {:?}",
                    field, VALID_FIELDS
                )),
            });
        }

        if params.value.is_none() {
            let query = format!("
                WITH grouped_events AS (
                    SELECT ROW_NUMBER() OVER (PARTITION BY ae.{} ORDER BY ae.ts DESC) AS row, ae.* FROM aircraft_events ae
                )
                SELECT 
                    ge.*, 
                    ge2.latitude AS prev_latitude, 
                    ge2.longitude AS prev_longitude,
                    iif(ae.aircraft_icao IS NULL, NULL, printf('%06x', ae.aircraft_icao)) AS icao_addr
                FROM grouped_events ge
                LEFT JOIN grouped_events ge2 ON ge.{} = ge2.{} AND ge2.row = 2
                WHERE ge.row = 1 
                    AND ifnull(ge.ts >= ?, 1)
                ORDER BY ge.ts DESC
            ", field, field, field);
            let results = match sqlx::query_as::<_, FlightSummaryRow>(query.as_str())
                .bind(params.since)
                .fetch_all(db)
                .await
            {
                Ok(x) => x,
                Err(e) => {
                    return HttpResponse::InternalServerError().json(ServerServiceResponse {
                        ok: false,
                        message: Some(format!("Query failed: {}", e.to_string())),
                    })
                }
            };

            HttpResponse::Ok().json(FlightSummaryResponse {
                ok: true,
                body: results
                    .iter()
                    .map(|x| FlightSummary {
                        icao: x.icao_addr.clone(),
                        callsign: x.callsign.clone(),
                        tail: x.tail.clone(),

                        last_heard: x.ts,
                        last_signal: x.signal,
                        last_freq_mhz: x.freq_mhz,

                        last_gs_id: x.gs_id,

                        coords: (x.latitude, x.longitude),
                        altitude: x.altitude,
                        prev_coords: match (x.prev_latitude, x.prev_longitude) {
                            (Some(x), Some(y)) => Some((x, y)),
                            (None, _) | (_, None) => None,
                        },
                    })
                    .collect(),
            })
        } else {
            let query = format!(
                "
                SELECT 
                    ae.*,
                    iif(ae.aircraft_icao IS NULL, NULL, printf('%06x', ae.aircraft_icao)) AS icao_addr
                FROM aircraft_events ae
                WHERE ae.{} = ?
                    AND ifnull(ae.ts >= ?, 1)
                ORDER BY ae.ts ASC
            ",
                field
            );
            let mut icao_value: u32 = 0;
            if field == FIELD_AIRCRAFT_ICAO {
                icao_value = match params.value.as_ref().unwrap().parse::<u32>() {
                    Ok(x) => x,
                    Err(e) => {
                        return HttpResponse::InternalServerError().json(ServerServiceResponse {
                            ok: false,
                            message: Some(format!(
                                "Provided value for field {} is not a valid number, {}: {}",
                                FIELD_AIRCRAFT_ICAO,
                                params.value.as_ref().unwrap(),
                                e.to_string()
                            )),
                        })
                    }
                };
            }

            let mut query_builder = sqlx::query_as::<_, FlightDetailRow>(query.as_str());
            if field == FIELD_AIRCRAFT_ICAO {
                query_builder = query_builder.bind(icao_value);
            } else {
                query_builder = query_builder.bind(&params.value);
            }
            let results = match query_builder.bind(params.since).fetch_all(db).await {
                Ok(x) => x,
                Err(e) => {
                    return HttpResponse::InternalServerError().json(ServerServiceResponse {
                        ok: false,
                        message: Some(format!("Query failed: {}", e.to_string())),
                    })
                }
            };

            HttpResponse::Ok().json(FlightDetailResponse {
                ok: true,
                body: results
                    .iter()
                    .map(|x| FlightDetail {
                        ts: x.ts,
                        icao: x.icao_addr.clone(),
                        callsign: x.callsign.clone(),
                        tail: x.tail.clone(),
                        signal: x.signal,
                        freq_mhz: x.freq_mhz,
                        gs_id: x.gs_id,
                        coords: (x.latitude, x.longitude),
                        altitude: x.altitude,
                    })
                    .collect(),
            })
        }
    } else {
        HttpResponse::NotImplemented().json(ServerServiceResponse {
            ok: false,
            message: Some(format!("State DB is disabled")),
        })
    }
}
