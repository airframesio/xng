//! Native NAVTEX (SITOR-B / CCIR 476) decode core for xng.
//!
//! NAVTEX is the international maritime safety-information broadcast on
//! 518 kHz (English), 490 kHz and 4209.5 kHz. On air it is 100-baud
//! narrow-shift (±85 Hz) FSK carrying the CCIR 476 seven-bit
//! constant-ratio code in collective B-mode (FEC-B): every character is
//! sent twice with time diversity, so a receiver that loses one copy can
//! still recover the other.
//!
//! This crate implements the **message/frame decode layer** — the part
//! that turns a demodulated CCIR 476 symbol stream into a structured
//! message — with every protocol fact anchored to an external reference
//! (see PROVENANCE.md). The layers, bottom-up:
//!
//! - [`ccir476`] — the 4-of-7 constant-ratio alphabet (LTRS/FIGS shift),
//!   bit packing, and the constant-ratio parity check.
//! - [`fec`] — FEC-B time-diversity recovery (DX copy preferred, RX
//!   fallback five characters earlier) and phasing sync.
//! - [`message`] — `ZCZC B1B2B3B4` header parsing, text body, `NNNN`
//!   end, and JSON emission.
//!
//! End-to-end: [`decode_symbols`] takes an interleaved DX/RX symbol stream
//! and returns a [`message::NavtexMessage`].
//!
//! # IQ front end
//!
//! [`NavtexChannelDecoder`] is the channelized IQ entry point, mirroring the
//! AIS template: it owns an [`xng_dsp::Ddc`] that mixes a wideband capture by
//! `freq_offset_hz` and decimates to [`CHANNEL_RATE`], then runs the
//! narrow-shift FSK [`demod::FskDemod`] (frequency discriminator + 100 Bd
//! timing recovery) to recover the CCIR 476 bit stream, packs it into 7-bit
//! codes, and feeds the verified [`decode_symbols`] core. [`to_message`]
//! normalizes a decoded message into the [`xng_types`] bus form.
//!
//! The DECODE core (tables, FEC-B, framing) stays oracle-anchored by its own
//! tests; the modulate→demod path used to validate the front end is
//! self-generated (see PROVENANCE.md and the `*_synth_iq` tests).

pub mod ccir476;
pub mod demod;
pub mod fec;
pub mod message;
pub mod modulate;

pub use message::NavtexMessage;

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::{Ddc, IqSample};
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Decode an interleaved DX/RX CCIR 476 symbol stream into a structured
/// NAVTEX message.
///
/// Each element of `symbols` is one 7-bit CCIR 476 code (use
/// [`ccir476::pack_bits`] to build them from bit decisions). The stream is
/// phase-located via [`fec::find_phase`]; if `first_dx` is `Some`, that
/// offset is used instead (e.g. when the caller already knows the phase).
///
/// Returns `None` if the stream is too short to phase-lock.
pub fn decode_symbols(symbols: &[u8], first_dx: Option<usize>) -> Option<NavtexMessage> {
    let off = match first_dx {
        Some(o) => o,
        None => fec::find_phase(symbols)?,
    };
    let recovered = fec::recover_stream(symbols, off);
    let text = fec::codes_to_text(&recovered, /* drop_lost = */ true);
    Some(message::parse(&text))
}

/// Frame parameters for the on-air NAVTEX signal (informational; used by a
/// future IQ front end).
pub mod params {
    /// Symbol/baud rate (CCIR 476 B-mode).
    pub const BAUD: f64 = 100.0;
    /// FSK frequency shift from center to each tone, Hz.
    pub const SHIFT_HZ: f64 = 85.0;
    /// Bits per CCIR 476 symbol.
    pub const BITS_PER_SYMBOL: usize = 7;
    /// International NAVTEX frequency (English), Hz.
    pub const FREQ_518K: u64 = 518_000;
    /// National/local NAVTEX frequency, Hz.
    pub const FREQ_490K: u64 = 490_000;
    /// Tropical/HF NAVTEX frequency, Hz.
    pub const FREQ_4209K5: u64 = 4_209_500;
}

/// Internal demod sample rate: 48 samples per bit at 100 Bd. A clean
/// integer multiple of the baud rate that resolves the ±85 Hz FSK swing
/// with margin for the timing loop.
pub const CHANNEL_RATE: f64 = 4_800.0;
/// One-sided DDC passband. Comfortably passes both ±85 Hz FSK tones plus a
/// realistic carrier tuning offset, while rejecting the adjacent NAVTEX
/// channel (518 / 490 kHz are 28 kHz apart on MF, far outside this).
pub const CHANNEL_PASSBAND_HZ: f64 = 250.0;

/// IQ→symbol front end: convert channelized NAVTEX IQ to a CCIR 476 symbol
/// stream via the narrow-shift FSK demod, then pack to 7-bit codes.
///
/// `sample_rate` must equal [`CHANNEL_RATE`]; the wideband case goes through
/// [`NavtexChannelDecoder`], which owns the DDC. The returned codes are the
/// interleaved DX/RX symbol stream for [`decode_symbols`]. `bit_phase`
/// selects the 7-bit packing alignment (0..7).
pub fn demod_fsk(iq: &[IqSample], sample_rate: f64, bit_phase: usize) -> Vec<u8> {
    assert!(
        (sample_rate - CHANNEL_RATE).abs() < 1e-6,
        "demod_fsk expects channel-rate IQ ({CHANNEL_RATE} S/s)"
    );
    let mut demod = demod::FskDemod::new();
    let mut bits = Vec::new();
    demod.process(iq, &mut bits);
    demod::pack_codes(&bits, bit_phase)
}

/// One fully decoded NAVTEX frame: the structured message plus the wire
/// symbols it was recovered from and the phase that locked.
#[derive(Debug, Clone)]
pub struct NavtexFrame {
    /// Parsed message (header fields, body text, end marker).
    pub message: NavtexMessage,
    /// The recovered DX/RX symbol stream (one 7-bit CCIR 476 code each).
    pub symbols: Vec<u8>,
}

/// Decodes one NAVTEX channel out of a wideband capture.
///
/// Mirrors the AIS [`xng_mode_ais::AisChannelDecoder`] contract: owns an
/// internal [`Ddc`] that mixes by `freq_offset_hz` and decimates the capture
/// to [`CHANNEL_RATE`], runs the FSK demod, and emits a [`NavtexFrame`] when
/// a complete `ZCZC … NNNN` message is recovered.
pub struct NavtexChannelDecoder {
    ddc: Option<Ddc>,
    demod: demod::FskDemod,
    /// All bits demodulated so far for this channel (NAVTEX bursts are slow
    /// and long; we buffer the channel's bit history and re-scan on flush).
    bits: Vec<u8>,
    channel_buf: Vec<Complex<f32>>,
    /// Texts already reported, to avoid re-emitting the same message as the
    /// buffer grows.
    seen: Vec<String>,
}

impl NavtexChannelDecoder {
    /// `input_rate` is any capture rate ≥ [`CHANNEL_RATE`]; a non-integer
    /// multiple is resampled by the DDC. `freq_offset_hz` is the NAVTEX
    /// channel center relative to the capture center (0 if the capture is
    /// already centered on the carrier).
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, CHANNEL_RATE, freq_offset_hz, CHANNEL_PASSBAND_HZ)?)
        };
        Ok(Self {
            ddc,
            demod: demod::FskDemod::new(),
            bits: Vec::new(),
            channel_buf: Vec::new(),
            seen: Vec::new(),
        })
    }

    /// Feed capture IQ; returns newly completed NAVTEX frames.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<NavtexFrame> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };
        self.demod.process(channel, &mut self.bits);

        // Try all seven 7-bit packing alignments and keep, for each, the
        // message the verified core decodes. A NAVTEX frame is only reported
        // once it has a parsed ZCZC header (so partial buffers don't emit
        // junk) and it hasn't been reported before.
        let mut out = Vec::new();
        let mut best: Option<NavtexFrame> = None;
        for phase in 0..7usize {
            let symbols = demod::pack_codes(&self.bits, phase);
            if symbols.len() < 4 {
                continue;
            }
            if let Some(message) = decode_symbols(&symbols, None) {
                if message.header_ok {
                    // Prefer the alignment yielding the most valid codes /
                    // the longest text (most fully recovered frame).
                    let better = match &best {
                        None => true,
                        Some(b) => message.text.len() > b.message.text.len(),
                    };
                    if better {
                        best = Some(NavtexFrame { message, symbols });
                    }
                }
            }
        }
        if let Some(frame) = best {
            let key = frame_key(&frame.message);
            if !self.seen.contains(&key) {
                self.seen.push(key);
                out.push(frame);
            }
        }
        out
    }

    /// Smoothed channel power level in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        self.demod.level_dbfs()
    }
}

/// Dedup key: header identity + body text uniquely names a message.
fn frame_key(m: &NavtexMessage) -> String {
    format!(
        "{:?}{:?}{:?}|{}",
        m.station, m.subject, m.message_number, m.text
    )
}

/// Convert a decoded NAVTEX frame into the normalized bus message.
///
/// `kind` is the B2 subject indicator (a single uppercase letter, or
/// `"?"` when no header parsed). `details` is the [`NavtexMessage`] JSON.
/// `decode.crc_ok` is set from `header_ok && end_ok` (a fully framed
/// `ZCZC … NNNN` message). `raw` carries the recovered wire symbols.
pub fn to_message(
    f: &NavtexFrame,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Message {
    let kind = f
        .message
        .subject
        .map(|c| c.to_ascii_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string());
    let details = serde_json::to_value(&f.message).unwrap_or(serde_json::Value::Null);
    Message {
        mode: Mode::Navtex,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality { rssi_db: Some(level_dbfs), ..Default::default() },
        decode: DecodeQuality {
            crc_ok: f.message.header_ok && f.message.end_ok,
            fec_corrected: None,
            errors: None,
        },
        body: MessageBody::Navtex { kind, details },
        raw: Some(f.symbols.clone()),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_are_spec_values() {
        assert_eq!(params::BAUD, 100.0);
        assert_eq!(params::SHIFT_HZ, 85.0);
        assert_eq!(params::FREQ_518K, 518_000);
    }

    #[test]
    fn channel_rate_is_integer_bit_multiple() {
        // Whole samples per bit (clean 100 Bd timing).
        let samples_per_bit = CHANNEL_RATE / params::BAUD;
        assert_eq!(samples_per_bit.fract(), 0.0, "{samples_per_bit} samples/bit");
        // Output rate must carry the two-sided passband (Nyquist).
        let min_rate = 2.0 * CHANNEL_PASSBAND_HZ;
        assert!(CHANNEL_RATE >= min_rate, "{CHANNEL_RATE} < {min_rate}");
    }
}
