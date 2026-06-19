//! SYNTHETIC end-to-end validation of the DSC MF/HF FSK demod front end.
//!
//! These tests are a self-generated modulate → demod loopback: a KNOWN symbol
//! stream (taken from the crate's external oracle vectors in
//! `tests/oracle_vectors.rs`, themselves real off-air HF DSC sequences) is
//! modulated as 100 Bd binary FSK (±85 Hz shift) IQ by [`modulate`], pushed
//! through [`DscChannelDecoder::process`], and the recovered [`DscMessage`] is
//! asserted to match the known-good decode.
//!
//! This validates ONLY the IQ→bits demod + phasing/symbol sync. The decode core
//! (symbol → message) remains anchored to the external oracle by
//! `tests/oracle_vectors.rs`; here the modulator and demodulator are
//! independent implementations of the same M.493 conventions, so a convention
//! error on either side surfaces as a loopback mismatch. See PROVENANCE.md.

use num_complex::Complex;
use xng_mode_dsc::message::{Category, EndOfSequence, FirstCommand, Format, NatureOfDistress};
use xng_mode_dsc::modulate::call_iq;
use xng_mode_dsc::{to_message, DscChannelDecoder, CHANNEL_RATE};
use xng_types::{AppInfo, Provenance, StationIdentity};

fn prov() -> Provenance {
    Provenance {
        station: StationIdentity::new("TEST-DSC"),
        app: AppInfo::xng(),
        sdr: None,
        channel: None,
    }
}

/// Distress alert (oracle_vectors::distress_alert), the data symbols of the
/// off-air sequence (the leading duplicate format specifier and DX/RX phasing
/// are added by the modulator).
const DISTRESS: &[i32] = &[
    112, 112, 25, 58, 5, 99, 70, 107, 4, 52, 60, 13, 7, 12, 52, 109, 127, 52, 127, 127,
];

/// Individual station call with two frequencies (oracle_vectors::ack_safety_test_command).
const INDIVIDUAL: &[i32] = &[
    120, 120, 32, 51, 42, 0, 0, 108, 0, 23, 71, 0, 0, 118, 126, 4, 10, 10, 4, 39, 30, 122, 54, 122,
    122,
];

/// Modulate a symbol stream at the channel rate (no DDC), pad with silence, and
/// run it through the channel decoder. Returns all messages emitted.
fn demod_at_channel_rate(symbols: &[i32]) -> Vec<xng_mode_dsc::DscMessage> {
    let mut iq = vec![Complex::new(0.0, 0.0); 400];
    iq.extend(call_iq(symbols, CHANNEL_RATE, 0.0, 0.6));
    iq.extend(vec![Complex::new(0.0, 0.0); 400]);

    let mut dec = DscChannelDecoder::new(CHANNEL_RATE, 0.0).unwrap();
    let mut out = Vec::new();
    for chunk in iq.chunks(512) {
        out.extend(dec.process(chunk));
    }
    out
}

#[test]
fn distress_alert_synth_iq() {
    let out = demod_at_channel_rate(DISTRESS);
    let m = out
        .iter()
        .find(|m| m.format == Format::DistressAlert)
        .expect("distress alert recovered from synthetic IQ");
    assert_eq!(m.category, Category::Distress);
    assert_eq!(m.nature, Some(NatureOfDistress::UndesignatedDistress));
    assert_eq!(m.from.as_deref(), Some("255805997"));
    assert_eq!(m.to.as_deref(), Some("ALL SHIPS"));
    assert_eq!(m.position.as_deref(), Some("45 26N 013 07E"));
    assert_eq!(m.time.as_deref(), Some("12:52"));
    assert_eq!(m.eos, EndOfSequence::OtherCalls);
    assert_eq!(m.ecc, 52);
    assert_eq!(m.status, "OK");
}

#[test]
fn individual_station_call_synth_iq() {
    let out = demod_at_channel_rate(INDIVIDUAL);
    let m = out
        .iter()
        .find(|m| m.format == Format::IndividualStationCall)
        .expect("individual station call recovered from synthetic IQ");
    assert_eq!(m.category, Category::Safety);
    assert_eq!(m.to.as_deref(), Some("325142000"));
    assert_eq!(m.from.as_deref(), Some("002371000"));
    assert_eq!(m.tc1, Some(FirstCommand::Test));
    assert_eq!(m.frequency.as_deref(), Some("04101.0/04393.0"));
    assert_eq!(m.eos, EndOfSequence::AcknowledgeBq);
    assert_eq!(m.ecc, 54);
    assert_eq!(m.status, "OK");
}

/// The demod path also works through the DDC: modulate at a higher capture rate
/// with a frequency offset and let the channel decoder mix + resample it down.
#[test]
fn distress_alert_via_ddc_synth_iq() {
    let capture_rate = 48_000.0;
    let offset = 6_000.0;
    let mut iq = vec![Complex::new(0.0, 0.0); 2_000];
    iq.extend(call_iq(DISTRESS, capture_rate, offset, 0.6));
    iq.extend(vec![Complex::new(0.0, 0.0); 2_000]);

    let mut dec = DscChannelDecoder::new(capture_rate, offset).unwrap();
    let mut out = Vec::new();
    for chunk in iq.chunks(2_048) {
        out.extend(dec.process(chunk));
    }
    let m = out
        .iter()
        .find(|m| m.format == Format::DistressAlert)
        .expect("distress alert recovered through the DDC");
    assert_eq!(m.from.as_deref(), Some("255805997"));
    assert_eq!(m.position.as_deref(), Some("45 26N 013 07E"));
    assert_eq!(m.status, "OK");
}

/// `to_message` maps a decoded distress alert onto the normalized model with the
/// right mode, kind, CRC flag, and details JSON.
#[test]
fn to_message_shape() {
    let out = demod_at_channel_rate(DISTRESS);
    let m = out
        .iter()
        .find(|m| m.format == Format::DistressAlert)
        .expect("distress alert recovered");
    let msg = to_message(m, 2_187_500, -12.5, prov());
    assert_eq!(msg.mode, xng_types::Mode::Dsc);
    assert_eq!(msg.frequency_hz, 2_187_500);
    assert!(msg.decode.crc_ok);
    assert_eq!(msg.signal.rssi_db, Some(-12.5));
    match &msg.body {
        xng_types::MessageBody::Dsc { kind, details } => {
            assert_eq!(kind, "distress_alert");
            assert_eq!(details["from"], "255805997");
            assert_eq!(details["position"], "45 26N 013 07E");
            assert_eq!(details["status"], "OK");
        }
        other => panic!("expected MessageBody::Dsc, got {other:?}"),
    }
    assert!(msg.raw.is_some());
}
