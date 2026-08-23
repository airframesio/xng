//! End-to-end loopback + synthetic-AWGN demod validation for the CHU and
//! WWV/WWVH time-signal decoders.
//!
//! VERIFICATION POSTURE (the project mandate):
//!
//! 1. The DECODE cores (CHU BCD/redundancy, WWV BCD/IRIG-H framing) are
//!    anchored by the published broadcast formats in the crate's `#[test]`
//!    table tests (`src/chu.rs`, `src/wwv.rs`).
//! 2. The DEMOD path is validated here by a SELF-GENERATED `modulate → [AWGN]
//!    → demod` loopback: synthesize a known UTC's audio, AM-modulate to IQ, run
//!    it through the real `TimeChannelDecoder`, and assert the recovered UTC
//!    equals the input AND the validity gate passes. The modulator is not an
//!    external reference (see modulate.rs); no off-air IQ exists, so no real-RF
//!    claim is made — the DSC/EOT/ATCS posture. Tests are named `*_synth`.

use num_complex::Complex;
use xng_mode_time::{chu, modulate, wwv::Symbol, TimeChannelDecoder, TimeDecoder, CHANNEL_RATE};

// ---------------------------------------------------------------------------
// Deterministic complex AWGN (LCG + Box-Muller), the EOT/sonde bench pattern.
// ---------------------------------------------------------------------------

struct Lcg(u64);
impl Lcg {
    fn next_f32(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let x = (self.0 >> 16) as u32;
        ((x as f64 + 1.0) / (u32::MAX as f64 + 2.0)) as f32
    }
}

fn add_awgn(iq: &mut [Complex<f32>], sigma: f32, seed: u64) {
    let mut rng = Lcg(seed);
    for s in iq.iter_mut() {
        let u1 = rng.next_f32().max(1e-9);
        let u2 = rng.next_f32();
        let mag = sigma * (-2.0 * u1.ln()).sqrt();
        let n_re = mag * (std::f32::consts::TAU * u2).cos();
        let n_im = mag * (std::f32::consts::TAU * u2).sin();
        *s += Complex::new(n_re, n_im);
    }
}

// ---------------------------------------------------------------------------
// CHU helpers.
// ---------------------------------------------------------------------------

/// A Format A (time-of-day) 10-byte packet for a known UTC.
fn chu_format_a(doy: u16, h: u8, m: u8, s: u8) -> [u8; chu::PACKET_BYTES] {
    let bcd2 = |hi: u8, lo: u8| (hi << 4) | lo;
    let data = [
        bcd2(6, (doy / 100) as u8),
        bcd2(((doy / 10) % 10) as u8, (doy % 10) as u8),
        bcd2(h / 10, h % 10),
        bcd2(m / 10, m % 10),
        bcd2(s / 10, s % 10),
    ];
    [
        data[0], data[1], data[2], data[3], data[4], // data
        data[0], data[1], data[2], data[3], data[4], // exact copy
    ]
}

// ---------------------------------------------------------------------------
// CHU loopback.
// ---------------------------------------------------------------------------

#[test]
fn chu_format_a_loopback_synth() {
    // Known UTC: day-of-year 159, 12:34:56.
    let pkt = chu_format_a(159, 12, 34, 56);
    let iq = modulate::chu_iq(&pkt, CHANNEL_RATE, 0.0, 0.9);

    let mut dec = TimeChannelDecoder::new(CHANNEL_RATE, 0.0).expect("decoder");
    dec.set_decoder(TimeDecoder::Chu);
    let frames = dec.process(&iq);

    let f = frames
        .iter()
        .find(|f| f.valid && f.hour.is_some())
        .expect("a validated CHU Format A frame");
    assert_eq!(f.station, "CHU");
    assert_eq!(f.day_of_year, Some(159));
    assert_eq!(f.hour, Some(12));
    assert_eq!(f.minute, Some(34));
    assert_eq!(f.second, Some(56));
    assert!(f.valid, "redundancy gate must pass");
    assert!(dec.level_dbfs().is_finite());
}

#[test]
fn chu_loopback_through_ddc_offset_synth() {
    // Transmit offset from the capture center and at a higher capture rate so
    // the internal Ddc must mix + decimate (integer factor 4 → 48 kHz).
    let pkt = chu_format_a(200, 23, 59, 58);
    let capture_rate = 48_000.0;
    let offset = 8_000.0;
    let iq = modulate::chu_iq(&pkt, capture_rate, offset, 0.8);

    let mut dec = TimeChannelDecoder::new(capture_rate, offset).expect("decoder");
    dec.set_decoder(TimeDecoder::Chu);
    let frames = dec.process(&iq);

    let f = frames
        .iter()
        .find(|f| f.valid && f.hour.is_some())
        .expect("CHU frame after DDC mix/decimate");
    assert_eq!(f.day_of_year, Some(200));
    assert_eq!(f.hour, Some(23));
    assert_eq!(f.minute, Some(59));
    assert_eq!(f.second, Some(58));
}

#[test]
fn chu_synthetic_awgn_recovery() {
    // SYNTHETIC demod metric: modulate a known Format A second, add complex
    // AWGN at a moderate SNR, demod, and require the redundancy-validated UTC
    // back. The redundancy gate makes this strict: a residual bit error fails
    // it. Run several seeds and require a healthy success fraction.
    let pkt = chu_format_a(77, 6, 15, 30);
    let amp = 0.9f32;
    let sigma = 0.12f32; // ~ +12 dB SNR per I/Q against the 0.9 carrier

    let trials = 10;
    let mut recovered = 0;
    for seed in 0..trials {
        let mut iq = modulate::chu_iq(&pkt, CHANNEL_RATE, 0.0, amp);
        add_awgn(&mut iq, sigma, 0xC0FFEE + seed * 0x9E37);
        let mut dec = TimeChannelDecoder::new(CHANNEL_RATE, 0.0).expect("decoder");
        dec.set_decoder(TimeDecoder::Chu);
        let frames = dec.process(&iq);
        if let Some(f) = frames.iter().find(|f| f.valid && f.hour.is_some()) {
            if f.day_of_year == Some(77) && f.hour == Some(6) && f.minute == Some(15) && f.second == Some(30) {
                recovered += 1;
            }
        }
    }
    assert!(
        recovered as f64 / trials as f64 >= 0.7,
        "CHU AWGN recovery too low: {recovered}/{trials}"
    );
}

// ---------------------------------------------------------------------------
// WWV helpers + loopback.
// ---------------------------------------------------------------------------

/// Build a 60-symbol WWV minute for a known UTC (mirrors the in-crate test
/// builder but lives here for the IQ loopback).
fn wwv_minute(year: u16, doy: u16, hour: u8, minute: u8, dut1_tenths: i8) -> Vec<Symbol> {
    let mut s = vec![Symbol::Zero; 60];
    s[0] = Symbol::Hole;
    for &m in &[9usize, 19, 29, 39, 49, 59] {
        s[m] = Symbol::Marker;
    }
    let set = |s: &mut [Symbol], sec: usize, on: bool| {
        s[sec] = if on { Symbol::One } else { Symbol::Zero };
    };
    let put = |s: &mut [Symbol], secs: &[usize], mut v: u16| {
        for &sec in secs {
            set(s, sec, v & 1 == 1);
            v >>= 1;
        }
    };
    put(&mut s, &[10, 11, 12, 13], minute as u16 % 10);
    put(&mut s, &[15, 16, 17], minute as u16 / 10);
    put(&mut s, &[20, 21, 22, 23], hour as u16 % 10);
    put(&mut s, &[25, 26], hour as u16 / 10);
    put(&mut s, &[30, 31, 32, 33], doy % 10);
    put(&mut s, &[35, 36, 37, 38], (doy / 10) % 10);
    put(&mut s, &[40, 41], doy / 100);
    let yy = year - 2000;
    put(&mut s, &[4, 5, 6, 7], yy % 10);
    put(&mut s, &[51, 52, 53, 54], yy / 10);
    set(&mut s, 50, dut1_tenths >= 0);
    put(&mut s, &[56, 57, 58], dut1_tenths.unsigned_abs() as u16);
    s
}

#[test]
fn wwv_loopback_synth() {
    // 2026, day 159, 12:34 UTC, DUT1 +0.3 s, WWV (1000 Hz tick).
    let symbols = wwv_minute(2026, 159, 12, 34, 3);
    let iq = modulate::wwv_iq(&symbols, 1000.0, CHANNEL_RATE, 0.0, 0.9);

    let mut dec = TimeChannelDecoder::new(CHANNEL_RATE, 0.0).expect("decoder");
    dec.set_decoder(TimeDecoder::Wwv);
    let frames = dec.process(&iq);

    let f = frames.iter().find(|f| f.valid).expect("a synced WWV frame");
    assert_eq!(f.year, Some(2026));
    assert_eq!(f.day_of_year, Some(159));
    assert_eq!(f.hour, Some(12));
    assert_eq!(f.minute, Some(34));
    assert!((f.dut1_s.unwrap() - 0.3).abs() < 1e-3);
    // Full UTC built (second = 0 at the minute mark).
    assert_eq!(
        f.utc.map(|u| u.to_rfc3339()),
        Some("2026-06-08T12:34:00+00:00".to_string())
    );
    assert_eq!(f.station, "WWV");
}

#[test]
fn wwvh_station_labelled_from_tick() {
    // Identical time code, but a 1200 Hz tick → WWVH.
    let symbols = wwv_minute(2025, 1, 0, 0, 0);
    let iq = modulate::wwv_iq(&symbols, 1200.0, CHANNEL_RATE, 0.0, 0.9);

    let mut dec = TimeChannelDecoder::new(CHANNEL_RATE, 0.0).expect("decoder");
    dec.set_decoder(TimeDecoder::Wwv);
    let frames = dec.process(&iq);
    let f = frames.iter().find(|f| f.valid).expect("synced WWVH frame");
    assert_eq!(f.station, "WWVH");
    assert_eq!(f.year, Some(2025));
    assert_eq!(f.day_of_year, Some(1));
}

#[test]
fn wwv_synthetic_awgn_recovery() {
    // SYNTHETIC demod metric for the 100 Hz BCD path: modulate a full minute,
    // add complex AWGN, and require the synced UTC back. Frame sync (the marker
    // grid + sec-0 hole) plus the field range checks gate this.
    let symbols = wwv_minute(2024, 300, 18, 45, -2);
    let amp = 0.9f32;
    let sigma = 0.10f32;

    let trials = 6;
    let mut recovered = 0;
    for seed in 0..trials {
        let mut iq = modulate::wwv_iq(&symbols, 1000.0, CHANNEL_RATE, 0.0, amp);
        add_awgn(&mut iq, sigma, 0xBEEF + seed * 0x9E37);
        let mut dec = TimeChannelDecoder::new(CHANNEL_RATE, 0.0).expect("decoder");
        dec.set_decoder(TimeDecoder::Wwv);
        let frames = dec.process(&iq);
        if let Some(f) = frames.iter().find(|f| f.valid) {
            if f.year == Some(2024)
                && f.day_of_year == Some(300)
                && f.hour == Some(18)
                && f.minute == Some(45)
            {
                recovered += 1;
            }
        }
    }
    assert!(
        recovered as f64 / trials as f64 >= 0.7,
        "WWV AWGN recovery too low: {recovered}/{trials}"
    );
}

// ---------------------------------------------------------------------------
// to_message mapping.
// ---------------------------------------------------------------------------

#[test]
fn to_message_emits_time_body_synth() {
    use xng_types::{AppInfo, MessageBody, Mode, Provenance, StationIdentity};

    let symbols = wwv_minute(2026, 159, 12, 34, 3);
    let iq = modulate::wwv_iq(&symbols, 1000.0, CHANNEL_RATE, 0.0, 0.9);
    let mut dec = TimeChannelDecoder::new(CHANNEL_RATE, 0.0).expect("decoder");
    dec.set_decoder(TimeDecoder::Wwv);
    let frames = dec.process(&iq);
    let f = frames.iter().find(|f| f.valid).expect("synced frame");

    let source = Provenance {
        station: StationIdentity::new("TEST-TIME"),
        app: AppInfo::xng(),
        sdr: None,
        channel: None,
    };
    let msg = xng_mode_time::to_message(f, 10_000_000, dec.level_dbfs(), source);

    assert_eq!(msg.mode, Mode::Time);
    assert_eq!(msg.frequency_hz, 10_000_000);
    assert!(msg.decode.crc_ok, "validated frame sets crc_ok");
    assert_eq!(msg.signal.rssi_db, Some(dec.level_dbfs()));
    match &msg.body {
        MessageBody::Time { station, details } => {
            assert_eq!(station, "WWV");
            assert_eq!(details["utc"], "2026-06-08T12:34:00+00:00");
            assert_eq!(details["year"], 2026);
            assert_eq!(details["day_of_year"], 159);
            assert_eq!(details["valid"], true);
        }
        other => panic!("expected MessageBody::Time, got {other:?}"),
    }
}
