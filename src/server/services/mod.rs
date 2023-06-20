use actix_web::{guard, web};
use serde::Serialize;

mod flight_events;
mod ground_station_active;
mod ground_station_events;
mod ground_station_stats;

#[derive(Serialize)]
pub struct ServerServiceResponse {
    ok: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

pub fn config(cfg: &mut web::ServiceConfig) {
    // TODO: /api/freq/stats/
    // TODO: /api/flight/overview
    // TODO: /api/flight/{icao,tail,callsign}/:value/path

    cfg.service(
        web::resource(flight_events::ROUTE)
            .guard(guard::Header("content-type", "application/json"))
            .route(web::get().to(flight_events::get)),
    );

    cfg.service(
        web::resource(ground_station_events::ROUTE)
            .guard(guard::Header("content-type", "application/json"))
            .route(web::get().to(ground_station_events::get)),
    );
    cfg.service(
        web::resource(ground_station_stats::ROUTE)
            .guard(guard::Header("content-type", "application/json"))
            .route(web::get().to(ground_station_stats::get)),
    );
    cfg.service(
        web::resource(ground_station_active::ROUTE)
            .guard(guard::Header("content-type", "application/json"))
            .route(web::get().to(ground_station_active::get)),
    );
}
