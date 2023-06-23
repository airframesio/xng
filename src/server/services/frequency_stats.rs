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

    count: u32,
    last_heard: DateTime<Utc>,
}

#[derive(Serialize)]
struct FrequencyStats {
    freq_mhz: f64,
    gs_id: u32,

    count: u32,
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
            SELECT * FROM frequency_stats f
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
                    gs_id: result.gs_id,
                    count: result.count,
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
