//! modulate → DmeChannelDecoder loopback tests.

use num_complex::Complex;
use xng_dsp::Nco;
use xng_mode_dme::channels::{DmeChannel, PairedKind, Role};
use xng_mode_dme::detect::{PulsePair, Thresholds};
use xng_mode_dme::modulate::{generate, score, SimConfig};
use xng_mode_dme::{DmeChannelDecoder, DmeEvent, IdentReport, StatusReport};

fn run(
    iq: &[Complex<f32>],
    rate: f64,
    offset: f64,
    carrier: Option<u64>,
) -> (Vec<PulsePair>, Vec<DmeEvent>) {
    let th = Thresholds::default();
    let mut d = DmeChannelDecoder::with_thresholds(rate, offset, th).unwrap();
    if let Some(c) = carrier {
        d = d.with_carrier(c, th);
    }
    let mut pairs = Vec::new();
    let mut ev = Vec::new();
    for chunk in iq.chunks(50_000) {
        ev.extend(d.process_with_pairs(chunk, &mut pairs));
    }
    ev.extend(d.finish(Some(&mut pairs)));
    (pairs, ev)
}

fn idents(ev: &[DmeEvent]) -> Vec<&IdentReport> {
    ev.iter()
        .filter_map(|e| {
            if let DmeEvent::Ident(r) = e {
                Some(r)
            } else {
                None
            }
        })
        .collect()
}

fn statuses(ev: &[DmeEvent]) -> Vec<&StatusReport> {
    ev.iter()
        .filter_map(|e| {
            if let DmeEvent::Status(s) = e {
                Some(s)
            } else {
                None
            }
        })
        .collect()
}

/// Shift a baseband signal to `offset` Hz inside the capture.
fn shift(iq: &mut [Complex<f32>], offset: f64, rate: f64) {
    Nco::new(-offset, rate).mix(iq);
}

#[test]
fn detects_x_pairs_through_the_ddc() {
    // 2.4 MS/s RTL-style capture, channel 400 kHz off centre.
    let rate = 2_400_000.0;
    let cfg = SimConfig {
        sample_rate: rate,
        duration_s: 2.0,
        snr_db: 15.0,
        seed: 3,
        ..Default::default()
    };
    let mut sim = generate(&cfg);
    shift(&mut sim.iq, 400_000.0, rate);
    let ch: DmeChannel = "77X".parse().unwrap();
    let (pairs, _) = run(&sim.iq, rate, 400_000.0, Some(ch.reply_hz()));
    let t: Vec<f64> = pairs.iter().map(|p| p.t_us).collect();
    let (tp, fp) = score(&sim.pairs, &t, 1.5);
    let pd = tp as f64 / sim.pairs.len() as f64;
    assert!(pd > 0.99, "Pd {pd}");
    assert!(fp < 10, "false pairs {fp}");
    assert!(pairs.iter().all(|p| p.spacing_us == 12));
}

#[test]
fn y_reply_uses_30_us_and_names_the_channel() {
    let rate = 2_000_000.0;
    let ch: DmeChannel = "17Y".parse().unwrap();
    let cfg = SimConfig {
        sample_rate: rate,
        duration_s: 11.0,
        spacing_us: 30.0,
        snr_db: 15.0,
        ident: Some("ABC".into()),
        seed: 4,
        ..Default::default()
    };
    let sim = generate(&cfg);
    let (pairs, ev) = run(&sim.iq, rate, 0.0, Some(ch.reply_hz()));
    let t: Vec<f64> = pairs.iter().map(|p| p.t_us).collect();
    let (tp, _) = score(&sim.pairs, &t, 1.5);
    assert!(tp as f64 / sim.pairs.len() as f64 > 0.99);
    let id = idents(&ev);
    assert_eq!(id.len(), 1, "{id:?}");
    assert_eq!(id[0].ident, "ABC");
    assert_eq!(id[0].link.channel.as_deref(), Some("17Y"));
    assert_eq!(id[0].link.role, Some(Role::Reply));
    assert_eq!(id[0].link.paired_vhf_hz, Some(108_050_000));
    assert_eq!(id[0].link.paired_kind, Some(PairedKind::Vor)); // 108.05: even tenth
    let st = statuses(&ev);
    assert_eq!(st.len(), 1);
    assert_eq!(st[0].link.spacing_us, 30);
    assert!((st[0].random_pps - 2700.0).abs() < 300.0, "{:?}", st[0]);
}

#[test]
fn decodes_idents_of_every_shape() {
    // Mixed letters, all dashes, all dots, a digit, four letters, and the
    // dot-length extremes of Annex 10 (0.1 and 0.16 s).
    for (i, (text, dot)) in [
        ("SFO", 125.0),
        ("MOM", 125.0),
        ("SHE", 100.0),
        ("IBOS", 160.0),
        ("OAK", 160.0),
        ("LA2", 110.0),
    ]
    .iter()
    .enumerate()
    {
        let cfg = SimConfig {
            sample_rate: 1_000_000.0,
            duration_s: 9.0,
            snr_db: 10.0,
            ident: Some(text.to_string()),
            dot_ms: *dot,
            seed: 10 + i as u64,
            ..Default::default()
        };
        let sim = generate(&cfg);
        let (_, ev) = run(&sim.iq, 1_000_000.0, 0.0, Some(1_151_000_000));
        let id = idents(&ev);
        assert_eq!(id.len(), 1, "{text}: {id:?}");
        assert_eq!(id[0].ident, *text);
        assert!(
            (id[0].dot_ms - *dot as f32).abs() < 25.0,
            "{text}: dot {}",
            id[0].dot_ms
        );
        assert!((id[0].ident_rate_hz - 1350.0).abs() < 2.0);
    }
}

#[test]
fn ident_with_replies_suppressed_and_drifted_clock() {
    let cfg = SimConfig {
        sample_rate: 1_000_000.0,
        duration_s: 8.0,
        snr_db: 10.0,
        ident: Some("PDX".into()),
        suppress_random_during_ident: true,
        // 1355 pp/s: 0.4 % off nominal.
        ident_period_us: 1e6 / 1355.0,
        seed: 21,
        ..Default::default()
    };
    let sim = generate(&cfg);
    let (_, ev) = run(&sim.iq, 1_000_000.0, 0.0, None);
    let id = idents(&ev);
    assert_eq!(id.len(), 1, "{id:?}");
    assert_eq!(id[0].ident, "PDX");
    assert!((id[0].ident_rate_hz - 1355.0).abs() < 1.0);
}

#[test]
fn ident_survives_low_snr() {
    // 5 dB peak SNR in 1 MHz: about a third of the pairs are detected.
    let cfg = SimConfig {
        sample_rate: 2_000_000.0,
        duration_s: 8.0,
        snr_db: 5.0,
        ident: Some("SFO".into()),
        seed: 5,
        ..Default::default()
    };
    let sim = generate(&cfg);
    let (_, ev) = run(&sim.iq, 2_000_000.0, 0.0, Some(1_151_000_000));
    let id = idents(&ev);
    assert_eq!(id.first().map(|r| r.ident.as_str()), Some("SFO"), "{id:?}");
}

#[test]
fn noise_only_is_quiet() {
    let cfg = SimConfig {
        sample_rate: 1_000_000.0,
        duration_s: 20.0,
        random_pps: 0.0,
        seed: 6,
        ..Default::default()
    };
    let sim = generate(&cfg);
    let (pairs, ev) = run(&sim.iq, 1_000_000.0, 0.0, None);
    // Three spacings searched: under one false pair per second in total.
    assert!(pairs.len() < 20, "{} false pairs in 20 s", pairs.len());
    assert!(idents(&ev).is_empty());
    assert!(statuses(&ev).is_empty());
}

#[test]
fn status_reports_rate_and_ident_pairs() {
    let ch: DmeChannel = "77X".parse().unwrap();
    let cfg = SimConfig {
        sample_rate: 1_000_000.0,
        duration_s: 10.5,
        snr_db: 15.0,
        ident: Some("SFO".into()),
        seed: 8,
        ..Default::default()
    };
    let sim = generate(&cfg);
    let (_, ev) = run(&sim.iq, 1_000_000.0, 0.0, Some(ch.reply_hz()));
    let st = statuses(&ev);
    assert_eq!(st.len(), 1);
    let s = st[0];
    assert_eq!(s.link.channel.as_deref(), Some("77X"));
    assert_eq!(s.link.paired_vhf_hz, Some(113_000_000));
    assert_eq!(s.link.paired_kind, Some(PairedKind::Vor));
    // SFO keys 2.25 s at 1350 pp/s over 10 s ≈ 300 pp/s of ident.
    assert!((s.ident_pps - 300.0).abs() < 60.0, "{s:?}");
    assert!((s.random_pps - 2700.0).abs() < 250.0, "{s:?}");
    assert!(!s.tacan);
    assert_eq!(s.last_ident.as_deref(), Some("SFO"));
}

#[test]
fn tacan_reference_bursts() {
    let cfg = SimConfig {
        sample_rate: 1_000_000.0,
        duration_s: 10.5,
        snr_db: 15.0,
        tacan: true,
        seed: 9,
        ..Default::default()
    };
    let sim = generate(&cfg);
    let (_, ev) = run(&sim.iq, 1_000_000.0, 0.0, Some(1_151_000_000));
    let st = statuses(&ev);
    assert_eq!(st.len(), 1);
    assert!(st[0].tacan, "{:?}", st[0]);
    assert!(
        (st[0].reference_bursts_per_s - 15.0).abs() < 3.0,
        "{:?}",
        st[0]
    );

    let plain = generate(&SimConfig {
        tacan: false,
        ..cfg
    });
    let (_, ev) = run(&plain.iq, 1_000_000.0, 0.0, Some(1_151_000_000));
    assert!(!statuses(&ev)[0].tacan);
}

#[test]
fn messages_serialize() {
    let cfg = SimConfig {
        sample_rate: 1_000_000.0,
        duration_s: 10.5,
        snr_db: 15.0,
        ident: Some("SFO".into()),
        seed: 12,
        ..Default::default()
    };
    let sim = generate(&cfg);
    let (_, ev) = run(&sim.iq, 1_000_000.0, 0.0, Some(1_151_000_000));
    for e in &ev {
        let source = xng_types::Provenance {
            station: xng_types::StationIdentity::new("TEST"),
            app: xng_types::AppInfo::xng(),
            sdr: None,
            channel: None,
        };
        let m = xng_mode_dme::to_message(e, 1_151_000_000, -30.0, source);
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["mode"], "dme");
        assert_eq!(j["body"]["details"]["channel"], "64X");
    }
}
