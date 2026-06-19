//! End-to-end DEMOD validation over a SELF-GENERATED IQ burst.
//!
//! SELF-GENERATED (loopback) — clearly named `*_synth_iq`:
//!
//! There is no public ADS-L reference IQ capture, so the *physical-layer*
//! front-end ([`xng_mode_adsl::demod`]) is validated against a matching
//! modulator ([`xng_mode_adsl::modulate`]) — both anchored to the SoftRF
//! `adsl_proto_desc` framing facts (2-FSK 100 kbit/s, ±50 kHz deviation,
//! IEEE-Manchester whitening, 8-byte sync word `55 99 95 A6 9A 65 A9 6A`,
//! payload inverted). The bytes that get modulated are the SAME bytes the
//! independent `decode_vectors` oracle pins (`gen_vector.py`), so the
//! *decode core* (`Frame::parse` / `IConspicuity`) stays externally
//! anchored; only the modulate→demod IQ path is self-consistent. See
//! PROVENANCE.md.

use num_complex::Complex;
use xng_mode_adsl::{modulate, AdslChannelDecoder, CHANNEL_RATE};

/// The independently-generated ADS-L frame from `decode_vectors.rs`, WITHOUT
/// the leading OGN length byte (the air interface carries Version + 20
/// payload + 3 CRC). These bytes are the `gen_vector.py` output.
const FRAME_NO_LEN: [u8; 24] = [
    0x00, 0x57, 0xee, 0x00, 0x23, 0xd8, 0x06, 0x67, 0x95, 0x5b, 0x5b, 0x52, 0x47, 0xbe, 0xf2, 0x49,
    0x41, 0xc8, 0xd5, 0x6a, 0xaa, 0xcd, 0x0d, 0x5f,
];

#[test]
fn decodes_self_generated_burst_at_baseband_synth_iq() {
    // Modulate the known frame at baseband (no carrier offset). CHANNEL_RATE
    // input + zero offset takes the DDC-bypass fast path inside the decoder.
    let iq = modulate::burst_iq(&FRAME_NO_LEN, CHANNEL_RATE, 0.0, 0.7);

    let mut dec = AdslChannelDecoder::new(CHANNEL_RATE, 0.0).expect("decoder");
    let frames = dec.process(&iq);

    assert_eq!(frames.len(), 1, "exactly one frame should demodulate");
    let m = &frames[0].iconspicuity;

    // The recovered iConspicuity fields must equal the oracle values.
    assert_eq!(m.address, 0x3C5EE2);
    assert_eq!(m.address_table, 5);
    assert_eq!(m.address_type, "icao");
    assert_eq!(m.timestamp_q, 40);
    assert_eq!(m.flight_state_name, "airborne");
    assert_eq!(m.aircraft_category_name, "glider");
    assert_eq!(m.emergency_name, "no_emergency");
    assert_eq!(m.ground_speed_mps, 120.0);
    assert_eq!(m.altitude_hae_m, 1000);
    assert_eq!(m.vertical_rate_mps, Some(10.0));
    assert!((m.ground_track_deg - 90.0).abs() < 1e-9);
    assert_eq!(m.source_integrity, 3);
    assert_eq!(m.navigation_integrity, 11);

    // raw must be the wire bytes that Frame::parse consumed.
    assert_eq!(frames[0].wire_bytes, FRAME_NO_LEN);
}

#[test]
fn decodes_channelized_with_carrier_offset_synth_iq() {
    // Capture at 2 MS/s with the channel sitting +250 kHz off the capture
    // center: exercises the DDC mix + decimate path, not just the bypass.
    let capture_rate = 2_000_000.0;
    let offset = 250_000.0;
    let iq = modulate::burst_iq(&FRAME_NO_LEN, capture_rate, offset, 0.7);

    let mut dec = AdslChannelDecoder::new(capture_rate, offset).expect("decoder");
    let frames = dec.process(&iq);

    assert_eq!(frames.len(), 1, "frame should survive DDC channelization");
    let m = &frames[0].iconspicuity;
    assert_eq!(m.address, 0x3C5EE2);
    assert_eq!(m.altitude_hae_m, 1000);
    assert_eq!(m.ground_speed_mps, 120.0);
}

#[test]
fn to_message_carries_mode_kind_and_raw() {
    use xng_types::{AppInfo, Mode, Provenance, StationIdentity};

    let iq = modulate::burst_iq(&FRAME_NO_LEN, CHANNEL_RATE, 0.0, 0.7);
    let mut dec = AdslChannelDecoder::new(CHANNEL_RATE, 0.0).expect("decoder");
    let frames = dec.process(&iq);
    assert_eq!(frames.len(), 1);

    let source = Provenance {
        station: StationIdentity::new("XX-TEST-ADSL"),
        app: AppInfo::xng(),
        sdr: None,
        channel: None,
    };
    let level = dec.level_dbfs();
    let msg = xng_mode_adsl::to_message(&frames[0], 868_000_000, level, source);
    assert_eq!(msg.mode, Mode::AdsL);
    assert!(msg.decode.crc_ok);
    assert_eq!(msg.frequency_hz, 868_000_000);
    assert_eq!(msg.raw.as_deref(), Some(&FRAME_NO_LEN[..]));
    match msg.body {
        xng_types::MessageBody::AdsL { kind, details } => {
            assert_eq!(kind, "iconspicuity");
            assert_eq!(details["address"], 0x3C5EE2);
            assert_eq!(details["aircraft_category_name"], "glider");
            assert_eq!(details["altitude_hae_m"], 1000);
        }
        other => panic!("expected MessageBody::AdsL, got {other:?}"),
    }
}

/// A burst whose carrier polarity is flipped (low tone = chip 1) must still
/// decode: the demod searches both sync polarities and inverts accordingly.
#[test]
fn decodes_inverted_carrier_polarity_synth_iq() {
    // Negating the deviation flips every chip's tone. Re-create the burst
    // with inverted IQ by conjugating + frequency negation: simplest is to
    // build the chip stream and modulate with negative amplitude on the
    // frequency by swapping via the modulator's sign — here we just feed the
    // complex-conjugate of the baseband burst, which negates instantaneous
    // frequency.
    let iq = modulate::burst_iq(&FRAME_NO_LEN, CHANNEL_RATE, 0.0, 0.7);
    let flipped: Vec<Complex<f32>> = iq.iter().map(|s| s.conj()).collect();

    let mut dec = AdslChannelDecoder::new(CHANNEL_RATE, 0.0).expect("decoder");
    let frames = dec.process(&flipped);
    assert_eq!(
        frames.len(),
        1,
        "inverted-polarity burst should still decode"
    );
    assert_eq!(frames[0].iconspicuity.address, 0x3C5EE2);
}
