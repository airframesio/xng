use std::future::Future;
use std::pin::Pin;

use actix_web::dev::Payload;
use actix_web::error::{
    ErrorExpectationFailed, ErrorNetworkAuthenticationRequired, ErrorUnauthorized,
};
use actix_web::http::header;
use actix_web::web::Data;
use actix_web::{Error, FromRequest, HttpRequest};
use reqwest::Method;
use tokio::sync::RwLock;

use crate::modules::settings::ModuleSettings;

pub struct Authorized;

impl FromRequest for Authorized {
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Authorized, Error>>>>;

    fn from_request(req: &HttpRequest, pl: &mut Payload) -> Self::Future {
        // NOTE: if we can't find ModuleSettings, something is very, very wrong
        let module_config = req
            .app_data::<Data<RwLock<ModuleSettings>>>()
            .unwrap()
            .clone();
        let method = req.method().clone();
        let headers = req.headers().clone();

        Box::pin(async move {
            let module_config = module_config.read().await;

            if (method == Method::POST || method == Method::PATCH)
                && module_config.disable_api_control
            {
                return Err(ErrorExpectationFailed("API Control is disabled"));
            }

            let Some(ref api_token) = module_config.api_token else {
                  return Ok(Authorized);
            };
            let Some(ref user_token) = headers.get(header::AUTHORIZATION) else {
                return Err(ErrorNetworkAuthenticationRequired("Missing authorization token"));  
            };
            match user_token.to_str() {
                Ok(token) => {
                    if token == api_token {
                        return Ok(Authorized);
                    }
                }
                Err(e) => {
                    return Err(ErrorUnauthorized(format!(
                        "Authorization token is malformed: {}",
                        e.to_string()
                    )))
                }
            }

            Err(ErrorUnauthorized("Invalid authorization token provided"))
        })
    }
}
