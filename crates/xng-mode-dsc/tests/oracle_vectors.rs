//! Oracle vectors for the DSC message/frame decoder.
//!
//! Every symbol stream and asserted decode below is reproduced VERBATIM from
//! the published unit tests of the external reference decoder
//! `alemassimo/TAOSW.DSC_Decoder` (MIT licensed), file
//! `TAOSW.DSC_Decoder.Core.Tests/SymbolsDecoderTests.cs`. Those are real
//! off-air HF DSC sequences (timestamped 2025-03..04 on 2187.5 / 8414.5 kHz)
//! with a human-verified decode. This is an external oracle, NOT an
//! encode→decode loopback. See PROVENANCE.md.
//!
//! Field-name mapping between the reference (.NET) decoder and this crate:
//!   To/From         -> to/from (MMSI or "ALL SHIPS"/area text)
//!   Format/Category -> format/category
//!   TC1/TC2         -> tc1/tc2
//!   EOS             -> eos
//!   CECC + Status   -> ecc + status
//!   Position/Time   -> position/time
//!   Nature          -> nature

use xng_mode_dsc::message::{
    decode, Category, DscMessage, EndOfSequence, FirstCommand, Format, NatureOfDistress,
    SecondCommand,
};

/// Convenience: a decoded message from a symbol slice.
fn dec(symbols: &[i32]) -> DscMessage {
    decode(symbols)
}

// --- Distress alert (format 112) ----------------------------------------

#[test]
fn distress_alert() {
    // SymbolsDecoderTests.DecodeDistressAlertTest
    let m = dec(&[
        112, 112, 25, 58, 5, 99, 70, 107, 4, 52, 60, 13, 7, 12, 52, 109, 127, 52, 127, 127,
    ]);
    assert_eq!(m.format, Format::DistressAlert);
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
fn distress_alert_with_ecc_error() {
    // DecodeDistressAlertTestWithError: ECC symbol corrupted 52 -> 51.
    let m = dec(&[
        112, 112, 25, 58, 5, 99, 70, 107, 4, 52, 60, 13, 7, 12, 52, 109, 127, 51, 127, 127,
    ]);
    assert_eq!(m.format, Format::DistressAlert);
    assert_eq!(m.from.as_deref(), Some("255805997"));
    assert_eq!(m.position.as_deref(), Some("45 26N 013 07E"));
    assert_eq!(m.ecc, 51);
    assert_eq!(m.status, "Error");
}

// --- Individual station call (format 120) -------------------------------

#[test]
fn ack_safety_test_command() {
    // DecodeAckTestMessageTest
    let m = dec(&[
        120, 120, 32, 51, 42, 0, 0, 108, 0, 23, 71, 0, 0, 118, 126, 4, 10, 10, 4, 39, 30, 122, 54,
        122, 122,
    ]);
    assert_eq!(m.format, Format::IndividualStationCall);
    assert_eq!(m.category, Category::Safety);
    assert_eq!(m.to.as_deref(), Some("325142000"));
    assert_eq!(m.from.as_deref(), Some("002371000"));
    assert_eq!(m.tc1, Some(FirstCommand::Test));
    assert_eq!(m.tc2, Some(SecondCommand::NoInformation));
    assert_eq!(m.frequency.as_deref(), Some("04101.0/04393.0"));
    assert_eq!(m.eos, EndOfSequence::AcknowledgeBq);
    assert_eq!(m.ecc, 54);
    assert_eq!(m.status, "OK");
}

#[test]
fn j3e_routine_with_freq() {
    // DecodeJ3ETestMessageTest
    let m = dec(&[
        120, 120, 34, 18, 55, 0, 0, 100, 0, 23, 71, 0, 0, 109, 126, 4, 10, 10, 4, 39, 30, 122, 27,
        122, 122,
    ]);
    assert_eq!(m.format, Format::IndividualStationCall);
    assert_eq!(m.category, Category::Routine);
    assert_eq!(m.to.as_deref(), Some("341855000"));
    assert_eq!(m.from.as_deref(), Some("002371000"));
    assert_eq!(m.tc1, Some(FirstCommand::J3eTp));
    assert_eq!(m.tc2, Some(SecondCommand::NoInformation));
    assert_eq!(m.frequency.as_deref(), Some("04101.0/04393.0"));
    assert_eq!(m.eos, EndOfSequence::AcknowledgeBq);
    assert_eq!(m.ecc, 27);
    assert_eq!(m.status, "OK");
}

#[test]
fn j3e_single_frequency() {
    // DecodeJ3ETestMessageTest2 (single frequency 08414.5)
    let m = dec(&[
        120, 120, 0, 23, 71, 0, 4, 100, 23, 82, 30, 0, 0, 109, 126, 8, 41, 45, 126, 126, 126, 117,
        7, 117, 117,
    ]);
    assert_eq!(m.to.as_deref(), Some("002371000"));
    assert_eq!(m.from.as_deref(), Some("238230000"));
    assert_eq!(m.frequency.as_deref(), Some("08414.5"));
    assert_eq!(m.eos, EndOfSequence::AcknowledgeRq);
    assert_eq!(m.ecc, 7);
    assert_eq!(m.status, "OK");
}

#[test]
fn position_requested() {
    // DecodeRequestTest: TC NoInformation, field 15 == 126 -> "Position Requested"
    let m = dec(&[
        120, 120, 51, 89, 99, 19, 50, 100, 0, 27, 11, 0, 0, 126, 126, 126, 126, 126, 126, 126,
        126, 117, 81, 117, 117,
    ]);
    assert_eq!(m.to.as_deref(), Some("518999195"));
    assert_eq!(m.from.as_deref(), Some("002711000"));
    assert_eq!(m.tc1, Some(FirstCommand::NoInformation));
    assert_eq!(m.tc2, Some(SecondCommand::NoInformation));
    assert_eq!(m.nature_description.as_deref(), Some("Position Requested"));
    assert_eq!(m.eos, EndOfSequence::AcknowledgeRq);
    assert_eq!(m.ecc, 81);
    assert_eq!(m.status, "OK");
}

#[test]
fn individual_with_position() {
    // DecodeRequestTest2: field 15 == 55 -> position follows
    let m = dec(&[
        120, 120, 0, 25, 70, 0, 0, 108, 23, 20, 19, 71, 50, 109, 126, 55, 5, 85, 30, 1, 34, 117,
        18, 117, 117,
    ]);
    assert_eq!(m.to.as_deref(), Some("002570000"));
    assert_eq!(m.from.as_deref(), Some("232019715"));
    assert_eq!(m.tc1, Some(FirstCommand::J3eTp));
    assert_eq!(m.position.as_deref(), Some("58 53N 001 34E"));
    assert_eq!(m.eos, EndOfSequence::AcknowledgeRq);
    assert_eq!(m.ecc, 18);
    assert_eq!(m.status, "OK");
}

#[test]
fn vhf_channel_pair() {
    // DecodeRequestTest5: frequency field begins with 90 -> VHF channels
    let m = dec(&[
        120, 120, 37, 11, 95, 0, 0, 100, 0, 27, 11, 0, 0, 126, 126, 90, 87, 49, 90, 82, 25, 117,
        37, 117, 117,
    ]);
    assert_eq!(m.to.as_deref(), Some("371195000"));
    assert_eq!(m.from.as_deref(), Some("002711000"));
    assert_eq!(
        m.frequency.as_deref(),
        Some("Duplex channel 749 - Duplex channel 225")
    );
    assert_eq!(m.eos, EndOfSequence::AcknowledgeRq);
    assert_eq!(m.ecc, 37);
    assert_eq!(m.status, "OK");
}

#[test]
fn individual_ship_to_ship_with_position() {
    // DecodeRequestTest6
    let m = dec(&[
        120, 120, 35, 20, 2, 55, 20, 108, 27, 10, 2, 60, 10, 109, 126, 55, 3, 61, 60, 21, 23, 117,
        118, -1, -1,
    ]);
    assert_eq!(m.to.as_deref(), Some("352002552"));
    assert_eq!(m.from.as_deref(), Some("271002601"));
    assert_eq!(m.tc1, Some(FirstCommand::J3eTp));
    assert_eq!(m.position.as_deref(), Some("36 16N 021 23E"));
    assert_eq!(m.eos, EndOfSequence::AcknowledgeRq);
    // ECC value can exceed 99 (it is a 7-bit field, 0..127).
    assert_eq!(m.ecc, 118);
    assert_eq!(m.status, "OK");
}

// --- All-ships call (format 116) ----------------------------------------

#[test]
fn all_ships_safety() {
    // DecodeAllShipsCallTest
    let m = dec(&[
        116, 116, 108, 0, 23, 71, 0, 0, 109, 126, 4, 12, 50, 4, 12, 50, 127, 36, 127, 127,
    ]);
    assert_eq!(m.format, Format::AllShipsCall);
    assert_eq!(m.category, Category::Safety);
    assert_eq!(m.to.as_deref(), Some("ALL SHIPS"));
    assert_eq!(m.from.as_deref(), Some("002371000"));
    assert_eq!(m.tc1, Some(FirstCommand::J3eTp));
    assert_eq!(m.tc2, Some(SecondCommand::NoInformation));
    assert_eq!(m.frequency.as_deref(), Some("04125.0/04125.0"));
    assert_eq!(m.eos, EndOfSequence::OtherCalls);
    assert_eq!(m.ecc, 36);
    assert_eq!(m.status, "OK");
}

#[test]
fn all_ships_safety_2() {
    // DecodeAllShipsCallTest4
    let m = dec(&[
        116, 116, 108, 0, 22, 41, 2, 20, 109, 126, 2, 13, 20, 1, 70, 70, 127, 71, 127, 127,
    ]);
    assert_eq!(m.format, Format::AllShipsCall);
    assert_eq!(m.from.as_deref(), Some("002241022"));
    assert_eq!(m.frequency.as_deref(), Some("02132.0/01707.0"));
    assert_eq!(m.eos, EndOfSequence::OtherCalls);
    assert_eq!(m.ecc, 71);
    assert_eq!(m.status, "OK");
}

// --- Geographic-area call (format 102) ----------------------------------

#[test]
fn geographic_area_call() {
    // DecodeAreaTest
    let m = dec(&[
        102, 102, 4, 40, 3, 5, 8, 108, 0, 22, 75, 40, 0, 109, 126, 2, 18, 20, 2, 18, 20, 127, 49,
        127, 127,
    ]);
    assert_eq!(m.format, Format::GeographicAreaGroupCall);
    assert_eq!(m.category, Category::Safety);
    assert_eq!(
        m.to.as_deref(),
        Some("North-East (NE), Reference point: 44°, 3°, Vertical side: 5°, Horizontal side: 8°")
    );
    assert_eq!(m.from.as_deref(), Some("002275400"));
    assert_eq!(m.tc1, Some(FirstCommand::J3eTp));
    assert_eq!(m.frequency.as_deref(), Some("02182.0/02182.0"));
    assert_eq!(m.eos, EndOfSequence::OtherCalls);
    assert_eq!(m.ecc, 49);
    assert_eq!(m.status, "OK");
}

#[test]
fn geographic_area_call_2() {
    // DecodeAreaTest2
    let m = dec(&[
        102, 102, 6, 0, 3, 8, 14, 108, 0, 21, 91, 0, 0, 109, 126, 1, 73, 40, 2, 7, 80, 127, 30,
        127, 127,
    ]);
    assert_eq!(m.format, Format::GeographicAreaGroupCall);
    assert_eq!(
        m.to.as_deref(),
        Some("North-East (NE), Reference point: 60°, 3°, Vertical side: 8°, Horizontal side: 14°")
    );
    assert_eq!(m.from.as_deref(), Some("002191000"));
    assert_eq!(m.frequency.as_deref(), Some("01734.0/02078.0"));
    assert_eq!(m.eos, EndOfSequence::OtherCalls);
    assert_eq!(m.ecc, 30);
    assert_eq!(m.status, "OK");
}

// --- Error / partial-recovery cases -------------------------------------

#[test]
fn truncated_stream_ecc_error() {
    // DecodeUnknownErrorTest: stream cut off after symbol 12; freq/EOS/ECC
    // unrecoverable. To/From still decode; ECC fails.
    let m = dec(&[
        120, 120, 0, 21, 50, 10, 0, 108, 22, 93, 64, 0, 0, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
        -1, -1,
    ]);
    assert_eq!(m.format, Format::IndividualStationCall);
    assert_eq!(m.category, Category::Safety);
    assert_eq!(m.to.as_deref(), Some("002150100"));
    assert_eq!(m.from.as_deref(), Some("229364000"));
    assert_eq!(m.tc1, Some(FirstCommand::Unknown)); // symbol -1 -> unknown
    assert_eq!(m.tc2, Some(SecondCommand::Unknown));
    assert_eq!(m.frequency.as_deref(), Some("--error--"));
    assert_eq!(m.eos, EndOfSequence::Unknown);
    assert_eq!(m.ecc, -1);
    assert_eq!(m.status, "Error");
}

#[test]
fn partial_recovery_tc2_known() {
    // DecodeUnknownErrorTest2: TC1 erased (-1) but TC2 == 111 (Medical
    // transports) recovered; frequency unrecoverable; ECC erased -> Error.
    let m = dec(&[
        120, 120, 0, 22, 41, 2, 20, 108, 25, 75, 30, 0, 0, -1, 111, -1, -1, 126, 126, -1, -1, -1,
        -1, 117, -1,
    ]);
    assert_eq!(m.to.as_deref(), Some("002241022"));
    assert_eq!(m.from.as_deref(), Some("257530000"));
    assert_eq!(m.tc1, Some(FirstCommand::Unknown));
    assert_eq!(m.tc2, Some(SecondCommand::MedicalTransports));
    assert_eq!(m.frequency.as_deref(), Some("--error--"));
    assert_eq!(m.eos, EndOfSequence::AcknowledgeRq);
    assert_eq!(m.ecc, -1);
    assert_eq!(m.status, "Error");
}

#[test]
fn mmsi_with_erasures_shows_placeholders() {
    // DecodeAckTestMessageTest3: addresses partially erased -> "_" fillers.
    let m = dec(&[
        120, 120, 24, 91, -1, -1, 0, 108, -1, -1, -1, -1, -1, 100, -1, 126, 126, 126, 126, -1, 126,
        122, 4, 122, 122,
    ]);
    assert_eq!(m.to.as_deref(), Some("2491____0"));
    assert_eq!(m.from.as_deref(), Some("_________"));
    assert_eq!(m.tc1, Some(FirstCommand::AllModesTp));
    assert_eq!(m.tc2, Some(SecondCommand::Unknown));
    assert_eq!(m.eos, EndOfSequence::AcknowledgeBq);
    assert_eq!(m.ecc, 4);
    assert_eq!(m.status, "Error");
}

// --- JSON output --------------------------------------------------------

#[test]
fn distress_alert_json() {
    let m = dec(&[
        112, 112, 25, 58, 5, 99, 70, 107, 4, 52, 60, 13, 7, 12, 52, 109, 127, 52, 127, 127,
    ]);
    let json = m.to_json();
    // Round-trip and spot-check the load-bearing fields.
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["format"], "distress_alert");
    assert_eq!(v["category"], "distress");
    assert_eq!(v["from"], "255805997");
    assert_eq!(v["to"], "ALL SHIPS");
    assert_eq!(v["nature"], "undesignated_distress");
    assert_eq!(v["position"], "45 26N 013 07E");
    assert_eq!(v["time"], "12:52");
    assert_eq!(v["eos"], "other_calls");
    assert_eq!(v["ecc"], 52);
    assert_eq!(v["status"], "OK");

    // Full serde round-trip preserves equality.
    let back: DscMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
}
