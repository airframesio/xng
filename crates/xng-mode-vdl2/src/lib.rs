//! Native VDL Mode 2 decode core.
//!
//! Pipeline per channel: wideband IQ → [`xng_dsp::Ddc`] → 50 kHz channel
//! IQ → [`demod::Vdl2Demod`] (D8PSK burst acquisition, header,
//! deinterleave + RS(255,249)) → [`avlc`] frame scan → ACARS-over-AVLC via
//! [`xng_acars::block`] → [`xng_types::Message`].
//!
//! See PROVENANCE.md for clean-room sourcing (no GPL decoder code used).

pub mod avlc;
pub mod demod;
pub mod header;
pub mod interleave;
pub mod modulate;
pub mod scramble;

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::rs::ReedSolomon;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Internal channel rate (≈4.76 samples/symbol).
pub const CHANNEL_RATE: f64 = 50_000.0;
/// One-sided passband: D8PSK 10.5 kBd, RC α=0.6 → ±8.4 kHz.
pub const CHANNEL_PASSBAND_HZ: f64 = 8_500.0;

/// One decoded AVLC frame plus its ACARS content when present.
pub struct Vdl2Frame {
    pub avlc: avlc::AvlcFrame,
    pub acars: Option<xng_acars::block::AcarsBlock>,
    pub rs_corrected: usize,
}

pub struct Vdl2ChannelDecoder {
    ddc: Option<Ddc>,
    demod: demod::Vdl2Demod,
    rs: ReedSolomon,
    channel_buf: Vec<Complex<f32>>,
}

impl Vdl2ChannelDecoder {
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, CHANNEL_RATE, freq_offset_hz, CHANNEL_PASSBAND_HZ)?)
        };
        Ok(Self {
            ddc,
            demod: demod::Vdl2Demod::new(CHANNEL_RATE),
            rs: interleave::vdl2_rs(),
            channel_buf: Vec::new(),
        })
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<Vdl2Frame> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };
        let mut out = Vec::new();
        for burst in self.demod.process(channel, &self.rs) {
            for frame in avlc::scan(&burst.bits) {
                let acars = match frame.payload {
                    avlc::Payload::Acars => {
                        let start = frame.info.iter().position(|&b| b != 0xFF).unwrap_or(0);
                        xng_acars::block::parse(&frame.info[start..])
                    }
                    _ => None,
                };
                out.push(Vdl2Frame { avlc: frame, acars, rs_corrected: burst.rs_corrected });
            }
        }
        out
    }

    pub fn level_dbfs(&self) -> f32 {
        self.demod.level_dbfs()
    }
}

/// Convert a decoded frame into the normalized message model.
pub fn to_message(f: &Vdl2Frame, frequency_hz: u64, level_dbfs: f32, source: Provenance) -> Message {
    let (body, crc_ok, errors) = match &f.acars {
        Some(b) => (
            MessageBody::Acars(b.core.clone()),
            b.crc_ok,
            Some(b.parity_errors),
        ),
        None => (avlc_body(&f.avlc), true, None),
    };
    Message {
        mode: Mode::Vdl2,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality { rssi_db: Some(level_dbfs), ..Default::default() },
        decode: DecodeQuality {
            crc_ok,
            fec_corrected: Some(f.rs_corrected as u32),
            errors,
        },
        body,
        raw: Some(f.avlc.raw.clone()),
        source,
    }
}

/// Structured body for non-ACARS AVLC frames: the link layer is always
/// fully parsed (addresses, control), XID parameters are decoded, and
/// ATN payloads are at least labeled by protocol.
fn avlc_body(frame: &avlc::AvlcFrame) -> MessageBody {
    use avlc::{Control, Payload};
    let kind = match (&frame.control, &frame.payload) {
        (Control::Unnumbered { kind: "XID", .. }, _) => "xid".to_string(),
        (Control::Unnumbered { kind, .. }, _) => format!("avlc-{}", kind.to_lowercase()),
        (Control::Supervisory { kind, .. }, _) => format!("avlc-{}", kind.to_lowercase()),
        (Control::Info { .. }, Payload::Atn { .. }) => "atn".to_string(),
        (Control::Info { .. }, _) => "avlc-i".to_string(),
    };
    let mut details = serde_json::json!({
        "dst": frame.dst,
        "src": frame.src,
        "control": frame.control,
    });
    if let Payload::Atn { ipi } = frame.payload {
        details["protocol"] = serde_json::json!(match ipi {
            0x81 => "CLNP",
            0x82 => "ES-IS",
            _ => "IDRP",
        });
    }
    if matches!(frame.control, Control::Unnumbered { kind: "XID", .. }) {
        if let Some(params) = avlc::parse_xid(&frame.info) {
            details["params"] = serde_json::json!(params);
        }
    }
    if !frame.info.is_empty() {
        let shown = &frame.info[..frame.info.len().min(64)];
        details["info_hex"] =
            serde_json::json!(shown.iter().map(|b| format!("{b:02x}")).collect::<String>());
        details["info_len"] = serde_json::json!(frame.info.len());
    }
    MessageBody::Vdl2 { kind, details }
}
