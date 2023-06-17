use actix_web::{web::Data, HttpRequest, HttpResponse};
use log::*;
use serde::Serialize;
use tokio::sync::RwLock;

use crate::modules::settings::ModuleSettings;

use super::middleware::Authorized;

pub const ROUTE: &'static str = "/api/session";

#[derive(Serialize)]
pub struct DeleteResponse {
    ok: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

pub async fn delete(req: HttpRequest, _: Authorized) -> HttpResponse {
    let module_settings = req
        .app_data::<Data<RwLock<ModuleSettings>>>()
        .unwrap()
        .read()
        .await;

    if let Err(e) = module_settings.end_session_signaler.send(()) {
        error!("Failed to end session: {}", e.to_string());
        return HttpResponse::InternalServerError().body(
            serde_json::to_string(&DeleteResponse {
                ok: false,
                message: Some(format!("Failed to end session: {}", e.to_string())),
            })
            .unwrap(),
        );
    }

    HttpResponse::Ok().body(
        serde_json::to_string(&DeleteResponse {
            ok: true,
            message: None,
        })
        .unwrap(),
    )
}
