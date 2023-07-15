pub mod arguments;
pub mod batcher;
pub mod es_utils;
pub mod events;
pub mod formats;
pub mod frame;
pub mod middleware;
pub mod wkt;

pub const AIRFRAMESIO_HOST: &'static str = "feed.acars.io";

pub const AIRFRAMESIO_DUMPHFDL_TCP_PORT: u16 = 5556;
pub const AIRFRAMESIO_DUMPVDL2_UDP_PORT: u16 = 5552;
