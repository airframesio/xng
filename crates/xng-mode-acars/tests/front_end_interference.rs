//! Front-end selectivity regressions (PERF-20): a weak channel next to a
//! strong neighbour, through the decoder the daemon builds
//! ([`AcarsMultiChannelDecoder::new`]).
//!
//! The demod takes the AM envelope of whatever the front end hands it, so any
//! neighbour energy left in the 24 kHz channel stream beats against the weak
//! carrier. A front end that only anti-aliased (a critically sampled
//! channelizer bin, ±12 kHz wide) lost the weak channel completely in both
//! cases below, where the shared decimation (a real ±5 kHz channel filter)
//! kept decoding it. Signal generators match `examples/acars_interf.rs`.

use num_complex::Complex;
use std::f64::consts::TAU;
use xng_mode_acars::modulate::{burst_iq, FrameSpec};
use xng_mode_acars::AcarsMultiChannelDecoder;

const FS: f64 = 2_400_000.0;
const CHUNK: usize = 65_536;
const SIGMA: f32 = 0.01;
const WEAK_MHZ: f64 = 131.525;
const PLAN8: [f64; 8] = [131.425, 131.450, 131.475, 131.525, 131.550, 131.575, 131.725, 131.825];
const PLAN16: [f64; 16] = [
    130.425, 130.45, 130.55, 130.825, 130.85, 131.125, 131.25, 131.425, 131.45, 131.475, 131.525,
    131.55, 131.65, 131.725, 131.825, 131.85,
];
const VOICE_F: [f64; 8] = [310.0, 520.0, 780.0, 1130.0, 1490.0, 1870.0, 2350.0, 2950.0];
const VOICE_A: [f64; 8] = [0.2, 0.18, 0.15, 0.13, 0.11, 0.09, 0.08, 0.06];

/// xorshift64 + Box-Muller: deterministic, dependency-free noise.
struct Rng(u64);

impl Rng {
    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 2.0)
    }

    fn gauss_pair(&mut self) -> (f32, f32) {
        let (u1, u2) = (self.uniform(), self.uniform());
        let r = (-2.0 * u1.ln()).sqrt();
        ((r * (TAU * u2).cos()) as f32, (r * (TAU * u2).sin()) as f32)
    }
}

#[derive(Clone, Copy)]
enum Interferer {
    /// Back-to-back ACARS bursts.
    Acars,
    /// Band-limited multitone AM voice (8 tones 300–3000 Hz, 4 Hz syllabic
    /// envelope, m = 0.8).
    Voice,
}

struct Scenario<'a> {
    plan: &'a [f64],
    center_hz: f64,
    /// Weak AM carrier power over the AWGN power in ±5 kHz (dB).
    snr_db: f64,
    interferer: Interferer,
    /// Interferer frequency relative to the weak channel (Hz).
    delta_hz: f64,
    /// Interferer carrier power relative to the weak carrier (dB).
    level_db: f64,
    bursts: usize,
    seed: u64,
}

fn weak_spec(text: &str) -> FrameSpec<'_> {
    FrameSpec {
        mode: '2',
        tail: "N471XG",
        ack: None,
        label: "H1",
        block_id: '3',
        msg_num: Some("M42A"),
        flight: Some("XG0042"),
        text,
        etb: false,
    }
}

const STRONG_SPEC: FrameSpec<'static> = FrameSpec {
    mode: '2',
    tail: "N999ZZ",
    ack: None,
    label: "H1",
    block_id: '5',
    msg_num: Some("M01B"),
    flight: Some("ZZ0001"),
    text: "STRONG NEIGHBOUR CHANNEL TRAFFIC 0123456789",
    etb: false,
};

/// Weak-channel bursts decoded (CRC-OK, expected payload) by `dec`.
fn weak_decodes(s: &Scenario, mut dec: AcarsMultiChannelDecoder) -> usize {
    let offs = offsets(s);
    let weak_ch = s.plan.iter().position(|&f| (f - WEAK_MHZ).abs() < 1e-6).expect("weak channel");
    let npow = 2.0 * f64::from(SIGMA).powi(2) * 10_000.0 / FS;
    let a_weak = (npow * 10f64.powf(s.snr_db / 10.0)).sqrt() as f32;
    let i_amp = 10f64.powf(s.level_db / 20.0) as f32 * a_weak;
    let texts: Vec<String> = (0..s.bursts).map(|k| format!("WEAK PAYLOAD {k:04}")).collect();
    let probe = burst_iq(&weak_spec(&texts[0]), FS, 0.0, 1.0).len();
    let slot = probe + (FS * 0.08) as usize;
    let lead = (FS * 0.25) as usize;
    let total = lead + s.bursts * slot + (FS * 0.2) as usize;
    let strong = burst_iq(&STRONG_SPEC, FS, 0.0, 1.0);

    let woff = offs[weak_ch];
    let w_rot = Complex::from_polar(1.0, TAU * woff / FS);
    let i_rot = Complex::from_polar(1.0, TAU * (woff + s.delta_hz) / FS);
    let vrot: Vec<Complex<f64>> =
        VOICE_F.iter().map(|f| Complex::from_polar(1.0, TAU * f / FS)).collect();
    let mut vph: Vec<Complex<f64>> =
        (0..VOICE_F.len()).map(|k| Complex::from_polar(1.0, k as f64 * 1.7)).collect();
    let (mut wph, mut iph) = (Complex::new(1.0f64, 0.0), Complex::new(1.0f64, 0.0));
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ s.seed.wrapping_mul(0x2545_F491_4F6C_DD1D));
    let mut cur: Option<(usize, Vec<Complex<f32>>)> = None;
    let mut got = vec![false; s.bursts];
    let mut buf = vec![Complex::new(0.0f32, 0.0); CHUNK];
    let mut n0 = 0usize;
    while n0 < total {
        let len = CHUNK.min(total - n0);
        for (k, x) in buf[..len].iter_mut().enumerate() {
            let n = n0 + k;
            let (a, b) = rng.gauss_pair();
            *x = Complex::new(a * SIGMA, b * SIGMA);
            if n >= lead && (n - lead) / slot < s.bursts && (n - lead) % slot < probe {
                let idx = (n - lead) / slot;
                if cur.as_ref().is_none_or(|(i, _)| *i != idx) {
                    cur = Some((idx, burst_iq(&weak_spec(&texts[idx]), FS, 0.0, 1.0)));
                }
                if let Some(&v) = cur.as_ref().and_then(|(_, b)| b.get((n - lead) % slot)) {
                    *x += v * Complex::new(wph.re as f32, wph.im as f32) * a_weak;
                }
            }
            let r = Complex::new(iph.re as f32, iph.im as f32);
            match s.interferer {
                Interferer::Acars => *x += strong[(n + 12_345) % strong.len()] * r * i_amp,
                Interferer::Voice => {
                    let mut au = 0.0f64;
                    for (ph, (rot, amp)) in vph.iter_mut().zip(vrot.iter().zip(VOICE_A)) {
                        au += amp * ph.im;
                        *ph *= rot;
                    }
                    let syl = 0.5 * (1.0 + (TAU * 4.0 * n as f64 / FS).sin());
                    *x += r * (i_amp * (1.0 + 0.8 * (au * syl) as f32));
                }
            }
            wph *= w_rot;
            iph *= i_rot;
            if n.is_multiple_of(4096) {
                wph /= wph.norm();
                iph /= iph.norm();
                for ph in &mut vph {
                    *ph /= ph.norm();
                }
            }
        }
        for (ch, frames) in dec.process(&buf[..len]) {
            if ch != weak_ch {
                continue;
            }
            for f in frames.iter().filter(|f| f.crc_ok) {
                let k = f.text.strip_prefix("WEAK PAYLOAD ").and_then(|k| k.parse().ok());
                if let Some(k) = k.filter(|&k: &usize| k < s.bursts) {
                    got[k] = true;
                }
            }
        }
        n0 += len;
    }
    got.iter().filter(|&&g| g).count()
}

fn offsets(s: &Scenario) -> Vec<f64> {
    s.plan.iter().map(|f| (f * 1e6 - s.center_hz).round()).collect()
}

#[test]
fn us_plan_weak_channel_survives_strong_acars_neighbour() {
    // 16-channel US plan centered 131.1425 MHz (off the 25 kHz raster, so
    // channels sit off their bin centers), weak 131.525 MHz near its
    // 50%-decode SNR, back-to-back ACARS bursts +30 dB at −25 kHz. Measured
    // on this capture: shared decimation 21/40; the critically sampled
    // channelizer with only an anti-alias stage 0/40.
    let s = Scenario {
        plan: &PLAN16,
        center_hz: 131_142_500.0,
        snr_db: 8.5,
        interferer: Interferer::Acars,
        delta_hz: -25_000.0,
        level_db: 30.0,
        bursts: 40,
        seed: 11,
    };
    let dec = AcarsMultiChannelDecoder::new(FS, &offsets(&s)).unwrap();
    let front = dec.describe_front();
    let got = weak_decodes(&s, dec);
    eprintln!("{front}: {got}/{}", s.bursts);
    // Floor a few bursts under the shared decimation's rate.
    assert!(got >= 18, "{front}: weak channel decoded {got}/{} (floor 18)", s.bursts);
}

#[test]
fn weak_channel_survives_strong_voice_neighbour() {
    // 8-channel plan centered 131.500 MHz, weak 131.525 MHz at the same SNR,
    // multitone AM voice +40 dB at +16.67 kHz (an 8.33 kHz-raster channel).
    // Measured on this capture: shared decimation 30/40; the critically
    // sampled channelizer with only an anti-alias stage 2/40.
    let s = Scenario {
        plan: &PLAN8,
        center_hz: 131_500_000.0,
        snr_db: 8.5,
        interferer: Interferer::Voice,
        delta_hz: 16_667.0,
        level_db: 40.0,
        bursts: 40,
        seed: 12,
    };
    let dec = AcarsMultiChannelDecoder::new(FS, &offsets(&s)).unwrap();
    let front = dec.describe_front();
    let got = weak_decodes(&s, dec);
    eprintln!("{front}: {got}/{}", s.bursts);
    // Floor a few bursts under the shared decimation's rate.
    assert!(got >= 24, "{front}: weak channel decoded {got}/{} (floor 24)", s.bursts);
}
