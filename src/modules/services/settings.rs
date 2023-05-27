use actix_web::http::header::ContentType;
use actix_web::web::Data;
use actix_web::{HttpRequest, HttpResponse};
use serde::Serialize;
use tokio::sync::RwLock;

use crate::modules::ModuleSettings;

use super::middleware::Authorized;

pub const ROUTE: &'static str = "/api/settings";

pub async fn get(req: HttpRequest) -> HttpResponse {
    let module_settings = req
        .app_data::<Data<RwLock<ModuleSettings>>>()
        .unwrap()
        .read()
        .await;

    HttpResponse::Ok()
        .content_type(ContentType::json())
        .body(serde_json::to_string(&*module_settings).unwrap())
}

pub async fn post(_: Authorized) -> HttpResponse {
    HttpResponse::Ok().body("Test")
}
