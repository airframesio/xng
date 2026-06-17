//! Native NAVTEX (SITOR-B / CCIR 476) decode core for xng.
//!
//! NAVTEX is the international maritime safety-information broadcast on
//! 518 kHz (English), 490 kHz and 4209.5 kHz. On air it is 100-baud
//! narrow-shift (±85 Hz) FSK carrying the CCIR 476 seven-bit
//! constant-ratio code in collective B-mode (FEC-B): every character is
//! sent twice with time diversity.
//!
//! This crate implements the message/frame decode layer; see PROVENANCE.md
//! for the external sourcing of every protocol fact.
//!
//! - [`ccir476`] — the 4-of-7 constant-ratio alphabet (LTRS/FIGS shift),
//!   bit packing, and the constant-ratio parity check.
//! - [`fec`] — FEC-B time-diversity recovery (DX copy preferred, RX
//!   fallback five characters earlier) and phasing sync.

pub mod ccir476;
pub mod fec;
