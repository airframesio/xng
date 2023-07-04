use actix_web::{HttpRequest, HttpResponse};

use crate::common::middleware::Authorized;

pub const ROUTE: &'static str = "/api/cleanup/";

pub async fn delete(req: HttpRequest, _: Authorized) -> HttpResponse {
    todo!()
}
