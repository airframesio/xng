use actix_web::{guard, web};

mod middleware;
mod settings;

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::resource(settings::ROUTE)
            .guard(guard::Header("content-type", "application/json"))
            .route(web::get().to(settings::get))
            .route(web::post().to(settings::post)),
    );
}
