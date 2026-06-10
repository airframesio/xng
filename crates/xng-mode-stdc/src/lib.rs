//! Native Inmarsat STD-C / EGC decode core (NCS common channel).
//!
//! Pipeline: wideband IQ → [`xng_dsp::Ddc`] → 12 kHz channel IQ →
//! [`demod::BpskDemod`] (coherent: coarse AFC, Costas, Gardner) →
//! [`frame`] (UW sync both polarities, depermute, deinterleave, Viterbi,
//! descramble) → [`packet::PacketParser`] (checksums, EGC/LCN assembly)
//! → [`xng_types::Message`].
//!
//! Constants and layouts per docs/notes/STDC.md (facts cross-verified
//! across GPL references and re-derived; see PROVENANCE.md).

pub mod demod;
pub mod frame;
pub mod modulate;
pub mod packet;

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

pub const CHANNEL_RATE: f64 = 12_000.0;
/// One-sided passband (signal ≈ ±1 kHz).
pub const CHANNEL_PASSBAND_HZ: f64 = 2_000.0;

pub struct StdcChannelDecoder {
    ddc: Option<Ddc>,
    demod: demod::BpskDemod,
    decoder: frame::FrameDecoder,
    parser: packet::PacketParser,
    channel_buf: Vec<Complex<f32>>,
    syms: Vec<f32>,
    /// Frames since last UW lock (drives re-acquisition).
    since_lock: u32,
}

impl StdcChannelDecoder {
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, CHANNEL_RATE, freq_offset_hz, CHANNEL_PASSBAND_HZ)?)
        };
        Ok(Self {
            ddc,
            demod: demod::BpskDemod::new(CHANNEL_RATE),
            decoder: frame::FrameDecoder::new(),
            parser: packet::PacketParser::new(),
            channel_buf: Vec::new(),
            syms: Vec::new(),
            since_lock: 0,
        })
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<packet::StdcPacket> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };
        let before = self.syms.len();
        self.demod.process(channel, &mut self.syms);
        let _ = before;

        let mut out = Vec::new();
        loop {
            if self.syms.len() < frame::FRAME_SYMBOLS {
                break;
            }
            let hard: Vec<u8> = self.syms[..frame::FRAME_SYMBOLS]
                .iter()
                .map(|&s| (s > 0.0) as u8)
                .collect();
            let (normal, inverted) = frame::uw_score(&hard);
            if normal >= frame::UW_MIN_MATCH || inverted >= frame::UW_MIN_MATCH {
                let bytes = self
                    .decoder
                    .decode(&self.syms[..frame::FRAME_SYMBOLS], inverted > normal);
                out.extend(self.parser.parse_frame(&bytes));
                self.syms.drain(..frame::FRAME_SYMBOLS);
                self.demod.locked = true;
                self.since_lock = 0;
            } else {
                self.syms.remove(0);
                self.since_lock += 1;
                if self.since_lock > 2 * frame::FRAME_SYMBOLS as u32 {
                    self.demod.locked = false; // re-run coarse acquisition
                }
            }
        }
        out
    }

    pub fn level_dbfs(&self) -> f32 {
        self.demod.level_dbfs()
    }
}

/// Convert a decoded packet into the normalized message model.
pub fn to_message(
    p: &packet::StdcPacket,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Message {
    Message {
        mode: Mode::StdC,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality { rssi_db: Some(level_dbfs), ..Default::default() },
        decode: DecodeQuality { crc_ok: p.checksum_ok, fec_corrected: None, errors: None },
        body: MessageBody::StdC {
            name: p.name.to_owned(),
            text: p.text.clone(),
            details: p.details.clone(),
        },
        raw: Some(p.raw.clone()),
        source,
    }
}
