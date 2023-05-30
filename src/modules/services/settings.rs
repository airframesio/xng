use actix_web::http::header::ContentType;
use actix_web::web::{self, Data};
use actix_web::{HttpRequest, HttpResponse};
use log::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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

#[derive(Deserialize)]
pub struct PatchRequest {
    prop: String,
    value: Value,
}

#[derive(Serialize)]
pub struct PatchResponse {
    ok: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

pub async fn patch(req: HttpRequest, _: Authorized, data: web::Json<PatchRequest>) -> HttpResponse {
    let mut module_settings = req
        .app_data::<Data<RwLock<ModuleSettings>>>()
        .unwrap()
        .write()
        .await;

    let Some(value) = module_settings.props.get_mut(&data.prop) else {
        return HttpResponse::BadRequest().body(
            serde_json::to_string(&PatchResponse {
                ok: false,
                message: Some(format!("Specified property is not valid: {}", data.prop))
            })
            .unwrap(),
        );
    };

    let types_match = match (&value, &data.value) {
        (Value::Bool(_), Value::Bool(_))
        | (Value::Number(_), Value::Number(_))
        | (Value::String(_), Value::String(_)) => true,
        _ => false,
    };

    if !types_match {
        return HttpResponse::BadRequest().body(
            serde_json::to_string(&PatchResponse {
                ok: false,
                message: Some(format!(
                    "Provided value for prop {} does not match types.",
                    data.prop
                )),
            })
            .unwrap(),
        );
    }

    *value = data.value.clone();

    if let Err(e) = module_settings.reload_signaler.send(()) {
        error!("Failed to signal reload: {}", e.to_string());
    }

    HttpResponse::Ok().body(
        serde_json::to_string(&PatchResponse {
            ok: true,
            message: None,
        })
        .unwrap(),
    )
}
