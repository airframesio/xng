//! Native Iridium decode core (v1: single-channel burst decoder aimed at
//! the fixed ring-alert channel; see PROVENANCE.md and
//! docs/notes/IRIDIUM.md).

pub mod demod;
pub mod frame;
pub mod ira;
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
        let mut out = Vec::new();
        for bits in self.demod.process(channel) {
            if let Some(f) = decode_bits(&bits) {
                out.push(f);
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

/// Convert a decoded frame into the normalized message model.
pub fn to_message(
    f: &ira::IridiumFrame,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Message {
    Message {
        mode: Mode::Iridium,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality { rssi_db: Some(level_dbfs), ..Default::default() },
        decode: DecodeQuality { crc_ok: true, fec_corrected: None, errors: None },
        body: MessageBody::Iridium { kind: f.kind.to_string(), details: f.details.clone() },
        raw: None,
        source,
    }
}
