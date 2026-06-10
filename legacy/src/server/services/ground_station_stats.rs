use actix_web::web::Data;
use actix_web::{HttpRequest, HttpResponse};
use serde::Serialize;
use sqlx::FromRow;
use tokio::sync::RwLock;

use crate::common::middleware::Authorized;
use crate::server::db::StateDB;

use super::ServerServiceResponse;

pub const ROUTE: &'static str = "/api/ground-station/stats/";

#[derive(FromRow)]
struct GSStatRow {
    id: u32,
    name: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    msgs_heard_from: u32,
    msgs_heard_to: u32,
}

#[derive(Serialize)]
struct GroundStation {
    id: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    coords: Option<(f64, f64)>,

    msgs_heard_from: u32,
    msgs_heard_to: u32,
}

#[derive(Serialize)]
struct GroundStationResponse {
    ok: bool,
    body: Vec<GroundStation>,
}

pub async fn get(req: HttpRequest, _: Authorized) -> HttpResponse {
    let state_db = req
        .app_data::<Data<RwLock<StateDB>>>()
        .unwrap()
        .read()
        .await;
    if let Some(db) = state_db.db_pool() {
        let results = match sqlx::query_as::<_, GSStatRow>(
            "
            SELECT * FROM ground_stations gs
            ORDER BY gs.id ASC
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

        HttpResponse::Ok().json(GroundStationResponse {
            ok: true,
            body: results
                .into_iter()
                .map(|result| GroundStation {
                    id: result.id,
                    name: result.name,
                    coords: if result.latitude.is_some() && result.longitude.is_some() {
                        Some((result.longitude.unwrap(), result.latitude.unwrap()))
                    } else {
                        None
                    },
                    msgs_heard_from: result.msgs_heard_from,
                    msgs_heard_to: result.msgs_heard_to,
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
