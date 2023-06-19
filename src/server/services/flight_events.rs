use actix_web::{HttpRequest, HttpResponse};

use crate::common::middleware::Authorized;

pub const ROUTE: &'static str = "/api/flight/events/";

pub async fn get(req: HttpRequest, _: Authorized) -> HttpResponse {
    todo!()
}
