//! Native NAVTEX (SITOR-B / CCIR 476) decode core for xng.
//!
//! NAVTEX is the international maritime safety-information broadcast on
//! 518 kHz (English), 490 kHz and 4209.5 kHz. On air it is 100-baud
//! narrow-shift (±85 Hz) FSK carrying the CCIR 476 seven-bit
//! constant-ratio code in collective B-mode (FEC-B): every character is
//! sent twice with time diversity.
//!
//! This crate implements the message/frame decode layer; see PROVENANCE.md
//! for the external sourcing of every protocol fact. The first layer is
//! the CCIR 476 alphabet ([`ccir476`]); the FEC-B diversity and message
//! framing layers build on it.

pub mod ccir476;
