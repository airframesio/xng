//! Native Radiosonde decode core for xng — Vaisala RS41 (RS41-SG / -SGP),
//! the most widely flown operational radiosonde worldwide.
//!
//! This crate decodes an RS41 *frame* (bytes -> structured fields). The
//! pipeline a frame goes through:
//!
//! 1. **De-whitening** ([`whitening`]): XOR the on-air bytes (after the
//!    8-byte header) against the fixed 64-byte mask.
//! 2. **Forward error correction** ([`rs`]): two interleaved
//!    RS(255,231) codewords over GF(2^8), correcting up to 12 byte errors
//!    each.
//! 3. **Sub-block parsing** ([`frame`]): the `ID | LEN | DATA | CRC16`
//!    chain — STATUS (serial, frame#, battery), GPS-INFO (week, TOW),
//!    GPS-POS (ECEF -> lat/lon/alt + velocity), PTU (raw T/H/P + this
//!    frame's calibration sub-frame).
//!
//! The GFSK demodulator (IQ -> bits) is a documented TODO; see
//! PROVENANCE.md.
//!
//! Every protocol fact (offsets, the whitening mask, CRC variant, the RS
//! field/interleave, and the ECEF formula) is sourced from rs1729/RS and
//! verified in tests against that project's published worked example and
//! sample frames (`rs41.txt`).

pub mod crc;
pub mod frame;
pub mod gf256;
pub mod rs;
pub mod whitening;

pub use frame::{
    decode_frame, ecef_to_geodetic, CrcStatus, DecodeError, GpsPos, GpsTime, Ptu, Rs41Frame,
};
pub use rs::{Rs41Rs, RsResult};

/// Result of running the FEC + frame decode on a de-whitened frame.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// Reed-Solomon correction result.
    pub rs: RsResult,
    /// The decoded frame.
    pub frame: Rs41Frame,
}

/// Decode an already-de-whitened, on-air-length RS41 frame: run Reed-Solomon
/// correction (in place on a copy), then parse the sub-blocks.
///
/// `frame` is the de-whitened byte stream beginning with the 8-byte sync
/// header (`86 35 F4 40 ...`), length 320 (standard) or up to 518
/// (extended). This is the form of the published rs1729/RS sample frames.
pub fn decode_dewhitened(frame: &[u8]) -> Result<Decoded, DecodeError> {
    if frame.len() < frame::STD_FRAME_LEN {
        return Err(DecodeError::TooShort(frame.len()));
    }
    let mut buf = frame.to_vec();
    let rs = Rs41Rs::new();
    let rs_result = rs.correct_frame(&mut buf);
    let decoded = decode_frame(&buf)?;
    Ok(Decoded {
        rs: rs_result,
        frame: decoded,
    })
}

/// Decode a raw on-air RS41 frame: de-whiten, then [`decode_dewhitened`].
///
/// `on_air` begins with the whitened 8-byte header (`10 B6 CA 11 ...`).
pub fn decode_on_air(on_air: &[u8]) -> Result<Decoded, DecodeError> {
    let mut buf = on_air.to_vec();
    whitening::dewhiten_frame(&mut buf);
    decode_dewhitened(&buf)
}
