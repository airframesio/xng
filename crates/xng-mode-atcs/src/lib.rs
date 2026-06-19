//! Native ATCS (Advanced Train Control System, AAR Spec-200) decode core.
//!
//! ATCS is the railroad data-radio system that links a dispatch office /
//! ground network to wayside field equipment (MCPs) over a pair of 900 MHz
//! channels at 4800 bps FSK. The RF link carries a synchronous HDLC-LAPB
//! bit stream; inside each HDLC frame is a Spec-200 (X.25-style) packet
//! whose header carries the source and destination ATCS addresses, a
//! priority/ARQ control field, and the message-type number.
//!
//! This crate delivers the **decode layer** (bits/bytes → structured
//! fields):
//!
//! * [`frame`] — HDLC/LAPB deframing: flag hunt, bit destuffing, FCS
//!   (CRC-16/X-25) check → raw frame bytes.
//! * [`address`] — ATCS address decode: direction/type digit, AAR railroad
//!   number, line/territory and node, from the BCD digit string.
//! * [`spec200`] — Spec-200 Layer-3 packet header: control octet
//!   (priority, ARQ, service-signal flags), the BCD address-length octet,
//!   the source and destination addresses, and the raw user payload.
//!
//! The full payload-protocol decode (the vendor codeline protocols carried
//! inside the user data, e.g. Genisys / ARES) is intentionally **out of
//! scope**; this crate stops at the Spec-200 header and hands back the raw
//! payload bytes.
//!
//! ## IQ demodulation
//!
//! The IQ → bits front end is [`demod::FskDemod`] driven through a
//! [`xng_dsp::Ddc`] by [`AtcsChannelDecoder`]: DDC to the 24 kHz channel,
//! a 2-FSK frequency discriminator at 4800 bps with timing recovery and NRZI
//! decode, feeding the existing [`frame::HdlcDeframer`] (bit-sync on the
//! 40-alternating-bit preamble + flag sync are handled by the discriminator
//! settling + the deframer's flag hunt). There is no public ATCS IQ oracle,
//! so the modulate → demod path is exercised by a clearly-named SYNTHETIC
//! loopback test ([`modulate`]) over a spec-derived frame; the DECODE core
//! (HDLC deframe + Spec-200 header) remains externally anchored by its own
//! tests. See PROVENANCE.md for the clean-room sourcing of every protocol
//! fact and the synthetic-vs-oracle boundary.

pub mod address;
pub mod demod;
pub mod frame;
pub mod modulate;
pub mod spec200;

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

pub use address::{AddressType, AtcsAddress};
pub use frame::{AtcsFrame, HdlcDeframer};
pub use spec200::{decode_packet, Spec200Packet};

/// Internal demod sample rate: 5 samples per bit at 4800 bd.
pub const CHANNEL_RATE: f64 = 24_000.0;
/// One-sided channel passband (2-FSK ±1800 Hz deviation plus the 4800 bd
/// sideband skirt, comfortably inside an ATCS 12.5/25 kHz channel).
pub const CHANNEL_PASSBAND_HZ: f64 = 4_800.0;

/// Decode one HDLC frame's bytes into a Spec-200 packet. Convenience for
/// the common pipeline frame → packet.
pub fn decode_frame(frame: &AtcsFrame) -> Option<Spec200Packet> {
    decode_packet(&frame.bytes)
}

/// A decoded ATCS frame: the CRC-valid HDLC frame plus the Spec-200 packet
/// recovered from it. Emitted by [`AtcsChannelDecoder::process`].
#[derive(Debug, Clone)]
pub struct AtcsDecoded {
    /// The CRC-valid HDLC frame (raw bytes, FCS stripped).
    pub frame: AtcsFrame,
    /// The decoded Spec-200 packet header + raw user payload.
    pub packet: Spec200Packet,
}

/// Decodes one ATCS data-radio channel out of a wideband capture.
///
/// Mirrors the AIS channelized template: owns an internal [`Ddc`] that mixes
/// the capture IQ down to [`CHANNEL_RATE`] at `freq_offset_hz`, runs the
/// 2-FSK [`demod::FskDemod`], feeds the NRZI link bits to the streaming
/// [`HdlcDeframer`], and decodes each CRC-valid frame's Spec-200 header.
pub struct AtcsChannelDecoder {
    ddc: Option<Ddc>,
    demod: demod::FskDemod,
    deframer: HdlcDeframer,
    channel_buf: Vec<Complex<f32>>,
    bit_buf: Vec<u8>,
}

impl AtcsChannelDecoder {
    /// `input_rate` is any capture rate ≥ the 24 kHz channel rate; a
    /// non-integer multiple is resampled by the DDC. `freq_offset_hz` is the
    /// channel center relative to the capture center (0 for a capture already
    /// centered on the channel at the channel rate, which skips the DDC).
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
            demod: demod::FskDemod::new(),
            deframer: HdlcDeframer::new(),
            channel_buf: Vec::new(),
            bit_buf: Vec::new(),
        })
    }

    /// Feed wideband IQ; returns the Spec-200 packets recovered from every
    /// CRC-valid HDLC frame in this chunk.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<AtcsDecoded> {
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

        let mut out = Vec::new();
        for &bit in &self.bit_buf {
            if let Some(frame) = self.deframer.push_bit(bit) {
                if let Some(packet) = decode_packet(&frame.bytes) {
                    out.push(AtcsDecoded { frame, packet });
                }
            }
        }
        out
    }

    /// Smoothed channel power level in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        self.demod.level_dbfs()
    }
}

/// Spec-200 packet-type label for a decoded frame. Spec-200 carries no
/// single "message type" octet at the header level the way some link layers
/// do; the meaningful packet classification is the traffic direction
/// (ground-to-field / field-to-ground), which the decode already derives.
fn packet_kind(d: &AtcsDecoded) -> String {
    d.packet.direction.to_string()
}

/// Convert a decoded ATCS frame into the normalized message model.
///
/// Emits [`MessageBody::Atcs`] with `kind` = the Spec-200 packet
/// classification (traffic direction) and `details` = the [`Spec200Packet`]
/// JSON. `raw` is the on-wire HDLC frame bytes (FCS stripped); `crc_ok` is
/// true because only FCS-valid frames reach here.
pub fn to_message(
    d: &AtcsDecoded,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Message {
    Message {
        mode: Mode::Atcs,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality {
            rssi_db: Some(level_dbfs),
            ..Default::default()
        },
        decode: DecodeQuality {
            crc_ok: true,
            fec_corrected: None,
            errors: None,
        },
        body: MessageBody::Atcs {
            kind: packet_kind(d),
            details: serde_json::to_value(&d.packet).unwrap_or(serde_json::Value::Null),
        },
        raw: Some(d.frame.bytes.clone()),
        source,
    }
}
