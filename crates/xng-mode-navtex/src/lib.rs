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
//! # IQ demodulation (TODO — stretch goal, not yet implemented)
//!
//! [`demod_fsk`] is a documented placeholder for the IQ→symbols front end
//! (100-baud ±85 Hz FSK discriminator, bit timing, 7-bit framing). It is
//! intentionally unimplemented: an IQ demod cannot be verified against an
//! external reference without a published IQ capture + ground-truth pair,
//! so per the crate's verification rules it is left as a TODO rather than
//! shipped unverified. The decode layer above is fully testable from a
//! symbol stream and is the verified deliverable.

pub mod ccir476;
pub mod fec;
pub mod message;

pub use message::NavtexMessage;

use xng_dsp::IqSample;

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

/// IQ→symbol front end. **Not yet implemented** — see the crate docs.
///
/// The intended contract: given channelized IQ at `sample_rate` centered
/// on the NAVTEX carrier, run a 100-baud ±85 Hz FSK discriminator with bit
/// timing recovery and emit one CCIR 476 code per 7 demodulated bits.
/// Returns the interleaved symbol stream for [`decode_symbols`].
///
/// Left as a TODO because it cannot be externally verified without a
/// published IQ capture paired with ground-truth text.
pub fn demod_fsk(_iq: &[IqSample], _sample_rate: f64) -> Result<Vec<u8>, NavtexError> {
    Err(NavtexError::DemodNotImplemented)
}

/// Errors from the NAVTEX core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavtexError {
    /// The IQ front end is not implemented (stretch goal / TODO).
    DemodNotImplemented,
}

impl std::fmt::Display for NavtexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NavtexError::DemodNotImplemented => {
                write!(f, "NAVTEX IQ FSK demod not yet implemented (see crate docs)")
            }
        }
    }
}

impl std::error::Error for NavtexError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demod_is_documented_todo() {
        assert_eq!(demod_fsk(&[], 48_000.0), Err(NavtexError::DemodNotImplemented));
    }

    #[test]
    fn params_are_spec_values() {
        assert_eq!(params::BAUD, 100.0);
        assert_eq!(params::SHIFT_HZ, 85.0);
        assert_eq!(params::FREQ_518K, 518_000);
    }
}
