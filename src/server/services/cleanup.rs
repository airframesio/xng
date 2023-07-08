use actix_web::web::{self, Data};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::common::middleware::Authorized;
use crate::server::db::StateDB;

use super::ServerServiceResponse;

pub const ROUTE: &'static str = "/api/cleanup/";

#[derive(Debug, Deserialize)]
struct CleanupParams {
    before: DateTime<Utc>,
}

pub async fn delete(req: HttpRequest, _: Authorized) -> HttpResponse {
    let params = match web::Query::<CleanupParams>::from_query(req.query_string()) {
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
        let deletions = [
            (
                "aircraft events",
                "
                DELETE FROM aircraft_events ae WHERE ae.ts < ? 
                ",
            ),
            (
                "ground station change events",
                "
                DELETE FROM ground_station_change_events gsce WHERE gsce.ts < ? 
                ",
            ),
        ];

        let mut ok = true;
        let mut msgs: Vec<String> = Vec::new();

        for query in deletions.iter() {
            match sqlx::query(query.1).bind(params.before).execute(db).await {
                Ok(x) => msgs.push(format!("removed {} {}", x.rows_affected(), query.0)),
                Err(e) => {
                    msgs.push(format!(
                        "encountered an error while removing {} ({})",
                        query.1,
                        e.to_string()
                    ));
                    ok = false;
                }
            }
        }

        HttpResponse::Ok().json(ServerServiceResponse {
            ok,
            message: Some(format!(
                "Delete operation {}: {}",
                if ok { "succeeded" } else { "failed" },
                msgs.join(", ")
            )),
        })
    } else {
        HttpResponse::NotImplemented().json(ServerServiceResponse {
            ok: false,
            message: Some(format!("State DB is disabled")),
        })
    }
}
