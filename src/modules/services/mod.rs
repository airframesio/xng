use actix_web::{guard, web};

mod session;
mod settings;

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::resource(settings::ROUTE)
            .guard(guard::Header("content-type", "application/json"))
            .route(web::get().to(settings::get))
            .route(web::patch().to(settings::patch)),
    );

    cfg.service(
        web::resource(session::ROUTE)
            .guard(guard::Header("content-type", "application/json"))
            .route(web::delete().to(session::delete)),
    );

    // TODO: add service to force end session
}
