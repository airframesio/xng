//! Real off-air ADS-C messages (from MIT-licensed libacars'
//! `examples/adsc_get_position.c`), with expected values cross-verified by
//! independent reimplementation. Field-exact conformance tests.

use xng_acars::adsc::{AdscTag, ReportKind};
use xng_acars::{decode, AcarsApp};

fn adsc_tags(text: &str) -> Vec<AdscTag> {
    let d = decode("B6", text, true);
    let Some(AcarsApp::Adsc { envelope, message }) = d.app else {
        panic!("expected ADS-C for {text}");
    };
    assert!(envelope.crc_ok, "real message must pass CRC: {text}");
    assert!(!message.err, "must parse fully: {text}");
    message.tags
}

fn close(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() < eps
}

#[test]
fn vt_anb_basic_report_with_refs_and_meteo() {
    let tags = adsc_tags(
        "/BOMASAI.ADS.VT-ANB072501A070A988CA73248F0E5DC10200000F5EE1ABC000102B885E0A19F5",
    );
    assert_eq!(tags.len(), 4);

    let AdscTag::Report(r) = &tags[0] else { panic!("tag 0: {tags:?}") };
    assert_eq!(r.kind, ReportKind::Basic);
    assert!(close(r.lat, 52.0401764, 1e-6), "lat {}", r.lat);
    assert!(close(r.lon, 19.8038864, 1e-6), "lon {}", r.lon);
    assert_eq!(r.alt_ft, 36004);
    assert!(close(r.timestamp_s, 3273.125, 1e-9));
    assert_eq!(r.accuracy, 7);
    assert!(r.nav_redundancy_ok);
    assert!(!r.tcas_ok);

    let AdscTag::EarthRef { true_track_deg, ground_speed_kt, vert_speed_fpm, .. } = &tags[1]
    else {
        panic!("tag 1: {tags:?}")
    };
    assert!(close(*true_track_deg, 263.7, 0.1));
    assert!(close(*ground_speed_kt, 516.0, 0.01));
    assert_eq!(*vert_speed_fpm, 0);

    let AdscTag::AirRef { true_heading_deg, mach, .. } = &tags[2] else { panic!() };
    assert!(close(*true_heading_deg, 266.8, 0.1));
    assert!(close(*mach, 0.8555, 1e-4));

    let AdscTag::Meteo { wind_speed_kt, wind_dir_deg, temperature_c, .. } = &tags[3] else {
        panic!()
    };
    assert!(close(*wind_speed_kt, 43.5, 0.01));
    assert!(close(*wind_dir_deg, 46.4, 0.1));
    assert!(close(*temperature_c, -62.75, 0.01));
}

#[test]
fn a6_pfe_report_with_climb() {
    let tags = adsc_tags(
        "/AUHASMO.ADS.A6-PFE0724D9586A36C92B2DCF1F0E74A8E4807C0F7219AF407C10422E9E08A1C4",
    );
    let AdscTag::Report(r) = &tags[0] else { panic!() };
    assert!(close(r.lat, 51.8189049, 1e-6));
    assert!(close(r.lon, 18.6704063, 1e-6));
    assert_eq!(r.alt_ft, 37552);
    assert!(r.tcas_ok);
    let AdscTag::EarthRef { vert_speed_fpm, ground_speed_kt, .. } = &tags[1] else { panic!() };
    assert_eq!(*vert_speed_fpm, 496);
    assert!(close(*ground_speed_kt, 457.0, 0.01));
}

#[test]
fn hb_jnb_waypoint_change_with_predicted_route() {
    let tags = adsc_tags(
        "/CTUE1YA.ADS.HB-JNB1424AB686D9308CA2EBA1D0D24A2C06C1B48CA004A248050667908CA004BF6",
    );
    let AdscTag::Report(r) = &tags[0] else { panic!() };
    assert_eq!(r.kind, ReportKind::WaypointChange);
    assert!(close(r.lat, 51.5665627, 1e-6));
    assert!(close(r.lon, 19.2610931, 1e-6));
    assert_eq!(r.alt_ft, 36000);

    let AdscTag::PredictedRoute {
        next_lat, next_lon, next_alt_ft, next_eta_s, next_next_lat, ..
    } = &tags[1]
    else {
        panic!("expected predicted route: {tags:?}")
    };
    assert!(close(*next_lat, 51.5190125, 1e-6));
    assert!(close(*next_lon, 19.0030861, 1e-6));
    assert_eq!(*next_alt_ft, 36000);
    assert_eq!(*next_eta_s, 74);
    assert!(close(*next_next_lat, 51.3298416, 1e-6));
}

#[test]
fn sp_lrh_low_altitude_approach() {
    let tags = adsc_tags(
        "/YQXE2YA.ADS.SP-LRH1424FD087806C0B527769F0D2500B877ED00B5401E2516707755C01340B768",
    );
    let AdscTag::Report(r) = &tags[0] else { panic!() };
    assert_eq!(r.kind, ReportKind::WaypointChange);
    assert!(close(r.lat, 52.0149422, 1e-6));
    assert!(close(r.lon, 21.0983849, 1e-6));
    assert_eq!(r.alt_ft, 2896);
    let AdscTag::PredictedRoute { next_next_alt_ft, .. } = &tags[1] else { panic!() };
    assert_eq!(*next_next_alt_ft, 308);
}

#[test]
fn h1_sublabel_then_plain_text() {
    let d = decode("H1", "#DFB/M1 ENGINE DATA", true);
    assert_eq!(d.sublabel.as_deref(), Some("DF"));
    assert_eq!(d.mfi.as_deref(), Some("M1"));
    assert!(d.app.is_none());
}

// --- ACARS-1.1: Q-series link-test / squitter classification ---
// Reference strings are the real documented examples from airframes'
// acars-message-documentation (research/Q0.md, Q2.md, QF.md, QQ.md) and the
// descriptions from airframes' own acars-decoder-typescript plugins.

#[test]
fn q0_link_test_classified() {
    use xng_acars::qseries::QKind;
    // research/Q0.md: "ACARS Link Test", messages are always empty.
    let d = decode("Q0", "", true);
    let Some(AcarsApp::QSeries(q)) = d.app else { panic!("expected Q-series: {:?}", d.app) };
    assert_eq!(q.kind, QKind::LinkTest);
    assert_eq!(q.description, "ACARS Link Test");
}

#[test]
fn q2_eta_report_classified() {
    use xng_acars::qseries::QKind;
    // research/Q2.md example: "   2002  99/DS KJFK" — ETA Report.
    let d = decode("Q2", "   2002  99/DS KJFK", true);
    let Some(AcarsApp::QSeries(q)) = d.app else { panic!("expected Q-series") };
    assert_eq!(q.kind, QKind::EtaReport);
    assert_eq!(q.description, "ETA Report");
}

#[test]
fn qf_off_destination_report_classified() {
    // research/QF.md example: "EWR2210ATL" — OFF Destination Report.
    let d = decode("QF", "EWR2210ATL", true);
    let Some(AcarsApp::QSeries(q)) = d.app else { panic!("expected Q-series") };
    assert_eq!(q.description, "OFF Destination Report");
}

#[test]
fn qq_off_report_classified() {
    // research/QQ.md example: "KEWRKSWF20041942" — OFF Report.
    let d = decode("QQ", "KEWRKSWF20041942", true);
    let Some(AcarsApp::QSeries(q)) = d.app else { panic!("expected Q-series") };
    assert_eq!(q.description, "OFF Report");
}

// --- ACARS-2.1: OOOI (OUT/OFF/ON/IN) text extraction ---
// Offsets/semantics from f00b4r0/acarsdec label.c; reference example
// strings (and the airports they encode) from airframes'
// acars-message-documentation.

#[test]
fn qq_off_report_oooi_fields() {
    // research/QQ.md: "KEWRKSWF20041942" — KEWR → KSWF, OFF 20:04.
    let d = decode("QQ", "KEWRKSWF20041942", true);
    let o = d.oooi.expect("QQ carries OOOI");
    assert_eq!(o.depa.as_deref(), Some("KEWR"));
    assert_eq!(o.dsta.as_deref(), Some("KSWF"));
    assert_eq!(o.wloff.as_deref(), Some("2004"));
}

#[test]
fn qq_off_report_with_status_tail() {
    // research/QQ.md: "KEWRKDFW1829OS KDFW ..." — KEWR → KDFW, OFF 18:29.
    let d = decode("QQ", "KEWRKDFW1829OS KDFW /FUL0306/MO 1816/APH 0000000", true);
    let o = d.oooi.expect("QQ carries OOOI");
    assert_eq!(o.depa.as_deref(), Some("KEWR"));
    assert_eq!(o.dsta.as_deref(), Some("KDFW"));
    assert_eq!(o.wloff.as_deref(), Some("1829"));
}

#[test]
fn non_oooi_label_has_no_oooi() {
    // ADS-C envelopes are not OOOI text; the field stays absent.
    let d = decode(
        "B6",
        "/BOMASAI.ADS.VT-ANB072501A070A988CA73248F0E5DC10200000F5EE1ABC000102B885E0A19F5",
        true,
    );
    assert!(d.oooi.is_none());
}
