use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identity of the feeding station. Airframes is moving to UUID station ids
/// (legacy human idents like `KE-KSEA-ACARS` remain as the display ident).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StationIdentity {
    pub id: Uuid,
    /// Human-readable station ident, e.g. `KE-KSEA-ACARS1`.
    pub ident: String,
}

impl StationIdentity {
    pub fn new(ident: impl Into<String>) -> Self {
        Self { id: Uuid::new_v4(), ident: ident.into() }
    }
}

/// The producing application (xng itself, or a wrapped external decoder).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
}

impl AppInfo {
    pub fn xng() -> Self {
        Self {
            name: "xng".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

/// The SDR device a message was captured on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdrInfo {
    /// User-assigned id for this device within the config (e.g. `vhf0`).
    pub id: String,
    /// Driver name, e.g. `rtlsdr`, `airspyhf`, `sdrplay`, `file`.
    pub driver: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
}

/// The logical channel within a capture that produced a message.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChannelInfo {
    /// Channel index within its capture/channelizer.
    pub index: u32,
    /// Channel center frequency in Hz.
    pub frequency_hz: u64,
    /// Channel (post-channelizer) sample rate in Hz.
    pub sample_rate: f64,
}

/// Full provenance attached to every normalized message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    pub station: StationIdentity,
    pub app: AppInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdr: Option<SdrInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<ChannelInfo>,
}
