//! Native Iridium decode core (v1: single-channel burst decoder aimed at
//! the fixed ring-alert channel; see PROVENANCE.md and
//! docs/notes/IRIDIUM.md).

pub mod demod;
pub mod frame;
pub mod iip;
pub mod ira;
pub mod ms;
pub mod sbd;
pub mod voice;
pub mod wideband;
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
    pager: ms::PagerReassembler,
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
            pager: ms::PagerReassembler::new(),
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
        for burst in self.demod.process(channel) {
            handle_bits(&burst.bits, time, &mut self.sbd, &mut self.pager, &mut out);
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
        frame::FrameKind::Ms => {
            // After the 32-bit messaging header: 64-bit chunks, each a
            // 2-way symbol interleave of two BCH blocks (toolkit MS path).
            let mut blocks = Vec::new();
            for chunk in data[32..].chunks_exact(64) {
                let (o, e) = frame::de_interleave2(chunk);
                blocks.push(o);
                blocks.push(e);
            }
            frame::strip_fill(&mut blocks);
            let (payload, _fixed) = frame::ecc_blocks(&blocks, frame::MESSAGING_BCH_POLY);
            let blocks21: Vec<Vec<u8>> =
                payload.chunks_exact(21).map(|c| c.to_vec()).collect();
            let f = ms::parse(&blocks21)?;
            Some(ira::IridiumFrame {
                kind: "msg",
                details: serde_json::to_value(&f).unwrap_or_default(),
                acars: None,
                raw_bits: bits.to_vec(),
            })
        }
        _ => None,
    }
}

/// Decode one demodulated burst's bit stream into frames, feeding DA
/// fragments through the SBD reassembler (shared by the single-channel
/// and wideband decoders).
fn handle_bits(
    bits: &[u8],
    time: f64,
    sbd: &mut sbd::SbdReassembler,
    pager: &mut ms::PagerReassembler,
    out: &mut Vec<ira::IridiumFrame>,
) {
    if let Some(f) = decode_bits(bits) {
        // Multi-part pages: emit the assembled text when complete.
        if f.kind == "msg" {
            let body = serde_json::from_value::<ms::MsFrame>(f.details.clone())
                .ok()
                .and_then(|m| m.body);
            if let Some(b) = body {
                if let Some(full) = pager.push(&b, time) {
                    out.push(ira::IridiumFrame {
                        kind: "msg-complete",
                        details: serde_json::json!({ "ric": b.ric, "text": full }),
                        acars: None,
                        raw_bits: Vec::new(),
                    });
                }
            }
        }
        out.push(f);
        return;
    }
    if let Some(f) = lcw_traffic_frame(bits) {
        out.push(f);
        return;
    }
    // LCW-bearing duplex frames: DA (SBD data) → reassembly → SBD
    // transport → ACARS.
    if let Some((da, raw)) = decode_da_bits(bits) {
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
        if let Some(msg) = sbd.push(&da, time) {
            out.push(ira::IridiumFrame {
                kind: "sbd",
                details: msg.details.clone(),
                acars: msg.acars,
                raw_bits: Vec::new(),
            });
        }
    }
}

/// Wideband decoder: hunts bursts across the whole capture (no channel
/// list needed) and decodes them through the same frame/SBD chain.
/// Returns each frame with the burst's frequency offset from the
/// capture center.
pub struct IridiumWidebandDecoder {
    wb: wideband::IridiumWideband,
    sbd: sbd::SbdReassembler,
    pager: ms::PagerReassembler,
    samples_seen: u64,
    input_rate: f64,
    level: f32,
}

impl IridiumWidebandDecoder {
    /// `input_rate` must be an integer multiple of 250 kHz.
    pub fn new(input_rate: f64) -> Result<Self, String> {
        Ok(Self {
            wb: wideband::IridiumWideband::new(input_rate)?,
            sbd: sbd::SbdReassembler::new(),
            pager: ms::PagerReassembler::new(),
            samples_seen: 0,
            input_rate,
            level: 0.0,
        })
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<(f64, ira::IridiumFrame)> {
        for x in input {
            self.level += 1e-5 * (x.norm_sqr() - self.level);
        }
        self.samples_seen += input.len() as u64;
        let time = self.samples_seen as f64 / self.input_rate;
        let mut out = Vec::new();
        for burst in self.wb.process(input) {
            let mut frames = Vec::new();
            handle_bits(&burst.bits, time, &mut self.sbd, &mut self.pager, &mut frames);
            out.extend(frames.into_iter().map(|f| (burst.offset_hz, f)));
        }
        out
    }

    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }
}

/// Decode an LCW-bearing burst's DA frame, if it is one (ft == 2).
pub fn decode_da_bits(bits: &[u8]) -> Option<(frame::DaFrame, Vec<u8>)> {
    match decode_lcw_bits(bits)? {
        (2, data) => {
            let da = frame::decode_da(&data[46..])?;
            Some((da, bits.to_vec()))
        }
        _ => None,
    }
}

/// Classify an LCW-bearing duplex burst: returns the LCW frame type and
/// the post-access-code data bits.
fn decode_lcw_bits(bits: &[u8]) -> Option<(u8, &[u8])> {
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
    Some((ft, data))
}

/// Tag the non-DA duplex traffic classes by LCW frame type: voice (AMBE
/// payload bytes extracted for external decoding — the codec itself is
/// proprietary), IP data, and sync bursts (iridium-toolkit ft mapping).
fn lcw_traffic_frame(bits: &[u8]) -> Option<ira::IridiumFrame> {
    let (ft, data) = decode_lcw_bits(bits)?;
    let kind = match ft {
        0 => "voice",
        1 => "ip-data",
        7 => "sync",
        _ => return None,
    };
    let payload = &data[46..];
    let payload_hex: String = payload
        .chunks(8)
        .map(|c| {
            format!("{:02x}", c.iter().fold(0u8, |v, &b| (v << 1) | b))
        })
        .collect();
    let mut details =
        serde_json::json!({ "payload_hex": payload_hex, "payload_bits": payload.len() });
    if ft == 0 {
        // Voice channel: run the VDA/VO6/VOD/VOZ/VOC classification
        // ladder and fold its result into the details.
        if let Some(serde_json::Value::Object(extra)) = voice::classify_voice(payload) {
            details.as_object_mut().unwrap().extend(extra);
        }
    } else if ft == 1 {
        // IP channel: IIP/IIQ/IIR frame classification.
        if let Some(serde_json::Value::Object(extra)) = iip::parse_ip_payload(payload) {
            details.as_object_mut().unwrap().extend(extra);
        }
    }
    Some(ira::IridiumFrame {
        kind,
        details,
        acars: None,
        raw_bits: bits.to_vec(),
    })
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
