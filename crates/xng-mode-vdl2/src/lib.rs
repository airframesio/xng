//! Native VDL Mode 2 decode core.
//!
//! Pipeline per channel: wideband IQ → [`xng_dsp::Ddc`] → 50 kHz channel
//! IQ → [`demod::Vdl2Demod`] (D8PSK burst acquisition, header,
//! deinterleave + RS(255,249)) → [`avlc`] frame scan → ACARS-over-AVLC via
//! [`xng_acars::block`] → [`xng_types::Message`].
//!
//! See PROVENANCE.md for clean-room sourcing (no GPL decoder code used).

pub mod atn;
pub mod atn_cpdlc;
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
/// Preferred channel rate (~9.5 samples/symbol): measurably better
/// off-air decode than the 50 kS/s floor; used whenever the capture
/// rate divides into it.
pub const CHANNEL_RATE_HI: f64 = 100_000.0;
/// One-sided passband: D8PSK 10.5 kBd, RC α=0.6 → ±8.4 kHz.
pub const CHANNEL_PASSBAND_HZ: f64 = 8_500.0;

/// One decoded AVLC frame plus its ACARS content when present.
pub struct Vdl2Frame {
    pub avlc: avlc::AvlcFrame,
    pub acars: Option<xng_acars::block::AcarsBlock>,
    /// Decoded ATN transport (X.25 packet, CLNP/COTP) for I-frames.
    pub atn: Option<serde_json::Value>,
    pub rs_corrected: usize,
}

pub struct Vdl2ChannelDecoder {
    ddc: Option<Ddc>,
    demod: demod::Vdl2Demod,
    rs: ReedSolomon,
    channel_buf: Vec<Complex<f32>>,
    x25: atn::X25Reassembler,
    samples_seen: u64,
    input_rate: f64,
}

impl Vdl2ChannelDecoder {
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        // Prefer the high channel rate when the capture divides into it;
        // 50 kS/s remains the floor (and the vendored-fixture rate).
        let channel_rate = if input_rate >= CHANNEL_RATE_HI
            && (input_rate / CHANNEL_RATE_HI).fract().abs() < 1e-9
        {
            CHANNEL_RATE_HI
        } else {
            CHANNEL_RATE
        };
        let ddc = if (input_rate - channel_rate).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, channel_rate, freq_offset_hz, CHANNEL_PASSBAND_HZ)?)
        };
        Ok(Self {
            ddc,
            demod: demod::Vdl2Demod::new(channel_rate),
            rs: interleave::vdl2_rs(),
            channel_buf: Vec::new(),
            x25: atn::X25Reassembler::new(),
            samples_seen: 0,
            input_rate,
        })
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<Vdl2Frame> {
        self.samples_seen += input.len() as u64;
        let now = self.samples_seen as f64 / self.input_rate;
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
                // ATN transport rides in I-frame info fields: an ISO 8208
                // packet (most links) or bare CLNP/ES-IS/IDRP.
                let atn = if acars.is_none()
                    && matches!(frame.control, avlc::Control::Info { .. })
                {
                    decode_atn(&frame.info, &mut self.x25, now)
                } else {
                    None
                };
                out.push(Vdl2Frame { avlc: frame, acars, atn, rs_corrected: burst.rs_corrected });
            }
        }
        out
    }

    pub fn level_dbfs(&self) -> f32 {
        self.demod.level_dbfs()
    }
}

/// Decode an I-frame information field as ATN transport.
fn decode_atn(
    info: &[u8],
    x25: &mut atn::X25Reassembler,
    now: f64,
) -> Option<serde_json::Value> {
    if let Some(pkt) = atn::parse_x25(info) {
        let mut v = serde_json::to_value(&pkt).unwrap_or_default();
        v["layer"] = serde_json::json!("x25");
        if pkt.kind == "data" {
            if let Some(full) = x25.push(&pkt, now) {
                if let Some(net) = atn::parse_network(&full) {
                    v["network"] = net;
                }
            } else {
                v["reassembling"] = serde_json::json!(true);
            }
        } else if !pkt.payload.is_empty() {
            // Call user data names the network protocol.
            if let Some(net) = atn::parse_network(&pkt.payload) {
                v["network"] = net;
            }
        }
        return Some(v);
    }
    atn::parse_network(info)
}

/// Convert a decoded frame into the normalized message model.
pub fn to_message(f: &Vdl2Frame, frequency_hz: u64, level_dbfs: f32, source: Provenance) -> Message {
    let (body, crc_ok, errors) = match &f.acars {
        Some(b) => (
            MessageBody::Acars(b.core.clone()),
            b.crc_ok,
            Some(b.parity_errors),
        ),
        None => (avlc_body(&f.avlc, f.atn.as_ref()), true, None),
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
fn avlc_body(frame: &avlc::AvlcFrame, atn: Option<&serde_json::Value>) -> MessageBody {
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
    if let Some(a) = atn {
        details["atn"] = a.clone();
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
