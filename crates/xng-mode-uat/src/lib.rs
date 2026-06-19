//! Native UAT (Universal Access Transceiver, 978 MHz, RTCA DO-282B) decode
//! core for xng.
//!
//! UAT carries two link types in the 978 MHz band:
//!
//! * **Downlink** — aircraft ADS-B broadcasts. A short message is an 18-byte
//!   payload (header + state vector); a long message is a 34-byte payload
//!   (header + state vector + the optional Mode-Status / Aux-State-Vector /
//!   Target-State elements). See [`UatDownlink`].
//! * **Uplink** — ground-station broadcasts carrying FIS-B (Flight
//!   Information Service – Broadcast): weather and aeronautical products. A
//!   corrected uplink MDB is 432 bytes; it frames a sequence of information
//!   frames, each (for type 0) a FIS-B APDU with a product id, product time,
//!   and segmentation flags. Text products (METAR/TAF/PIREP/winds) use DLAC
//!   6-bit packing. See [`UatUplink`] / [`FisbProduct`].
//!
//! FEC is Reed-Solomon over GF(2^8): RS(30,18) and RS(48,34) for the two
//! downlink lengths, and six byte-interleaved RS(92,72) blocks per uplink
//! frame. See [`fec`].
//!
//! This crate is both the message/frame decode layer (bytes → structured
//! fields) and the wideband IQ front-end: [`UatChannelDecoder`] mirrors the
//! ADS-B interface (single 978 MHz signal, offset 0) — DDC → 2-ary CPFSK
//! discriminator → 36-bit sync hunt → bit slice → [`decode_frame`] (RS-FEC).
//! See [`demod`] and PROVENANCE.md.
//!
//! Every protocol fact is anchored to an external reference; see
//! PROVENANCE.md and the `tests/` vectors.

pub mod bits;
pub mod demod;
pub mod dlac;
pub mod downlink;
pub mod fec;
pub mod modulate;
pub mod uplink;

pub use downlink::UatDownlink;
pub use uplink::{FisbProduct, UatUplink};

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Internal demod sample rate: 2 samples per bit at 1.041667 Mbit/s.
pub const CHANNEL_RATE: f64 = 2.0 * UAT_BIT_RATE; // 2_083_334.0
/// One-sided DDC passband: covers the ±312.5 kHz CPFSK deviation (h≈0.6)
/// plus modulation skirts, inside the CHANNEL_RATE Nyquist.
pub const CHANNEL_PASSBAND_HZ: f64 = 625_000.0;

/// A decoded UAT frame as recovered from the air: the structured message,
/// the RS symbols corrected, the with-parity wire bytes, and the channel
/// level at detection. Carries its own level so [`to_message`] needs no
/// level argument (the wideband ADS-B interface contract).
#[derive(Debug, Clone)]
pub struct UatFrame {
    pub message: UatMessage,
    pub errors: usize,
    /// With-parity octets as sliced off-air (the RS codeword / interleaved
    /// uplink frame).
    pub wire_bytes: Vec<u8>,
    pub level_dbfs: f32,
}

impl UatFrame {
    /// `"adsb"` for a downlink state vector, `"fisb"` for an uplink product.
    pub fn kind(&self) -> &'static str {
        match self.message {
            UatMessage::Downlink(_) => "adsb",
            UatMessage::Uplink(_) => "fisb",
        }
    }

    /// The decoded message as a JSON value (downlink or uplink shape).
    pub fn details(&self) -> serde_json::Value {
        match &self.message {
            UatMessage::Downlink(d) => d.to_json(),
            UatMessage::Uplink(u) => u.to_json(),
        }
    }
}

/// Decodes UAT from a wideband capture centered on 978 MHz (offset 0).
///
/// Mirrors the ADS-B wideband interface: a single signal fills the whole
/// capture, the DDC conditions it to ~2 samples/bit, the FSK demod hunts
/// the 36-bit sync words and slices candidate blocks, and
/// [`decode_frame`]'s RS gate validates them.
pub struct UatChannelDecoder {
    /// Present unless the capture is already at CHANNEL_RATE (then the demod
    /// runs straight on the input).
    ddc: Option<Ddc>,
    demod: demod::FskDemod,
    channel_buf: Vec<Complex<f32>>,
}

impl UatChannelDecoder {
    /// `input_rate` is any capture rate ≥ [`CHANNEL_RATE`]; the DDC (offset
    /// 0, since UAT fills the band it is captured in) decimates / resamples
    /// to the demod rate. Offset is always 0 for this wideband mode.
    pub fn new(input_rate: f64) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, CHANNEL_RATE, 0.0, CHANNEL_PASSBAND_HZ)?)
        };
        Ok(Self { ddc, demod: demod::FskDemod::new(), channel_buf: Vec::new() })
    }

    /// Feed wideband IQ; return the UAT frames whose RS-FEC validated.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<UatFrame> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };
        let mut out = Vec::new();
        for burst in self.demod.process(channel) {
            // A downlink burst is sliced at the long length; the short
            // block is its 30-byte prefix. Try the length the header
            // implies first, then fall back — the RS gate validates.
            let candidates: [&[u8]; 2] = if burst.downlink {
                [&burst.bytes, demod::short_prefix(&burst.bytes)]
            } else {
                [&burst.bytes, &burst.bytes]
            };
            let mut seen_short = false;
            for cand in candidates {
                // Skip the duplicate uplink candidate.
                if !burst.downlink && seen_short {
                    break;
                }
                seen_short = true;
                if let Ok((message, errors)) = decode_frame(cand) {
                    out.push(UatFrame {
                        message,
                        errors,
                        wire_bytes: cand.to_vec(),
                        level_dbfs: burst.level_dbfs,
                    });
                    break;
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

/// Convert a decoded UAT frame into the normalized message model. The frame
/// validated through RS-FEC, so `crc_ok` is always true (with the corrected
/// symbol count surfaced as `fec_corrected`).
pub fn to_message(f: &UatFrame, frequency_hz: u64, source: Provenance) -> Message {
    Message {
        mode: Mode::Uat,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality { rssi_db: Some(f.level_dbfs), ..Default::default() },
        decode: DecodeQuality {
            crc_ok: true,
            fec_corrected: Some(f.errors as u32),
            errors: None,
        },
        body: MessageBody::Uat { kind: f.kind().to_string(), details: f.details() },
        raw: Some(f.wire_bytes.clone()),
        source,
    }
}

/// 978.000 MHz — the single UAT channel.
pub const UAT_FREQUENCY_HZ: u64 = 978_000_000;
/// UAT bit rate (DO-282B): 1.041667 Mbit/s nominal.
pub const UAT_BIT_RATE: f64 = 1_041_667.0;

/// The kind of UAT message, by raw (with-parity) frame length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UatFrameKind {
    /// 30-byte downlink frame (18 data + 12 parity).
    DownlinkShort,
    /// 48-byte downlink frame (34 data + 14 parity).
    DownlinkLong,
    /// 552-byte uplink frame (6 × RS(92,72)).
    Uplink,
}

/// A fully decoded UAT message. Both variants are boxed so the enum stays
/// small regardless of which payload is the larger of the two.
#[derive(Debug, Clone)]
pub enum UatMessage {
    Downlink(Box<UatDownlink>),
    Uplink(Box<UatUplink>),
}

/// Decode a raw, with-parity UAT frame: run RS correction, then decode the
/// corrected payload. Returns the message and the number of RS symbols
/// corrected, or an error if the length is unknown or the frame is
/// uncorrectable.
pub fn decode_frame(raw: &[u8]) -> Result<(UatMessage, usize), &'static str> {
    match raw.len() {
        n if n == fec::DOWNLINK_SHORT_BLOCK || n == fec::DOWNLINK_LONG_BLOCK => {
            let c = fec::correct_downlink(raw).map_err(|_| "downlink uncorrectable")?;
            let msg = UatDownlink::decode(&c.payload)?;
            Ok((UatMessage::Downlink(Box::new(msg)), c.errors))
        }
        fec::UPLINK_FRAME_BYTES => {
            let (data, errors) = fec::correct_uplink(raw).map_err(|_| "uplink uncorrectable")?;
            let msg = UatUplink::decode(&data)?;
            Ok((UatMessage::Uplink(Box::new(msg)), errors))
        }
        _ => Err("unknown UAT frame length"),
    }
}
