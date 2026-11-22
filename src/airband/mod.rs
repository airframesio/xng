//! Airband voice in the xng runtime.
//!
//! Airband channels produce audio, which does not fit the message bus. The
//! channel set ([`set::AirbandSet`]) still puts one message per transmission
//! start/end on the bus (so console, JSONL, MQTT, ZMQ and asf-2.0 see the
//! activity), and publishes the audio itself on the process-wide
//! [`hub::AudioHub`]. Audio outputs (recordings, UDP PCM, the dashboard's
//! live listening page, Icecast, the Airframes ATC feeder) subscribe there.
//!
//! Streams on the hub:
//! - one per channel ([`hub::StreamId::Channel`]),
//! - `mix`: every channel of a session summed (rtl_airband's mixer),
//! - `scan`: follows one channel at a time, scanner style
//!   ([`scan_stream`], driven by `xng_mode_airband::scan::ScanController`).

pub mod atc_auth;
pub mod atc_proto;
pub mod cli;
pub mod config;
pub mod hub;
#[cfg(feature = "opus")]
pub mod opus_enc;
pub mod scan_stream;
pub mod set;
pub mod sherpa;
pub mod transcribe;

pub use config::{AirbandChannel, AirbandConfig, AirbandOutputs};
pub use hub::{hub, AudioEvent, AudioFrame, ChannelMeta, StreamId};

/// Resolves when the message bus closes, i.e. the session is ending. Audio
/// outputs subscribe to the process-wide hub, which never closes, so they
/// watch the bus to know when to finish.
pub async fn bus_closed(mut rx: tokio::sync::broadcast::Receiver<std::sync::Arc<xng_types::Message>>) {
    use tokio::sync::broadcast::error::RecvError;
    loop {
        if let Err(RecvError::Closed) = rx.recv().await {
            break;
        }
    }
}
