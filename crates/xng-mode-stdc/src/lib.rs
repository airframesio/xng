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

/// Minimum extra UW symbols a mid-frame polarity-flip correction must buy
/// over the best whole-frame polarity score to be trusted. Large enough
/// that noise alone cannot fabricate a flip: a real Costas 180° slip
/// roughly halves the whole-frame UW score (the two runs cancel), so a
/// genuine correction recovers tens of UW symbols.
pub const MID_FRAME_FLIP_MIN_GAIN: u32 = 24;

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
        Self::with_matched_filter(input_rate, freq_offset_hz, true)
    }

    /// Construct with the demod RRC matched filter explicitly toggled.
    /// `matched_filter = false` is for the BER oracle test that quantifies
    /// the matched-filter gain; production always uses `new` (filter on).
    pub fn with_matched_filter(
        input_rate: f64,
        freq_offset_hz: f64,
        matched_filter: bool,
    ) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, CHANNEL_RATE, freq_offset_hz, CHANNEL_PASSBAND_HZ)?)
        };
        Ok(Self {
            ddc,
            demod: demod::BpskDemod::with_matched_filter(CHANNEL_RATE, matched_filter),
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
                let invert = inverted > normal;
                let (bytes, mut stats) = self
                    .decoder
                    .decode_with_stats(&self.syms[..frame::FRAME_SYMBOLS], invert);
                // Per-frame UW BER for the matched polarity.
                let uw_matches = if invert { inverted } else { normal };
                stats.uw_ber_ppt = frame::uw_ber_ppt(uw_matches);
                for mut pkt in self.parser.parse_frame(&bytes) {
                    pkt.fec_corrected = Some(stats.fec_corrected);
                    pkt.uw_ber_ppt = Some(stats.uw_ber_ppt);
                    out.push(pkt);
                }
                self.syms.drain(..frame::FRAME_SYMBOLS);
                self.demod.locked = true;
                self.since_lock = 0;
            } else if let Some(flip) = frame::detect_polarity_flip(&hard, MID_FRAME_FLIP_MIN_GAIN)
                .filter(|f| f.uw_score >= frame::UW_MIN_MATCH)
            {
                // Mid-frame Costas 180° slip: correct the odd-polarity run
                // in place, then decode the now-consistent frame. Recovers
                // frames a whole-frame polarity test would discard.
                let mut soft: Vec<f32> = self.syms[..frame::FRAME_SYMBOLS].to_vec();
                frame::apply_polarity_flip(&mut soft, &flip);
                let (bytes, mut stats) = self.decoder.decode_with_stats(&soft, false);
                stats.uw_ber_ppt = frame::uw_ber_ppt(flip.uw_score);
                for mut pkt in self.parser.parse_frame(&bytes) {
                    pkt.fec_corrected = Some(stats.fec_corrected);
                    pkt.uw_ber_ppt = Some(stats.uw_ber_ppt);
                    out.push(pkt);
                }
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
    // Carry the per-frame unique-word BER into details (the normalized
    // model has no dedicated field for it); fec corrections map onto the
    // standard DecodeQuality field.
    let mut details = p.details.clone();
    if let (Some(obj), Some(ber)) = (details.as_object_mut(), p.uw_ber_ppt) {
        obj.entry("uw_ber_ppt").or_insert_with(|| serde_json::json!(ber));
    }
    Message {
        mode: Mode::StdC,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality { rssi_db: Some(level_dbfs), ..Default::default() },
        decode: DecodeQuality {
            crc_ok: p.checksum_ok,
            fec_corrected: p.fec_corrected,
            errors: None,
        },
        body: MessageBody::StdC {
            name: p.name.to_owned(),
            text: p.text.clone(),
            details,
        },
        raw: Some(p.raw.clone()),
        source,
    }
}
