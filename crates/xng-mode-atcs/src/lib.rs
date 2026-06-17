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
//! ## IQ demodulation: TODO (stretch, not shipped)
//!
//! An IQ → bits front end (DDC to the channel, FSK discriminator at 4800
//! bps, NRZI decode, bit-sync on the 40-alternating-bit preamble) is a
//! documented future addition. It is deliberately **not** implemented
//! here: there is no public ATCS IQ vector to verify a demodulator
//! against, and the project's policy forbids shipping an unverifiable
//! self-consistency loopback. The verified, externally-anchored decode
//! layer ships now; the demod is left as a clearly marked TODO so it can
//! be added against a real capture later.
//!
//! See PROVENANCE.md for the clean-room sourcing of every protocol fact.

pub mod address;
pub mod frame;
pub mod spec200;

pub use address::{AddressType, AtcsAddress};
pub use frame::{AtcsFrame, HdlcDeframer};
pub use spec200::{decode_packet, Spec200Packet};

/// Decode one HDLC frame's bytes into a Spec-200 packet. Convenience for
/// the common pipeline frame → packet.
pub fn decode_frame(frame: &AtcsFrame) -> Option<Spec200Packet> {
    decode_packet(&frame.bytes)
}
