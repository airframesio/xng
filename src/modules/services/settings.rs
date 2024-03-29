use actix_web::http::header::ContentType;
use actix_web::web::{self, Data};
use actix_web::{HttpRequest, HttpResponse};
use log::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::common::middleware::Authorized;
use crate::modules::ModuleSettings;

pub const ROUTE: &'static str = "/api/settings/";

#[derive(Serialize)]
pub struct GetResponse {
    ok: bool,
    body: Value,
}

pub async fn get(req: HttpRequest) -> HttpResponse {
    let module_settings = req
        .app_data::<Data<RwLock<ModuleSettings>>>()
        .unwrap()
        .read()
        .await;

    HttpResponse::Ok()
        .content_type(ContentType::json())
        .json(GetResponse {
            ok: true,
            body: serde_json::to_value(&*module_settings).unwrap(),
        })
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

    if let Some(validator_callback) = module_settings.get_validator(&data.prop) {
        if let Err(e) = validator_callback(&data.value) {
            return HttpResponse::BadRequest().json(PatchResponse {
                ok: false,
                message: Some(format!(
                    "Provided value for prop {} failed validation check: {}",
                    data.prop,
                    e.to_string(),
                )),
            });
        }
    }

    let Some(value) = module_settings.props.get_mut(&data.prop) else {
        return HttpResponse::BadRequest().json(PatchResponse {
            ok: false,
            message: Some(format!("Specified property is not valid: {}", data.prop))
        });
    };

    let types_match = match (&value, &data.value) {
        (Value::Bool(_), Value::Bool(_))
        | (Value::Number(_), Value::Number(_))
        | (Value::String(_), Value::String(_))
        | (Value::Array(_), Value::Array(_)) => true,
        _ => false,
    };

    if !types_match {
        return HttpResponse::BadRequest().json(PatchResponse {
            ok: false,
            message: Some(format!(
                "Provided value for prop {} does not match types.",
                data.prop
            )),
        });
    }

    *value = data.value.clone();

    if let Err(e) = module_settings.reload_signaler.send(()) {
        warn!("Failed to signal reload: {}", e.to_string());
        return HttpResponse::Ok().body(
            serde_json::to_string(&PatchResponse {
                ok: true,
                message: Some(format!("Could not reload settings: {}", e.to_string())),
            })
            .unwrap(),
        );
    }

    HttpResponse::Ok().json(PatchResponse {
        ok: true,
        message: None,
    })
}
