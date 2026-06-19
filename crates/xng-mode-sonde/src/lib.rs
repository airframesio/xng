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
//! The GFSK demodulator front-end (wideband IQ -> on-air frame bytes) lives
//! in [`demod`] + [`framer`] and is wired through [`SondeChannelDecoder`];
//! it reuses the AIS discriminator + integrate-and-dump structure (GMSK and
//! GFSK share it) per the workspace channelized-decoder contract. The
//! modulate->demod validation path ([`modulate`]) is self-generated (see
//! PROVENANCE.md); the decode core below stays oracle-anchored.
//!
//! Every protocol fact (offsets, the whitening mask, CRC variant, the RS
//! field/interleave, and the ECEF formula) is sourced from rs1729/RS and
//! verified in tests against that project's published worked example and
//! sample frames (`rs41.txt`).

pub mod crc;
pub mod demod;
pub mod frame;
pub mod framer;
pub mod gf256;
pub mod modulate;
pub mod rs;
pub mod whitening;

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

pub use frame::{
    decode_frame, ecef_to_geodetic, CrcStatus, DecodeError, GpsPos, GpsTime, Ptu, Rs41Frame,
};
pub use rs::{Rs41Rs, RsResult};

/// Internal demod sample rate: 10 samples/symbol at 4800 bd.
pub const CHANNEL_RATE: f64 = 48_000.0;
/// One-sided DDC passband. RS41 GFSK is ~±2.4 kHz deviation (mod index ≈ 1)
/// at 4800 bd; a ~7 kHz one-sided passband keeps the FSK sidebands plus a
/// margin for residual carrier offset.
pub const CHANNEL_PASSBAND_HZ: f64 = 7_000.0;

/// Result of running the FEC + frame decode on a de-whitened frame.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// Reed-Solomon correction result.
    pub rs: RsResult,
    /// The decoded frame.
    pub frame: Rs41Frame,
    /// The de-whitened, RS-corrected wire frame (header `86 35 F4 40 …`).
    pub wire_bytes: Vec<u8>,
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
        wire_bytes: buf,
    })
}

/// Decode a raw on-air RS41 frame: de-whiten, then [`decode_dewhitened`].
///
/// `on_air` begins with the *de-whitened* 8-byte header (`86 35 F4 40 …`)
/// followed by the whitened body — the form produced by [`SondeChannelDecoder`]
/// after it de-whitens the recovered sync header in place. (The body is
/// de-whitened here; the header passes through unchanged, matching the
/// `whitening::dewhiten_frame` contract.)
pub fn decode_on_air(on_air: &[u8]) -> Result<Decoded, DecodeError> {
    let mut buf = on_air.to_vec();
    whitening::dewhiten_frame(&mut buf);
    decode_dewhitened(&buf)
}

/// Decodes one RS41 radiosonde channel out of a wideband (or already
/// channel-rate) capture.
///
/// Pipeline: wideband IQ → [`Ddc`] (mix by `freq_offset_hz`, decimate to
/// [`CHANNEL_RATE`]) → [`demod::GfskDemod`] (frequency discriminator + offset
/// tracking + integrate-and-dump → hard NRZ bits) → [`framer::Framer`] (sync
/// correlation + LSB-first byte packing → on-air whitened frame) →
/// [`decode_on_air`] (de-whiten + interleaved RS(255,231) + sub-block parse).
pub struct SondeChannelDecoder {
    ddc: Option<Ddc>,
    demod: demod::GfskDemod,
    framer: framer::Framer,
    channel_buf: Vec<Complex<f32>>,
    bit_buf: Vec<u8>,
    frame_buf: Vec<Vec<u8>>,
}

impl SondeChannelDecoder {
    /// `input_rate` is any capture rate ≥ [`CHANNEL_RATE`]; a non-integer
    /// multiple is resampled by the DDC. `freq_offset_hz` is the sonde channel
    /// center relative to the capture center (e.g. a 404.0 MHz sonde in a
    /// capture centered elsewhere in the ~400–406 MHz sonde band).
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(
                input_rate,
                CHANNEL_RATE,
                freq_offset_hz,
                CHANNEL_PASSBAND_HZ,
            )?)
        };
        Ok(Self {
            ddc,
            demod: demod::GfskDemod::new(),
            framer: framer::Framer::new(),
            channel_buf: Vec::new(),
            bit_buf: Vec::new(),
            frame_buf: Vec::new(),
        })
    }

    /// Feed wideband IQ; returns every RS41 frame whose STATUS sub-block
    /// decodes (after de-whitening + RS correction).
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<Decoded> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };

        self.bit_buf.clear();
        self.demod.process(channel, &mut self.bit_buf);

        self.frame_buf.clear();
        self.framer.process(&self.bit_buf, &mut self.frame_buf);

        let mut out = Vec::new();
        for on_air in self.frame_buf.drain(..) {
            // The framer yields the on-air *whitened* frame (header
            // `10 B6 CA 11 …`). `decode_on_air` expects the header already
            // de-whitened, so de-whiten the 8 header bytes here, then hand the
            // header-de-whitened / body-whitened buffer to the decode core.
            let mut buf = on_air;
            whitening::xor_mask(&mut buf[..whitening::HEADER.len()], 0);
            if let Ok(d) = decode_on_air(&buf) {
                out.push(d);
            }
        }
        out
    }

    /// Smoothed channel power level in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        self.demod.level_dbfs()
    }
}

/// Convert a decoded RS41 frame into the normalized message model.
///
/// Emits [`MessageBody::Sonde`] with `kind = "rs41"` and `details` = the
/// decoded [`Rs41Frame`] as JSON. `decode.crc_ok` is the STATUS sub-block CRC
/// (always true here, since a frame only reaches this point when STATUS
/// decodes); `fec_corrected` carries the RS byte-correction count. `raw` is
/// the de-whitened, RS-corrected wire frame.
pub fn to_message(
    d: &Decoded,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Message {
    let fec = d.rs.total_corrected();
    Message {
        mode: Mode::Sonde,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality {
            rssi_db: Some(level_dbfs),
            ..Default::default()
        },
        decode: DecodeQuality {
            crc_ok: d.frame.crc.status,
            fec_corrected: Some(fec as u32),
            errors: None,
        },
        body: MessageBody::Sonde {
            kind: "rs41".to_string(),
            details: serde_json::to_value(&d.frame).unwrap_or(serde_json::Value::Null),
        },
        raw: Some(d.wire_bytes.clone()),
        source,
    }
}
