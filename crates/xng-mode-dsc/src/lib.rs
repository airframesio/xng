//! Native Digital Selective Calling (DSC) decode core for xng.
//!
//! DSC (ITU-R M.493 / M.541, built on the CCIR 493 alphabet) is the calling
//! and distress-alerting layer of the GMDSS, carried by FSK on MF/HF
//! (170 Hz shift, 100 Bd) and VHF (1300/2100 Hz, 1200 Bd) channels.
//!
//! Pipeline:
//!
//! 1. **Symbol level** ([`symbol`]) — the FSK bit stream is sliced into 10-bit
//!    CCIR 493 symbols (7 information bits + a 3-bit count of the zero
//!    information bits, giving each symbol its own integrity check), and the
//!    DX/RX time-diversity streams are de-interleaved into one symbol
//!    sequence, recovering symbols erased in one stream from the other.
//! 2. **Message level** ([`message`]) — the symbol sequence is parsed by
//!    format specifier into a structured [`message::DscMessage`]: addressed
//!    and self-identification MMSIs, category, telecommands, distress
//!    nature/position/time, frequency or working channel, end-of-sequence,
//!    and the recomputed error-check character (ECC) status. The message
//!    serializes to JSON via [`message::DscMessage::to_json`].
//!
//! The bit→symbol→message layers are pinned to an external reference decoder's
//! published vectors (see PROVENANCE.md). The IQ→bits front end ([`demod`]) is
//! the MF/HF FSK demodulator (100 Bd binary FSK, ±85 Hz shift) reusing the
//! frequency-discriminator + timing-recovery pattern from the ACARS/AIS demods,
//! validated SYNTHETICally (modulate a known symbol stream → demod → assert the
//! same message decode; see `tests/demod_synth.rs` and PROVENANCE.md).

pub mod demod;
pub mod message;
pub mod modulate;
pub mod symbol;

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

pub use message::{
    decode, Category, DscMessage, EndOfSequence, FirstCommand, Format, NatureOfDistress,
    SecondCommand,
};
pub use symbol::{decode_bitstream, decode_symbol, deinterleave_dx_rx, ERASURE};

/// Internal demod sample rate: 80 samples per bit at 100 Bd. An integer
/// multiple of the symbol rate so the timing loop wraps cleanly on bit
/// boundaries, and comfortably oversamples the ±85 Hz tones.
pub const CHANNEL_RATE: f64 = 8_000.0;
/// One-sided DDC passband. The ±85 Hz FSK shift plus the 100 Bd main lobe sit
/// well inside this; the DDC's anti-alias filter rejects everything else in the
/// (typically USB-audio) channel.
pub const CHANNEL_PASSBAND_HZ: f64 = 500.0;

/// Decodes a full bit stream (10 bits/symbol) into a [`DscMessage`], applying
/// DX/RX time-diversity de-interleaving with the standard geometry (6 leading
/// DX phasing characters; RX repeat trailing by 2). This is the convenience
/// path once a demod has produced a synchronised bit stream.
pub fn decode_from_bits(bits: &[u8]) -> DscMessage {
    let chars = symbol::decode_bitstream(bits);
    let symbols = symbol::deinterleave_dx_rx(&chars, 6, 2);
    message::decode(&symbols)
}

/// Decodes one DSC MF/HF channel out of a (typically narrowband) IQ capture.
///
/// Owns an internal [`Ddc`] that mixes by `freq_offset_hz` and resamples the
/// capture-rate IQ down to [`CHANNEL_RATE`], then runs the FSK discriminator
/// ([`demod::FskDemod`]), hunts the DSC phasing sequence
/// ([`demod::DscBitSync`]), and feeds the aligned bit stream to
/// [`decode_from_bits`].
pub struct DscChannelDecoder {
    ddc: Option<Ddc>,
    demod: demod::FskDemod,
    channel_buf: Vec<Complex<f32>>,
    /// Rolling demodulated bit stream awaiting phasing acquisition.
    bits: Vec<u8>,
    /// Bit offset already searched for phasing (so re-scans are cheap).
    scanned: usize,
}

/// Number of bits a complete DSC sequence can span before we give up and slide
/// the window. A distress alert is ~20 symbols of data plus 6+ phasing chars,
/// all sent in DX/RX pairs: well under ~120 characters → ~2400 bits. We keep a
/// generous window so a whole call (with diversity) is always present.
const MAX_BITS_WINDOW: usize = 4096;

/// Bits that must follow a found phasing character before a decode is even
/// attempted: enough for the shortest complete call (a distress alert is 6
/// phasing + ~18 data DX characters, each paired with an RX character →
/// ~48 characters → 480 bits). Completeness past this is judged by the decoded
/// message itself (see [`frame_is_complete`]), which handles the variable call
/// lengths without over-waiting on a short frame.
const MIN_FRAME_BITS: usize = 460;

impl DscChannelDecoder {
    /// `input_rate` is any capture rate ≥ [`CHANNEL_RATE`]; a non-integer
    /// multiple is resampled by the DDC. `freq_offset_hz` is the channel center
    /// relative to the capture center.
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, CHANNEL_RATE, freq_offset_hz, CHANNEL_PASSBAND_HZ)?)
        };
        Ok(Self {
            ddc,
            demod: demod::FskDemod::new(),
            channel_buf: Vec::new(),
            bits: Vec::new(),
            scanned: 0,
        })
    }

    /// Feed capture IQ; returns any DSC messages that became decodable.
    ///
    /// On finding a phasing sequence, the aligned bit window is decoded and the
    /// consumed bits are dropped, so each call is reported once.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<DscMessage> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };
        self.demod.process(channel, &mut self.bits);

        let mut out = Vec::new();
        // Hunt for a phasing sequence in the accumulated bits. Once a full
        // call's worth of bits has arrived after it, decode and advance past it.
        while let Some(off) = demod::DscBitSync::find_phasing(&self.bits[self.scanned..]) {
            let start = self.scanned + off;
            // Need at least a short call's worth of bits before trying at all.
            if self.bits.len() - start < MIN_FRAME_BITS {
                break;
            }
            let msg = decode_from_bits(&self.bits[start..]);
            if msg.format == Format::Unknown {
                // A chance 125 at this phase, not a real call: step past it.
                self.scanned = start + symbol::SYMBOL_BITS;
                continue;
            }
            if !frame_is_complete(&msg) {
                // Real format but the tail (EOS/ECC) has not arrived yet; wait
                // for more bits rather than reporting a truncated decode.
                break;
            }
            out.push(msg);
            // Advance past the whole consumed call so the next hunt looks ahead;
            // keep enough tail to catch a following call.
            self.scanned = start + symbol::SYMBOL_BITS;
            if self.scanned >= self.bits.len() {
                break;
            }
        }

        // Bound memory: keep only the unscanned tail (plus a small overlap so a
        // phasing sequence straddling the trim point survives).
        if self.bits.len() > MAX_BITS_WINDOW {
            let drop = self.scanned.min(self.bits.len());
            self.bits.drain(..drop);
            self.scanned = 0;
        }
        out
    }

    /// Smoothed channel power level in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        self.demod.level_dbfs()
    }
}

/// Whether a decoded call has its trailing fields (end-of-sequence and the
/// error-check character), i.e. the whole sequence has been demodulated. Used
/// while streaming to avoid reporting a frame whose tail has not arrived yet.
/// A recognised EOS plus a present ECC is the M.493 end marker.
fn frame_is_complete(msg: &DscMessage) -> bool {
    msg.eos != EndOfSequence::Unknown && msg.ecc != ERASURE
}

/// Convert a decoded DSC message into the normalized message model.
///
/// `kind` is the call format (e.g. `distress_alert`, `individual_station_call`),
/// matching the `Format` serde tag; `details` is the full `DscMessage` JSON.
pub fn to_message(
    f: &DscMessage,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Message {
    let kind = match f.format {
        Format::DistressAlert => "distress_alert",
        Format::AllShipsCall => "all_ships_call",
        Format::GroupCall => "group_call",
        Format::IndividualStationCall => "individual_station_call",
        Format::GeographicAreaGroupCall => "geographic_area_group_call",
        Format::AutomaticServiceCall => "automatic_service_call",
        Format::Unknown => "unknown",
    }
    .to_string();
    let crc_ok = f.status == "OK";
    let raw: Vec<u8> = f.symbols.iter().map(|&s| s.clamp(0, 255) as u8).collect();
    Message {
        mode: Mode::Dsc,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality { rssi_db: Some(level_dbfs), ..Default::default() },
        decode: DecodeQuality { crc_ok, fec_corrected: None, errors: None },
        body: MessageBody::Dsc {
            kind,
            details: serde_json::to_value(f).expect("DscMessage serializes"),
        },
        raw: Some(raw),
        source,
    }
}
