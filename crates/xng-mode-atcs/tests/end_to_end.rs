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

use num_complex::Complex;
use xng_mode_atcs::address::AddressType;
use xng_mode_atcs::frame::hdlc_bits;
use xng_mode_atcs::{decode_frame, AtcsChannelDecoder, HdlcDeframer, CHANNEL_RATE};

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

/// SYNTHETIC IQ loopback (modulate → FSK-demod → deframe → decode).
///
/// This validates the IQ front end ([`AtcsChannelDecoder`] /
/// `demod::FskDemod`) end-to-end. It is self-generated, NOT an external
/// oracle: no public ATCS IQ capture exists. The FRAME it carries is the
/// same spec-derived Spec-200 packet the oracle-anchored decode tests use,
/// wrapped with a genuine CRC-16/X-25 FCS; we 2-FSK/NRZI modulate it
/// (`xng_mode_atcs::modulate`), run it through the channel decoder, and
/// assert the recovered Spec-200 fields equal the known-good values. The
/// DECODE core stays oracle-anchored by the tests above — see PROVENANCE.md.
#[test]
fn decodes_synth_iq_at_channel_rate() {
    use xng_mode_atcs::modulate::burst_iq;

    let packet = spec200_packet_bytes();

    // Modulate at the channel rate (DDC bypassed: offset 0, rate match), with
    // lead-in/lead-out silence so the demod's DC/timing loops settle.
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); 200];
    iq.extend(burst_iq(&packet, CHANNEL_RATE, 0.0, 0.5));
    iq.extend(vec![Complex::new(0.0f32, 0.0f32); 200]);

    let mut dec = AtcsChannelDecoder::new(CHANNEL_RATE, 0.0).unwrap();
    let mut found = Vec::new();
    for chunk in iq.chunks(256) {
        found.extend(dec.process(chunk));
    }
    assert_eq!(found.len(), 1, "expected exactly one decoded frame");

    let d = &found[0];
    assert_eq!(d.frame.bytes, packet, "wire bytes round-trip");
    let p = &d.packet;
    assert!(p.control_well_formed);
    assert_eq!(p.priority, 2);
    assert!(!p.arq_disabled);
    assert_eq!(p.source.digits, "5125013826");
    assert_eq!(p.source.addr_type, AddressType::WaysideRf);
    assert_eq!(p.source.railroad, 125);
    assert_eq!(p.destination.digits, "2125385538");
    assert_eq!(p.destination.addr_type, AddressType::Host);
    assert_eq!(p.direction, "field-to-ground");
    assert_eq!(p.user_data, vec![0x02, 0x04, 0x05, 0x00, 0x00, 0x00]);

    // The demod measured real signal energy on the burst.
    assert!(dec.level_dbfs() > -60.0, "level should reflect the burst");
}

/// SYNTHETIC IQ through the full DDC: a wideband capture offset from the
/// channel center, plus light noise and a small carrier offset. Validates
/// the `Ddc` mix/decimate path and the demod's carrier-offset tracking.
#[test]
fn decodes_synth_iq_wideband_with_ddc() {
    use xng_mode_atcs::modulate::burst_iq;

    let packet = spec200_packet_bytes();
    let fs = 240_000.0; // 10× the channel rate
    let offset = 50_000.0; // channel sits +50 kHz from capture center

    // 300 Hz carrier offset on top of the channel offset (radio + rx ppm).
    let burst = burst_iq(&packet, fs, offset + 300.0, 0.4);
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); 4_000];
    iq.extend(burst);
    iq.extend(vec![Complex::new(0.0f32, 0.0f32); 4_000]);

    // Light noise.
    let mut state: u64 = 0xa1c5_0d00_face_b00c;
    let mut noise = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state as f32 / u64::MAX as f32) * 2.0 - 1.0
    };
    for s in &mut iq {
        *s += Complex::new(noise() * 0.01, noise() * 0.01);
    }

    let mut dec = AtcsChannelDecoder::new(fs, offset).unwrap();
    let mut found = Vec::new();
    for chunk in iq.chunks(8_192) {
        found.extend(dec.process(chunk));
    }
    assert_eq!(found.len(), 1, "wideband+DDC decode of one frame");
    let p = &found[0].packet;
    assert_eq!(p.source.digits, "5125013826");
    assert_eq!(p.destination.digits, "2125385538");
    assert_eq!(p.direction, "field-to-ground");
}

/// The decoded frame maps onto the normalized message model with the right
/// mode, body variant, CRC flag, and `details` JSON.
#[test]
fn synth_iq_to_message() {
    use xng_mode_atcs::modulate::burst_iq;
    use xng_mode_atcs::to_message;
    use xng_types::{AppInfo, MessageBody, Mode, Provenance, StationIdentity};

    let packet = spec200_packet_bytes();
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); 200];
    iq.extend(burst_iq(&packet, CHANNEL_RATE, 0.0, 0.5));
    iq.extend(vec![Complex::new(0.0f32, 0.0f32); 200]);

    let mut dec = AtcsChannelDecoder::new(CHANNEL_RATE, 0.0).unwrap();
    let mut found = Vec::new();
    for chunk in iq.chunks(256) {
        found.extend(dec.process(chunk));
    }
    assert_eq!(found.len(), 1);

    let prov = Provenance {
        station: StationIdentity::new("XX-TEST-ATCS"),
        app: AppInfo::xng(),
        sdr: None,
        channel: None,
    };
    let msg = to_message(&found[0], 935_000_000, dec.level_dbfs(), prov);
    assert_eq!(msg.mode, Mode::Atcs);
    assert_eq!(msg.frequency_hz, 935_000_000);
    assert!(msg.decode.crc_ok);
    assert_eq!(msg.raw.as_deref(), Some(&packet[..]));
    match &msg.body {
        MessageBody::Atcs { kind, details } => {
            assert_eq!(kind, "field-to-ground");
            assert_eq!(details["source"]["digits"], "5125013826");
            assert_eq!(details["destination"]["addr_type"], "host");
            assert_eq!(details["priority"], 2);
        }
        other => panic!("expected MessageBody::Atcs, got {other:?}"),
    }
}
