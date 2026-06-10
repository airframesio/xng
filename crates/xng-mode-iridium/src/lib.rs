//! Native Iridium decode core (v1: single-channel burst decoder aimed at
//! the fixed ring-alert channel; see PROVENANCE.md and
//! docs/notes/IRIDIUM.md).

pub mod demod;
pub mod frame;
pub mod ira;
pub mod sbd;
pub mod modulate;

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Channel rate (10 samples/symbol at 25 000 sym/s, as gr-iridium).
pub const CHANNEL_RATE: f64 = 250_000.0;
/// One-sided passband (one 40 kHz Iridium channel).
pub const CHANNEL_PASSBAND_HZ: f64 = 25_000.0;
/// The simplex ring-alert channel.
pub const RING_ALERT_HZ: u64 = 1_626_270_833;

pub struct IridiumChannelDecoder {
    ddc: Option<Ddc>,
    demod: demod::IridiumDemod,
    channel_buf: Vec<Complex<f32>>,
    sbd: sbd::SbdReassembler,
    samples_seen: u64,
}

impl IridiumChannelDecoder {
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, CHANNEL_RATE, freq_offset_hz, CHANNEL_PASSBAND_HZ)?)
        };
        Ok(Self {
            ddc,
            demod: demod::IridiumDemod::new(CHANNEL_RATE),
            channel_buf: Vec::new(),
            sbd: sbd::SbdReassembler::new(),
            samples_seen: 0,
        })
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<ira::IridiumFrame> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };
        self.samples_seen += channel.len() as u64;
        let time = self.samples_seen as f64 / CHANNEL_RATE;
        let mut out = Vec::new();
        for bits in self.demod.process(channel) {
            if let Some(f) = decode_bits(&bits) {
                out.push(f);
                continue;
            }
            // LCW-bearing duplex frames: DA (SBD data) → reassembly →
            // SBD transport → ACARS.
            if let Some((da, raw)) = decode_da_bits(&bits) {
                out.push(ira::IridiumFrame {
                    kind: "ida",
                    details: serde_json::json!({
                        "cont": da.continuation,
                        "ctr": da.ctr,
                        "len": da.len,
                        "crc_ok": da.crc_ok,
                        "data_hex": da.data[..da.len.min(20) as usize]
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>(),
                    }),
                    acars: None,
                    raw_bits: raw,
                });
                if let Some(msg) = self.sbd.push(&da, time) {
                    out.push(ira::IridiumFrame {
                        kind: "sbd",
                        details: msg.details.clone(),
                        acars: msg.acars,
                        raw_bits: Vec::new(),
                    });
                }
            }
        }
        out
    }

    pub fn level_dbfs(&self) -> f32 {
        self.demod.level_dbfs()
    }
}

/// Decode a demodulated burst bit stream (starting at the access code).
pub fn decode_bits(bits: &[u8]) -> Option<ira::IridiumFrame> {
    let data = if bits.len() > 24 && bits[..24] == frame::ACCESS_DL[..] {
        &bits[24..]
    } else if bits.len() > 24 && bits[..24] == frame::ACCESS_UL[..] {
        &bits[24..]
    } else {
        return None;
    };
    match frame::classify(data) {
        frame::FrameKind::Ra => {
            let mut blocks = frame::ra_blocks(data);
            frame::strip_fill(&mut blocks);
            let (payload, fixed) = frame::ecc_blocks(&blocks, frame::RINGALERT_BCH_POLY);
            ira::parse_ra(&payload, fixed, bits)
        }
        frame::FrameKind::Bc => {
            // BCH(7,3) header: first 3 bits are the broadcast type.
            let bc_type = data[..3].iter().fold(0u32, |v, &b| (v << 1) | b as u32);
            let mut blocks = Vec::new();
            for chunk in data[6..].chunks_exact(64) {
                let (o, e) = frame::de_interleave2(chunk);
                blocks.push(o);
                blocks.push(e);
            }
            let (payload, fixed) = frame::ecc_blocks(&blocks, frame::RINGALERT_BCH_POLY);
            if payload.is_empty() {
                return None;
            }
            Some(ira::parse_bc(bc_type, &payload, fixed, bits))
        }
        _ => None,
    }
}

/// Decode an LCW-bearing burst's DA frame, if it is one (ft == 2).
pub fn decode_da_bits(bits: &[u8]) -> Option<(frame::DaFrame, Vec<u8>)> {
    let data = if bits.len() > 24 && bits[..24] == frame::ACCESS_DL[..] {
        &bits[24..]
    } else if bits.len() > 24 && bits[..24] == frame::ACCESS_UL[..] {
        &bits[24..]
    } else {
        return None;
    };
    if frame::classify(data) != frame::FrameKind::Lw {
        return None;
    }
    let (ft, _, _, _) = frame::decode_lcw(data)?;
    if ft != 2 {
        return None;
    }
    let da = frame::decode_da(&data[46..])?;
    Some((da, bits.to_vec()))
}

/// Convert a decoded frame into the normalized message model.
pub fn to_message(
    f: &ira::IridiumFrame,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Message {
    // SBD-carried ACARS surfaces as a first-class ACARS message (like
    // the HFDL/Aero carriers), so downstream consumers treat it as
    // ACARS traffic.
    let (body, crc_ok, errors) = match &f.acars {
        Some(b) => (
            MessageBody::Acars(b.core.clone()),
            b.crc_ok,
            Some(b.parity_errors),
        ),
        None => (
            MessageBody::Iridium { kind: f.kind.to_string(), details: f.details.clone() },
            true,
            None,
        ),
    };
    Message {
        mode: Mode::Iridium,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality { rssi_db: Some(level_dbfs), ..Default::default() },
        decode: DecodeQuality { crc_ok, fec_corrected: None, errors },
        body,
        raw: None,
        source,
    }
}
