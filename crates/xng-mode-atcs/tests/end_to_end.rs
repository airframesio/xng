//! End-to-end ATCS decode: a spec-derived Spec-200 packet wrapped in a
//! real HDLC frame (genuine CRC-16/X-25 FCS) is run through the full
//! pipeline — HDLC flag-hunt + destuffing + FCS check → Spec-200 header
//! decode — and the recovered fields are asserted against the AAR header
//! layout and the sigidwiki.com worked example.
//!
//! This is documented in PROVENANCE.md as SPEC-DERIVED, not an
//! encode→decode loopback: the packet bytes are laid out by hand from the
//! published AAR Spec-200 header format, the FCS comes from the public
//! CRC-16/X-25 catalogue definition, and the decode is checked against the
//! standard's field semantics and the external sample addresses — never
//! against a modulator in this crate.

use xng_mode_atcs::address::AddressType;
use xng_mode_atcs::frame::hdlc_bits;
use xng_mode_atcs::{decode_frame, HdlcDeframer};

/// A spec-derived Spec-200 packet (see src/spec200.rs for the per-octet
/// commentary). Field MCP (source) → dispatch office (destination), both
/// railroad 125, priority 2, ARQ enabled.
fn spec200_packet_bytes() -> Vec<u8> {
    vec![
        0x24, // control: Q=0 D=0 10 PPP=010(prio 2) A=0
        0x00, 0x00, 0x00, // reserved
        0xAA, // addr lengths: src=10, dst=10
        0x51, 0x25, 0x01, 0x38, 0x26, // source "5125013826" (field MCP)
        0x21, 0x25, 0x38, 0x55, 0x38, // dest   "2125385538" (dispatch)
        0x02, 0x04, 0x05, 0x00, 0x00, 0x00, // opaque codeline user data
    ]
}

#[test]
fn full_pipeline_bits_to_packet() {
    let packet = spec200_packet_bytes();

    // Wrap in a real HDLC frame (opening flag, stuffed payload + true FCS,
    // closing flag), then feed the bit stream through the deframer.
    let bits = hdlc_bits(&packet);
    let mut deframer = HdlcDeframer::new();
    let frames = deframer.push_bits(&bits);
    assert_eq!(frames.len(), 1, "exactly one CRC-valid frame expected");

    // The deframer recovered the exact packet bytes (FCS stripped).
    let frame = &frames[0];
    assert_eq!(frame.bytes, packet);

    // Decode the Spec-200 header.
    let p = decode_frame(frame).expect("Spec-200 decode");
    assert!(p.control_well_formed);
    assert_eq!(p.priority, 2);
    assert!(!p.arq_disabled);
    assert!(!p.service_signal);

    assert_eq!(p.source.digits, "5125013826");
    assert_eq!(p.source.addr_type, AddressType::WaysideRf);
    assert_eq!(p.source.railroad, 125);

    assert_eq!(p.destination.digits, "2125385538");
    assert_eq!(p.destination.addr_type, AddressType::Host);
    assert_eq!(p.destination.railroad, 125);

    assert_eq!(p.direction, "field-to-ground");
    assert_eq!(p.user_data, vec![0x02, 0x04, 0x05, 0x00, 0x00, 0x00]);
}

#[test]
fn corrupted_frame_is_dropped_by_fcs() {
    let packet = spec200_packet_bytes();
    let mut bits = hdlc_bits(&packet);
    // Flip a payload bit inside the frame; the FCS must reject it so no
    // packet ever reaches the Spec-200 decoder.
    bits[40] ^= 1;
    let mut deframer = HdlcDeframer::new();
    assert!(deframer.push_bits(&bits).is_empty());
}

#[test]
fn frame_surrounded_by_idle_and_noise_still_decodes() {
    let packet = spec200_packet_bytes();
    let mut bits = vec![0u8; 7];
    // Idle line: flags and alternating bits before the real frame.
    bits.extend([0, 1, 1, 1, 1, 1, 1, 0]); // a stray flag
    bits.extend((0..16).map(|i| (i % 2) as u8)); // bit-sync-like preamble
    bits.extend(hdlc_bits(&packet));
    bits.extend((0..16).map(|i| (i % 2) as u8)); // trailing idle

    let mut deframer = HdlcDeframer::new();
    let frames = deframer.push_bits(&bits);
    assert_eq!(frames.len(), 1);
    let p = decode_frame(&frames[0]).unwrap();
    assert_eq!(p.source.railroad, 125);
    assert_eq!(p.direction, "field-to-ground");
}
