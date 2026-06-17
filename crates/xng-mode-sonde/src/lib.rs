//! Native Radiosonde decode core for xng — Vaisala RS41 (RS41-SG / -SGP),
//! the most widely flown operational radiosonde worldwide.
//!
//! This crate decodes an RS41 *frame* (bytes -> structured fields). The FEC
//! layer landing first:
//!
//! 1. **De-whitening** ([`whitening`]): XOR the on-air bytes (after the
//!    8-byte header) against the fixed 64-byte mask.
//! 2. **Forward error correction** ([`rs`]): two interleaved
//!    RS(255,231) codewords over GF(2^8), correcting up to 12 byte errors
//!    each.
//!
//! Sub-block parsing (STATUS / GPS / PTU) lands next. The GFSK
//! demodulator (IQ -> bits) is a documented TODO; see PROVENANCE.md.
//!
//! Every protocol fact (the whitening mask, CRC variant, the RS
//! field/interleave) is sourced from rs1729/RS and verified in tests
//! against that project's published worked example (`rs41.txt`).

pub mod crc;
pub mod gf256;
pub mod rs;
pub mod whitening;

pub use rs::{Rs41Rs, RsResult};
