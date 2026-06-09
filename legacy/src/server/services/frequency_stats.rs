use actix_web::web::Data;
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use tokio::sync::RwLock;

use crate::common::middleware::Authorized;
use crate::server::db::StateDB;

use super::ServerServiceResponse;

pub const ROUTE: &'static str = "/api/frequency/stats/";

#[derive(FromRow)]
struct EventRow {
    khz: u32,
    gs_id: u32,

    to_gs: u32,
    from_gs: u32,
    last_heard: DateTime<Utc>,

    name: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
}

#[derive(Serialize)]
struct GroundStation {
    id: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,

    coords: Option<(f64, f64)>,
}

#[derive(Serialize)]
struct FrequencyStats {
    freq_mhz: f64,
    gs: GroundStation,

    to_gs: u32,
    from_gs: u32,
    last_heard: DateTime<Utc>,
}

#[derive(Serialize)]
struct FreqStatsResponse {
    ok: bool,
    body: Vec<FrequencyStats>,
}

pub async fn get(req: HttpRequest, _: Authorized) -> HttpResponse {
    let state_db = req
        .app_data::<Data<RwLock<StateDB>>>()
        .unwrap()
        .read()
        .await;

    if let Some(db) = state_db.db_pool() {
        let results = match sqlx::query_as::<_, EventRow>(
            "
            SELECT f.khz, f.gs_id, f.to_gs, f.from_gs, f.last_heard, gs.name, gs.latitude, gs.longitude FROM frequency_stats f
            JOIN ground_stations gs ON f.gs_id = gs.id
            ORDER BY f.khz ASC
            ",
        )
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

        HttpResponse::Ok().json(FreqStatsResponse {
            ok: true,
            body: results
                .into_iter()
                .map(|result| FrequencyStats {
                    freq_mhz: result.khz as f64 / 1000.0,
                    gs: GroundStation {
                        id: result.gs_id,
                        name: result.name,
                        coords: if result.latitude.is_some() && result.longitude.is_some() {
                            Some((result.longitude.unwrap(), result.latitude.unwrap()))
                        } else {
                            None
                        },
                    },
                    to_gs: result.to_gs,
                    from_gs: result.from_gs,
                    last_heard: result.last_heard,
                })
                .collect(),
        })
    } else {
        HttpResponse::NotImplemented().json(ServerServiceResponse {
            ok: false,
            message: Some(format!("State DB is disabled")),
        })
    }
}
