use actix_web::web::{self, Data};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use tokio::sync::RwLock;

use crate::common::middleware::Authorized;
use crate::server::db::StateDB;

use super::ServerServiceResponse;

pub const ROUTE: &'static str = "/api/extremities/";

#[derive(FromRow)]
struct EventRow {
    #[sqlx(rename = "id")]
    _id: u32,

    ts: DateTime<Utc>,
    aircraft_icao: Option<u32>,
    callsign: Option<String>,
    tail: Option<String>,

    #[sqlx(rename = "gs_id")]
    _gs_id: u32,
    signal: f64,
    freq_mhz: f64,
    latitude: f64,
    longitude: f64,
    altitude: Option<u32>,
}

#[derive(Serialize)]
struct AircraftEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    icao: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    callsign: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    tail: Option<String>,

    ts: DateTime<Utc>,
    signal: f64,
    freq_mhz: f64,
    coords: (f64, f64),

    #[serde(skip_serializing_if = "Option::is_none")]
    altitude: Option<u32>,
}

enum ExtremityDirection {
    North,
    East,
    South,
    West,
}

#[derive(Serialize)]
struct ExtremitiesData {
    #[serde(skip_serializing_if = "Option::is_none")]
    northmost: Option<AircraftEvent>,

    #[serde(skip_serializing_if = "Option::is_none")]
    eastmost: Option<AircraftEvent>,

    #[serde(skip_serializing_if = "Option::is_none")]
    southmost: Option<AircraftEvent>,

    #[serde(skip_serializing_if = "Option::is_none")]
    westmost: Option<AircraftEvent>,
}

#[derive(Serialize)]
struct ExtremitiesResponse {
    ok: bool,
    body: ExtremitiesData,
}

async fn get_flight_event(
    db: &SqlitePool,
    dir: ExtremityDirection,
    lat: f64,
    lon: f64,
) -> Option<AircraftEvent> {
    let query = format!(
        "WITH norm_aircraft_events AS (
            SELECT 
                ae.id, 
                ae.ts, 
                ae.aircraft_icao, 
                ae.callsign, 
                ae.tail, 
                ae.gs_id, 
                ae.signal, 
                ae.freq_mhz, 
                ae.latitude-({}) AS norm_latitude, 
                ae.longitude-({}) AS norm_longitude,
                ae.latitude,
                ae.longitude,
                ae.altitude 
            FROM aircraft_events ae             
        )
        SELECT * FROM norm_aircraft_events ae {} LIMIT 1",
        lat,
        lon,
        match dir {
            ExtremityDirection::North => "ORDER BY ae.norm_latitude DESC",
            ExtremityDirection::East => "ORDER BY ae.norm_longitude DESC",
            ExtremityDirection::South => "ORDER BY ae.norm_latitude ASC",
            ExtremityDirection::West => "ORDER BY ae.norm_longitude ASC",
        }
    );

    sqlx::query_as::<_, EventRow>(query.as_str())
        .fetch_one(db)
        .await
        .map(|result| AircraftEvent {
            icao: result.aircraft_icao.map(|x| format!("{:06X}", x)),
            callsign: result.callsign,
            tail: result.tail,
            ts: result.ts,
            signal: result.signal,
            freq_mhz: result.freq_mhz,
            coords: (result.longitude, result.latitude),
            altitude: result.altitude,
        })
        .ok()
}

#[derive(Debug, Deserialize)]
struct ExtremitiesParam {
    lat: Option<f64>,
    lon: Option<f64>,
}

pub async fn get(req: HttpRequest, _: Authorized) -> HttpResponse {
    let params = match web::Query::<ExtremitiesParam>::from_query(req.query_string()) {
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
        let lat = params.lat.unwrap_or(0.0);
        let lon = params.lon.unwrap_or(0.0);

        HttpResponse::Ok().json(ExtremitiesResponse {
            ok: true,
            body: ExtremitiesData {
                northmost: get_flight_event(db, ExtremityDirection::North, lat, lon).await,
                eastmost: get_flight_event(db, ExtremityDirection::East, lat, lon).await,
                southmost: get_flight_event(db, ExtremityDirection::South, lat, lon).await,
                westmost: get_flight_event(db, ExtremityDirection::West, lat, lon).await,
            },
        })
    } else {
        HttpResponse::NotImplemented().json(ServerServiceResponse {
            ok: false,
            message: Some(format!("State DB is disabled")),
        })
    }
}
