//! Synthetic IQ loopback for the COSPAS-SARSAT biphase-L PSK demodulator.
//!
//! There is no public COSPAS-SARSAT IQ oracle, so the demod front-end is
//! validated **self-consistently**: a KNOWN-GOOD beacon hex (one of the
//! `amsa-code/fgb-decoder` compliance vectors already pinned in
//! `tests/oracle.rs`) is modulated at this mode's biphase-L ±1.1 rad / 400 bps
//! waveform (`xng_mode_sarsat::modulate`), run through the real
//! `SarsatChannelDecoder::process`, and the recovered beacon's decoded fields
//! are asserted equal to the known-good values. The DECODE core itself stays
//! oracle-anchored by `tests/oracle.rs`; this test only proves the
//! modulate→demod path recovers the frame. See PROVENANCE.md.

use num_complex::Complex;
use xng_mode_sarsat::{
    modulate::burst_iq, to_message, Format, SarsatChannelDecoder, CHANNEL_RATE,
};
use xng_types::{AppInfo, Message, MessageBody, Mode, Provenance, StationIdentity};

fn approx(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "expected {b}, got {a}");
}

/// Modulate a known long beacon at the channel rate (no DDC), demod it, and
/// assert the oracle-known fields. Vector: compliance-kit PLB - Serial with
/// coarse + offset position + PDF-2 (Vietnam, 574).
#[test]
fn decodes_known_long_beacon_synth_iq() {
    let hex = "A3E7B10016150D364D8B3689C09437";
    let iq = burst_iq(hex, CHANNEL_RATE, 0.0, 0.6);

    let mut dec = SarsatChannelDecoder::new(CHANNEL_RATE, 0.0).unwrap();
    let frames = dec.process(&iq);
    assert!(!frames.is_empty(), "demod recovered no frame from synthetic IQ");

    let f = &frames[0];
    assert_eq!(f.hex, hex, "recovered hex must match the modulated beacon");
    let b = &f.beacon;
    assert_eq!(b.protocol_type, "PLB - Serial");
    assert_eq!(b.format, Format::Long);
    assert_eq!(b.hex_id, "47CF62002CFFBFF");
    assert_eq!(b.country_code, 574);
    assert_eq!(b.cs_type_approval, Some(708));
    assert_eq!(b.beacon_serial_number, Some(22));
    let c = b.coarse_position.expect("coarse position");
    approx(c.latitude, 21.0);
    approx(c.longitude, 105.5);
    let p = b.position.expect("offset-refined position");
    approx(p.latitude, 21.041_111_111_111_11);
    approx(p.longitude, 105.49);
    assert!(b.bch1.ok, "PDF-1 BCH should verify on the recovered frame");
    assert!(b.bch2.as_ref().expect("PDF-2 present").ok, "PDF-2 BCH should verify");
}

/// Same vector, but demodulated out of a higher-rate capture with a carrier
/// frequency offset — exercises the DDC mix+decimate and the carrier-recovery
/// loop, not just the bit slicer.
#[test]
fn decodes_with_ddc_and_cfo_synth_iq() {
    let hex = "8DA41A02C17FDFF83B4235FFFFFFFF"; // Standard Location ELT - Serial, France.
    let capture_rate = 48_000.0;
    let offset = 3_500.0;
    let burst = burst_iq(hex, capture_rate, offset, 0.5);

    // Pad with leading/trailing silence so the DDC FIR history is realistic.
    let mut iq = vec![Complex::new(0.0f32, 0.0); 4_000];
    iq.extend(burst);
    iq.extend(std::iter::repeat_n(Complex::new(0.0f32, 0.0), 2_000));

    let mut dec = SarsatChannelDecoder::new(capture_rate, offset).unwrap();
    let frames = dec.process(&iq);
    assert!(!frames.is_empty(), "no frame recovered through DDC+CFO");

    let f = &frames[0];
    assert_eq!(f.hex, hex);
    let b = &f.beacon;
    assert_eq!(b.protocol_type, "ELT - Serial");
    assert_eq!(b.hex_id, "1B48340582FFBFF");
    assert_eq!(b.country_code, 218);
    assert_eq!(b.cs_type_approval, Some(104));
    assert_eq!(b.beacon_serial_number, Some(705));
    assert!(b.bch1.ok);

    // level_dbfs is a real (negative) dBFS reading, not the silent floor.
    assert!(dec.level_dbfs() > -60.0, "level estimate too low: {}", dec.level_dbfs());
}

/// The normalized-message conversion emits the Sarsat variant with the protocol
/// class as `kind` and the SarsatBeacon JSON as `details`.
#[test]
fn to_message_emits_sarsat_variant_synth_iq() {
    let hex = "A3E7B10016150D364D8B3689C09437";
    let iq = burst_iq(hex, CHANNEL_RATE, 0.0, 0.6);
    let mut dec = SarsatChannelDecoder::new(CHANNEL_RATE, 0.0).unwrap();
    let frames = dec.process(&iq);
    let f = frames.first().expect("frame");

    let source = Provenance {
        station: StationIdentity::new("TEST"),
        app: AppInfo::xng(),
        sdr: None,
        channel: None,
    };
    let msg: Message = to_message(f, 406_037_000, dec.level_dbfs(), source);

    assert_eq!(msg.mode, Mode::Sarsat);
    assert_eq!(msg.frequency_hz, 406_037_000);
    assert!(msg.decode.crc_ok, "PLB vector has valid PDF-1+PDF-2 BCH");
    assert_eq!(msg.signal.rssi_db, Some(dec.level_dbfs()));
    match &msg.body {
        MessageBody::Sarsat { kind, details } => {
            assert_eq!(kind, "PLB - Serial");
            assert_eq!(details["hex_id"], "47CF62002CFFBFF");
            assert_eq!(details["country_code"], 574);
        }
        other => panic!("expected MessageBody::Sarsat, got {other:?}"),
    }
    // raw preserves the 30-nibble wire bytes.
    assert_eq!(msg.raw.as_ref().unwrap().len(), 15);
}
