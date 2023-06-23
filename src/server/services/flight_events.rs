use actix_web::web::{self, Data};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use tokio::sync::RwLock;

use crate::common::middleware::Authorized;
use crate::server::db::StateDB;
use crate::utils::normalize_tail;

use super::ServerServiceResponse;

pub const ROUTE: &'static str = "/api/flight/events/";

const DEFAULT_AE_LIMIT: u32 = 250;

#[derive(Debug, Deserialize)]
struct FlightEventsParam {
    limit: Option<u32>,
    icao: Option<String>,
    tail: Option<String>,
    callsign: Option<String>,
}

#[derive(Serialize)]
struct GroundStation {
    id: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,

    coords: (f64, f64),
}

#[derive(Serialize)]
struct FlightEvent {
    id: u32,
    ts: DateTime<Utc>,

    #[serde(skip_serializing_if = "Option::is_none")]
    icao: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    callsign: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    tail: Option<String>,

    signal: f64,
    freq_mhz: f64,

    coords: (f64, f64),

    #[serde(skip_serializing_if = "Option::is_none")]
    altitude: Option<u32>,

    gs: GroundStation,
}

#[derive(Serialize)]
struct FlightEventsResponse {
    ok: bool,
    body: Vec<FlightEvent>,
}

#[derive(FromRow)]
struct EventRow {
    id: u32,
    ts: DateTime<Utc>,
    icao_addr: Option<String>,
    callsign: Option<String>,
    tail: Option<String>,
    gs_id: u32,
    gs_name: Option<String>,
    gs_lat: f64,
    gs_lon: f64,
    signal: f64,
    freq_mhz: f64,
    latitude: f64,
    longitude: f64,
    altitude: Option<u32>,
}

pub async fn get(req: HttpRequest, _: Authorized) -> HttpResponse {
    let params = match web::Query::<FlightEventsParam>::from_query(req.query_string()) {
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
        let results = match sqlx::query_as::<_, EventRow>(
            "
            SELECT 
                ae.id, 
                ae.ts, 
                iif(a.icao IS NULL, NULL, printf('%06x', a.icao)) AS icao_addr, 
                ae.callsign, 
                ae.tail,
                gs.id AS gs_id,
                gs.name AS gs_name,
                gs.latitude AS gs_lat,
                gs.longitude AS gs_lon, 
                ae.signal, 
                ae.freq_mhz, 
                ae.latitude, 
                ae.longitude, 
                ae.altitude 
            FROM aircraft_events ae
            LEFT JOIN aircrafts a ON a.icao = ae.aircraft_icao OR a.tail = COALESCE(ae.tail, \"\")
            JOIN ground_stations gs ON gs.id = ae.gs_id
            WHERE ifnull(COALESCE(a.icao, \"\") = ?, 1) AND ifnull(COALESCE(ae.tail, \"\") = ?, 1) AND ifnull(COALESCE(ae.callsign, \"\") = ?, 1)
            ORDER BY ae.ts DESC
            LIMIT ?
            ",
        )
        .bind(
            if let Some(ref addr) = params.icao {
                u32::from_str_radix(addr.as_str(), 16).ok()
            } else {
                None        
            }
        )
        .bind(params.tail.as_ref().map(|x| normalize_tail(x)))
        .bind(&params.callsign)
        .bind(params.limit.unwrap_or(DEFAULT_AE_LIMIT))
        .fetch_all(db)
        .await {
            Ok(x) => x,
            Err(e) => return HttpResponse::InternalServerError().json(
                ServerServiceResponse {
                    ok: false,
                    message: Some(format!("Query failed: {}", e.to_string())),
                }
            )
        };

        HttpResponse::Ok().json(FlightEventsResponse {
            ok: true,
            body: results.into_iter().map(|result| FlightEvent {
                id: result.id,
                ts: result.ts,
                icao: result.icao_addr,
                callsign: result.callsign,
                tail: result.tail,
                signal: result.signal,
                freq_mhz: result.freq_mhz,
                coords: (result.longitude, result.latitude),
                altitude: result.altitude,
                gs: GroundStation {
                    id: result.gs_id,
                    name: result.gs_name,
                    coords: (result.gs_lon, result.gs_lat),
                }
            }).collect(),
        })
    } else {
        HttpResponse::NotImplemented().json(ServerServiceResponse {
            ok: false,
            message: Some(format!("State DB is disabled")),
        })
    }
}
