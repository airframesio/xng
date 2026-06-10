use actix_web::web::Data;
use actix_web::{HttpRequest, HttpResponse};

use serde::Serialize;
use tokio::sync::RwLock;

use crate::common::middleware::Authorized;
use crate::modules::settings::{GroundStation, ModuleSettings};

pub const ROUTE: &'static str = "/api/ground-station/active/";

#[derive(Serialize)]
struct GSActiveResponse {
    ok: bool,
    body: Vec<GroundStation>,
}

pub async fn get(req: HttpRequest, _: Authorized) -> HttpResponse {
    let module_settings = req
        .app_data::<Data<RwLock<ModuleSettings>>>()
        .unwrap()
        .read()
        .await;

    HttpResponse::Ok().json(GSActiveResponse {
        ok: true,
        body: module_settings.stations.clone(),
    })
}
