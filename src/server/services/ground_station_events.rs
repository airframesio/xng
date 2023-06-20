use actix_web::http::header::ContentType;
use actix_web::web::{self, Data};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use tokio::sync::RwLock;

use crate::common::middleware::Authorized;
use crate::server::db::StateDB;
use crate::server::services::ServerServiceResponse;

pub const ROUTE: &'static str = "/api/ground-station/events/";

const DEFAULT_GSCE_LIMIT: u32 = 250;

#[derive(Serialize)]
pub struct GroundStationChangeEvent {
    ts: DateTime<Utc>,
    gs_id: u64,
    name: Option<String>,
    kind: String,

    #[serde(skip_serializing_if = "Value::is_null")]
    old: Value,

    #[serde(skip_serializing_if = "Value::is_null")]
    new: Value,
}

#[derive(FromRow)]
struct GSCEventRow {
    ts: DateTime<Utc>,
    gs_id: i64,
    name: Option<String>,
    kind: String,
    old: String,
    new: String,
}

#[derive(Serialize)]
struct GroundStationEventResponse {
    ok: bool,
    body: Vec<GroundStationChangeEvent>,
}

#[derive(Debug, Deserialize)]
struct GroundStationEventsParam {
    limit: Option<u32>,
    gs_id: Option<u32>,
}

pub async fn get(req: HttpRequest, _: Authorized) -> HttpResponse {
    let params = match web::Query::<GroundStationEventsParam>::from_query(req.query_string()) {
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
        let results = match sqlx::query_as::<_, GSCEventRow>(
            "
            SELECT gsce.ts, gsce.gs_id, gs.name, gsce.type AS kind, gsce.old, gsce.new FROM ground_station_change_events gsce 
            JOIN ground_stations gs ON gs.id = gsce.gs_id
            WHERE ifnull(gsce.gs_id = ?, 1)
            ORDER BY gsce.ts DESC
            LIMIT ?
            ",
        )
        .bind(&params.gs_id)
        .bind(params.limit.unwrap_or(DEFAULT_GSCE_LIMIT))
        .fetch_all(db)
        .await
        {
            Ok(x) => x,
            Err(e) => {
                return HttpResponse::InternalServerError().json(
                    ServerServiceResponse {
                        ok: false,
                        message: Some(format!("Query failed: {}", e.to_string()))
                    }
                )
            }
        };

        let mut events: Vec<GroundStationChangeEvent> = Vec::new();
        for result in results {
            events.push(GroundStationChangeEvent {
                ts: result.ts,
                gs_id: result.gs_id as u64,
                name: result.name,
                kind: result.kind,
                old: serde_json::from_str(result.old.as_str()).unwrap_or(Value::Null),
                new: serde_json::from_str(result.new.as_str()).unwrap_or(Value::Null),
            });
        }

        HttpResponse::Ok()
            .content_type(ContentType::json())
            .json(GroundStationEventResponse {
                ok: true,
                body: events,
            })
    } else {
        HttpResponse::NotImplemented().json(ServerServiceResponse {
            ok: false,
            message: Some(format!("State DB is disabled")),
        })
    }
}
