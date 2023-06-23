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

#[derive(Clone, FromRow, Serialize)]
struct FrequencyStats {
    #[sqlx(rename = "khz")]
    freq: f64,
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
        let mut results = match sqlx::query_as::<_, FrequencyStats>(
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
                .iter_mut()
                .map(|x| {
                    x.freq /= 1000.0;
                    x.clone()
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
