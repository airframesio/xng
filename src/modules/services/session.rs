use actix_web::{web::Data, HttpRequest, HttpResponse};
use log::*;
use serde::Serialize;
use tokio::sync::RwLock;

use crate::common::middleware::Authorized;
use crate::modules::session::EndSessionReason;
use crate::modules::settings::ModuleSettings;

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

    if let Err(e) = module_settings
        .end_session_signaler
        .send(EndSessionReason::UserAPIControl)
    {
        error!("Failed to end session: {}", e.to_string());
        return HttpResponse::InternalServerError().json(DeleteResponse {
            ok: false,
            message: Some(format!("Failed to end session: {}", e.to_string())),
        });
    }

    HttpResponse::Ok().json(DeleteResponse {
        ok: true,
        message: None,
    })
}
