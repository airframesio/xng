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
//! This is the message/frame decode layer (bytes → structured fields). A
//! spec-faithful IQ demodulator (978 kbit/s 2-FSK, sync-word hunt, soft
//! deinterleave) is a documented follow-up — see PROVENANCE.md.
//!
//! Every protocol fact is anchored to an external reference; see
//! PROVENANCE.md and the `tests/` vectors.

pub mod bits;
pub mod dlac;
pub mod downlink;
pub mod fec;
pub mod uplink;

pub use downlink::UatDownlink;
pub use uplink::{FisbProduct, UatUplink};

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
