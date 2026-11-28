//! Generator → decoder loopback: every measurement the decoder reports is
//! checked against the synthetic signal it was given.

use xng_mode_navaid::modulate::{
    CarrierSpec, Generator, IdentSpec, IlsParams, Scene, Signal, VorParams,
};
use xng_mode_navaid::{NavaidChannelDecoder, NavaidKind, NavaidObservation, NavaidStatus};

/// A low input rate keeps the tests fast; the channelizer is the same at
/// any rate.
const RATE: f64 = 192_000.0;
const OFFSET: f64 = 30_000.0;

fn run(kind: NavaidKind, scene: Scene, seconds: f64, report_s: f64) -> Vec<NavaidObservation> {
    let mut g = Generator::new(scene);
    let mut d = NavaidChannelDecoder::new(kind, RATE, OFFSET).unwrap();
    d.set_report_interval(report_s);
    let mut out = Vec::new();
    let block = (RATE / 10.0) as usize;
    for _ in 0..(seconds * 10.0) as usize {
        out.extend(d.process(&g.next_block(block)));
    }
    out
}

fn angle_err(a: f64, b: f64) -> f64 {
    ((a - b + 540.0) % 360.0 - 180.0).abs()
}

fn vor_scene(p: VorParams, cn0: Option<f64>) -> Scene {
    Scene::single(RATE, OFFSET, Signal::Vor(p), cn0)
}

#[test]
fn vor_radials_all_around() {
    for radial in [0.0, 45.0, 123.0, 180.0, 251.0, 338.0, 359.5] {
        let p = VorParams {
            radial_deg: radial,
            ..Default::default()
        };
        let obs = run(NavaidKind::Vor, vor_scene(p, Some(50.0)), 12.0, 4.0);
        let last = obs.last().unwrap();
        let v = last.vor.as_ref().expect("vor measure");
        assert!(
            angle_err(v.radial_deg, radial) < 1.0,
            "radial {radial}: got {}",
            v.radial_deg
        );
        assert!((v.mod30_pct - 30.0).abs() < 2.0, "mod30 {}", v.mod30_pct);
        assert!(
            (v.mod9960_pct - 30.0).abs() < 3.0,
            "mod9960 {}",
            v.mod9960_pct
        );
    }
}

#[test]
fn vor_deviation_on_a_clean_signal() {
    // The discriminator reading is a diagnostic for strong signals only.
    let obs = run(
        NavaidKind::Vor,
        vor_scene(
            VorParams {
                radial_deg: 300.0,
                ..Default::default()
            },
            None,
        ),
        8.0,
        4.0,
    );
    let v = obs.last().unwrap().vor.as_ref().unwrap();
    assert!(
        (v.deviation_hz - 480.0).abs() < 5.0,
        "deviation {}",
        v.deviation_hz
    );
    assert!(
        angle_err(v.radial_deg, 300.0) < 0.3,
        "radial {}",
        v.radial_deg
    );
}

#[test]
fn vor_tolerates_subcarrier_error_dvor_and_voice() {
    let p = VorParams {
        radial_deg: 77.0,
        subcarrier_hz: 9_960.0 + 40.0,
        dvor_asymmetry: 0.1,
        voice_depth: 0.3,
        ..Default::default()
    };
    let obs = run(NavaidKind::Vor, vor_scene(p, Some(45.0)), 16.0, 4.0);
    let v = obs.last().unwrap().vor.as_ref().expect("vor measure");
    assert!(angle_err(v.radial_deg, 77.0) < 2.0, "got {}", v.radial_deg);
    assert!(
        (v.subcarrier_offset_hz - 40.0).abs() < 3.0,
        "subcarrier offset {}",
        v.subcarrier_offset_hz
    );
}

#[test]
fn vor_carrier_offset_is_measured() {
    let p = VorParams {
        radial_deg: 200.0,
        ..Default::default()
    };
    let scene = Scene::single(RATE, OFFSET + 2_500.0, Signal::Vor(p), Some(50.0));
    let obs = run(NavaidKind::Vor, scene, 10.0, 5.0);
    let last = obs.last().unwrap();
    assert!(
        (last.carrier_offset_hz.unwrap() - 2_500.0).abs() < 5.0,
        "{:?}",
        last.carrier_offset_hz
    );
    assert!(angle_err(last.vor.as_ref().unwrap().radial_deg, 200.0) < 1.0);
}

#[test]
fn no_signal_reports_nothing_measured() {
    let scene = Scene {
        sample_rate: RATE,
        carriers: vec![],
        cn0_dbhz: None,
        seed: 1,
    };
    // Pure noise: give the generator a vanishing carrier so it adds noise.
    let mut scene = scene;
    scene.carriers.push(CarrierSpec {
        offset_hz: OFFSET,
        amplitude: 1e-6,
        phase_rad: 0.0,
        signal: Signal::Vor(VorParams::default()),
    });
    scene.cn0_dbhz = Some(-60.0);
    let obs = run(NavaidKind::Vor, scene, 10.0, 5.0);
    for o in &obs {
        assert_eq!(o.status, NavaidStatus::NoSignal);
        assert!(o.vor.is_none() && o.ident.is_none());
    }
}

#[test]
fn vor_ident_decodes() {
    let mut id = IdentSpec::standard("SAC");
    id.start_s = 1.0;
    let p = VorParams {
        radial_deg: 10.0,
        ident: Some(id),
        ..Default::default()
    };
    let obs = run(NavaidKind::Vor, vor_scene(p, Some(50.0)), 40.0, 10.0);
    let last = obs.last().unwrap();
    assert_eq!(last.ident.as_deref(), Some("SAC"));
    assert_eq!(last.status, NavaidStatus::Normal);
    // Ident depth 10 %, tone 1020 Hz.
    assert!(
        (last.ident_depth_pct.unwrap() - 10.0).abs() < 3.0,
        "{:?}",
        last.ident_depth_pct
    );
    assert!(
        (last.ident_tone_hz.unwrap() - 1020.0).abs() < 2.0,
        "{:?}",
        last.ident_tone_hz
    );
}

#[test]
fn weak_ident_is_never_wrong() {
    // Below the ident threshold the decoder must say nothing, not guess.
    for (cn0, seed) in [(30.0, 1), (33.0, 2), (36.0, 3), (36.0, 4)] {
        let mut id = IdentSpec::standard("OAK");
        id.start_s = 1.0;
        let p = VorParams {
            radial_deg: 10.0,
            ident: Some(id),
            ..Default::default()
        };
        let mut scene = vor_scene(p, Some(cn0));
        scene.seed = seed;
        for o in run(NavaidKind::Vor, scene, 60.0, 10.0) {
            if let Some(w) = &o.ident {
                assert_eq!(w, "OAK", "wrong ident at {cn0} dB-Hz");
            }
        }
    }
}

fn ils_run(kind: NavaidKind, p: IlsParams, cn0: f64) -> Vec<NavaidObservation> {
    run(
        kind,
        Scene::single(RATE, OFFSET, Signal::Ils(p), Some(cn0)),
        8.0,
        4.0,
    )
}

#[test]
fn localizer_ddm() {
    for ddm in [-0.155, -0.05, 0.0, 0.02, 0.1] {
        let obs = ils_run(NavaidKind::Localizer, IlsParams::localizer(ddm), 60.0);
        let m = &obs.last().unwrap().ils[0];
        assert!((m.ddm - ddm).abs() < 0.003, "ddm {ddm}: got {}", m.ddm);
        assert!((m.sdm_pct - 40.0).abs() < 1.5, "sdm {}", m.sdm_pct);
    }
}

#[test]
fn glideslope_ddm() {
    for ddm in [-0.0875, 0.0, 0.04, 0.175] {
        let obs = ils_run(NavaidKind::Glideslope, IlsParams::glideslope(ddm), 60.0);
        let m = &obs.last().unwrap().ils[0];
        assert!((m.ddm - ddm).abs() < 0.003, "ddm {ddm}: got {}", m.ddm);
        assert!((m.sdm_pct - 80.0).abs() < 2.5, "sdm {}", m.sdm_pct);
    }
}

#[test]
fn two_frequency_localizer_reports_both_carriers() {
    let course = CarrierSpec {
        offset_hz: OFFSET + 4_000.0,
        amplitude: 0.25,
        phase_rad: 0.7,
        signal: Signal::Ils(IlsParams::localizer(0.03)),
    };
    let clearance = CarrierSpec {
        offset_hz: OFFSET - 4_000.0,
        amplitude: 0.25 * 10f64.powf(-8.0 / 20.0),
        phase_rad: 2.1,
        signal: Signal::Ils(IlsParams::localizer(0.3)),
    };
    let scene = Scene {
        sample_rate: RATE,
        carriers: vec![course, clearance],
        cn0_dbhz: Some(50.0),
        seed: 9,
    };
    let obs = run(NavaidKind::Localizer, scene, 10.0, 5.0);
    let ils = &obs.last().unwrap().ils;
    assert_eq!(ils.len(), 2, "{ils:?}");
    // Strongest first: the course carrier.
    assert!((ils[0].carrier_offset_hz - 4_000.0).abs() < 5.0);
    assert!((ils[0].ddm - 0.03).abs() < 0.003, "course {}", ils[0].ddm);
    assert!((ils[1].carrier_offset_hz + 4_000.0).abs() < 5.0);
    assert!((ils[1].ddm - 0.3).abs() < 0.01, "clearance {}", ils[1].ddm);
    assert!(
        (ils[1].relative_db + 8.0).abs() < 1.0,
        "relative {}",
        ils[1].relative_db
    );
}

#[test]
fn localizer_ident_decodes() {
    let mut p = IlsParams::localizer(0.0);
    let mut id = IdentSpec::standard("ISAC");
    id.start_s = 1.0;
    p.ident = Some(id);
    let obs = run(
        NavaidKind::Localizer,
        Scene::single(RATE, OFFSET, Signal::Ils(p), Some(50.0)),
        40.0,
        10.0,
    );
    assert_eq!(obs.last().unwrap().ident.as_deref(), Some("ISAC"));
}

#[test]
fn cn0_estimate_is_calibrated() {
    for cn0 in [35.0, 45.0, 55.0] {
        let obs = ils_run(NavaidKind::Localizer, IlsParams::localizer(0.0), cn0);
        let got = obs.last().unwrap().cn0_dbhz.unwrap();
        assert!((got - cn0).abs() < 1.0, "C/N0 {cn0}: got {got:.2}");
    }
}
