//! Output sinks: subscribe to the message bus and deliver messages somewhere
//! (console, JSONL files; Airframes asf-2.0 gRPC/QUIC and legacy JSON compat
//! land in M3).

pub mod acarsdec_json;
pub mod beast;
pub mod asf2_grpc;
pub mod asf2_quic;
pub mod console;
pub mod jsonl;
pub mod metrics;
pub mod sbs;
