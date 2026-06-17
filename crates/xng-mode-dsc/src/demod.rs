//! IQ → bits front end for DSC. **Not yet implemented.**
//!
//! This module is a placeholder for the FSK demodulator that turns channel IQ
//! into the synchronised bit stream consumed by [`crate::symbol`]. It is left
//! as a documented TODO rather than shipped half-built, so the verified
//! symbol/message decode layers stand on their own.
//!
//! What a spec-faithful demod needs (ITU-R M.493 / M.541):
//!
//! - **MF/HF**: 100 Bd binary FSK, ±85 Hz shift about the assigned mark/space
//!   pair (the "B"/"Y" tones). Narrow-band; typically taken from a USB audio
//!   channel.
//! - **VHF**: 1200 Bd FSK, 1300 Hz (Y) / 2100 Hz (B) AFSK over FM.
//! - A frequency-discriminator or Goertzel/dual-filter tone detector, bit
//!   timing recovery at the symbol rate, and dot-pattern + phasing-sequence
//!   acquisition (the DX `125` / RX `111..104` phasing characters) to align
//!   the 10-bit symbol boundaries before handing 10-bit groups to
//!   [`crate::symbol::decode_bitstream`].
//!
//! Once implemented, this should feed [`crate::decode_from_bits`].
//!
//! Verifying a demod requires real recorded IQ with an independently known
//! decode; until such a vector is wired in, no demod code is committed (per
//! the project's "never commit unverified code" rule).

/// Marker for the unimplemented IQ front end. Returns `None` always.
///
/// Kept as a typed stub so downstream wiring can reference the intended entry
/// point without depending on a fabricated implementation.
pub fn demodulate_iq(_iq: &[num_complex_stub::Complex32]) -> Option<Vec<u8>> {
    None
}

/// Minimal local complex type alias placeholder so this stub compiles without
/// pulling an IQ dependency into the verified decode crate. Replaced by the
/// real `num_complex::Complex<f32>` when the demod lands.
pub mod num_complex_stub {
    /// Interleaved-IQ complex sample (placeholder).
    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    pub struct Complex32 {
        pub re: f32,
        pub im: f32,
    }
}
