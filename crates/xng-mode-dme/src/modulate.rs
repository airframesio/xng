//! Synthetic DME transponder signal for loopback tests and benchmarks.
//!
//! Generates the ground side of a DME channel at any sample rate: random
//! squitter/reply pulse pairs (Poisson with a transponder dead time) plus an
//! optional Morse ident keyed at 1350 pp/s, Gaussian or cos² pulses 3.5 µs
//! wide, a carrier offset, and complex white noise. Also returns the true
//! pair times so detectors can be scored.
//!
//! SNR convention: peak pulse power over noise power in a 1 MHz bandwidth
//! (`A² / (N0 · 1 MHz)`), independent of the sample rate.

use crate::ident::{morse_code, NOMINAL_PERIOD_US};
use num_complex::Complex;

/// Pulse envelope.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PulseShape {
    /// Gaussian, 3.5 µs at half amplitude.
    Gaussian,
    /// cos² over 7 µs, 3.5 µs at half amplitude (closer to a real
    /// transponder's 2.5 µs rise and fall than a Gaussian).
    CosSquared,
}

/// Simulation settings.
#[derive(Debug, Clone)]
pub struct SimConfig {
    pub sample_rate: f64,
    pub duration_s: f64,
    pub spacing_us: f64,
    /// Mean random pair rate (squitter + replies), pairs/s.
    pub random_pps: f64,
    /// Minimum gap after any pair, µs.
    pub dead_time_us: f64,
    /// Morse ident text (keyed once at `ident_start_s`).
    pub ident: Option<String>,
    pub ident_start_s: f64,
    pub dot_ms: f64,
    /// Ident pair period, µs.
    pub ident_period_us: f64,
    /// Stop random pairs while the ident is keyed down.
    pub suppress_random_during_ident: bool,
    pub snr_db: f64,
    pub amplitude: f64,
    pub freq_offset_hz: f64,
    pub shape: PulseShape,
    /// Add TACAN bearing reference bursts (X mode): main bursts of 12 pairs
    /// 30 µs apart at 15 Hz, auxiliary bursts of 6 pairs 24 µs apart at the
    /// other eight 135 Hz slots.
    pub tacan: bool,
    pub seed: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            sample_rate: 2_000_000.0,
            duration_s: 1.0,
            spacing_us: 12.0,
            random_pps: 2700.0,
            dead_time_us: 60.0,
            ident: None,
            ident_start_s: 0.2,
            dot_ms: 125.0,
            ident_period_us: NOMINAL_PERIOD_US,
            suppress_random_during_ident: false,
            snr_db: 20.0,
            amplitude: 0.1,
            freq_offset_hz: 0.0,
            shape: PulseShape::Gaussian,
            tacan: false,
            seed: 1,
        }
    }
}

/// One transmitted pair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TruthPair {
    /// First pulse peak, µs.
    pub t_us: f64,
    pub ident: bool,
}

pub struct SimOutput {
    pub iq: Vec<Complex<f32>>,
    /// Transmitted pairs in time order.
    pub pairs: Vec<TruthPair>,
    /// Key-down intervals of the ident, µs.
    pub keydown: Vec<(f64, f64)>,
}

/// Small deterministic RNG (xorshift64*), so tests need no dependency.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in (0, 1).
    pub fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    /// Standard normal pair (Box–Muller).
    pub fn normal2(&mut self) -> (f64, f64) {
        let r = (-2.0 * self.uniform().ln()).sqrt();
        let th = std::f64::consts::TAU * self.uniform();
        (r * th.cos(), r * th.sin())
    }
}

/// Key-down intervals (µs) for a Morse ident starting at `start_us`.
pub fn keying(text: &str, start_us: f64, dot_ms: f64) -> Vec<(f64, f64)> {
    let dot = dot_ms * 1000.0;
    let mut t = start_us;
    let mut out = Vec::new();
    for (li, c) in text.chars().enumerate() {
        if li > 0 {
            t += 3.0 * dot;
        }
        let Some(code) = morse_code(c) else { continue };
        for (ei, e) in code.chars().enumerate() {
            if ei > 0 {
                t += dot;
            }
            let len = if e == '.' { dot } else { 3.0 * dot };
            out.push((t, t + len));
            t += len;
        }
    }
    out
}

/// Generate the signal.
pub fn generate(cfg: &SimConfig) -> SimOutput {
    let fs = cfg.sample_rate;
    let n = (cfg.duration_s * fs) as usize;
    let mut rng = Rng::new(cfg.seed);
    let end_us = cfg.duration_s * 1e6;

    let keydown = match &cfg.ident {
        Some(text) => keying(text, cfg.ident_start_s * 1e6, cfg.dot_ms),
        None => Vec::new(),
    };
    let keyed = |t: f64| keydown.iter().any(|&(a, b)| t >= a && t < b);

    // Ident pairs.
    let mut pairs: Vec<TruthPair> = Vec::new();
    for &(a, b) in &keydown {
        let mut t = a;
        while t < b {
            pairs.push(TruthPair {
                t_us: t,
                ident: true,
            });
            t += cfg.ident_period_us;
        }
    }
    // TACAN reference bursts (they take priority like ident pairs).
    if cfg.tacan {
        let slot = 1e6 / 135.0;
        let mut k = 0u64;
        loop {
            let t0 = 500.0 + k as f64 * slot;
            if t0 > end_us - 1000.0 {
                break;
            }
            let (n, gap) = if k.is_multiple_of(9) {
                (12, 30.0)
            } else {
                (6, 24.0)
            };
            for j in 0..n {
                pairs.push(TruthPair {
                    t_us: t0 + j as f64 * gap,
                    ident: false,
                });
            }
            k += 1;
        }
        pairs.sort_by(|a, b| a.t_us.total_cmp(&b.t_us));
    }
    // Random pairs: Poisson after a dead time, not overlapping ident pairs.
    let mean_gap = 1e6 / cfg.random_pps.max(1e-9) - cfg.dead_time_us;
    let mut t = 20.0;
    let ident_times: Vec<f64> = pairs.iter().map(|p| p.t_us).collect();
    // A random pair must not overlap a reserved (ident/burst) pair.
    while cfg.random_pps > 0.0 && t <= end_us - 50.0 {
        t += cfg.dead_time_us + (-rng.uniform().ln()) * mean_gap.max(1.0);
        if t > end_us - 50.0 {
            break;
        }
        if cfg.suppress_random_during_ident && keyed(t) {
            continue;
        }
        // Transponder priority: ident pairs win; drop a reply that collides.
        let i = ident_times.partition_point(|&x| x < t - cfg.spacing_us - 10.0);
        if ident_times
            .get(i)
            .is_some_and(|&x| (x - t).abs() < cfg.spacing_us + 10.0)
            || (cfg.tacan && ident_times.get(i).is_some_and(|&x| x - t < 400.0))
        {
            continue;
        }
        pairs.push(TruthPair {
            t_us: t,
            ident: false,
        });
    }
    pairs.sort_by(|a, b| a.t_us.total_cmp(&b.t_us));
    pairs.retain(|p| p.t_us + cfg.spacing_us + 10.0 < end_us);

    // Noise.
    let snr = 10f64.powf(cfg.snr_db / 10.0);
    let a = cfg.amplitude;
    let noise_var = a * a / snr * (fs / 1e6);
    let nstd = (noise_var / 2.0).sqrt();
    let mut iq: Vec<Complex<f32>> = (0..n)
        .map(|_| {
            let (x, y) = rng.normal2();
            Complex::new((x * nstd) as f32, (y * nstd) as f32)
        })
        .collect();

    // Pulses.
    let sigma = 3.5 / (2.0 * (2.0f64 * 2.0f64.ln()).sqrt()); // µs
    let half_len_us = match cfg.shape {
        PulseShape::Gaussian => 4.5 * sigma,
        PulseShape::CosSquared => 3.5,
    };
    let env = |dt: f64| -> f64 {
        match cfg.shape {
            PulseShape::Gaussian => (-dt * dt / (2.0 * sigma * sigma)).exp(),
            PulseShape::CosSquared => {
                if dt.abs() >= 3.5 {
                    0.0
                } else {
                    (std::f64::consts::PI * dt / 7.0).cos().powi(2)
                }
            }
        }
    };
    let w = std::f64::consts::TAU * cfg.freq_offset_hz;
    for p in &pairs {
        let phase0 = std::f64::consts::TAU * rng.uniform();
        for t0 in [p.t_us, p.t_us + cfg.spacing_us] {
            let i0 = (((t0 - half_len_us) * 1e-6 * fs).floor().max(0.0)) as usize;
            let i1 = ((((t0 + half_len_us) * 1e-6 * fs).ceil()) as usize).min(n);
            for (i, s) in iq.iter_mut().enumerate().take(i1).skip(i0) {
                let ts = i as f64 / fs; // s
                let e = a * env(ts * 1e6 - t0);
                let ph = phase0 + w * ts;
                *s += Complex::new((e * ph.cos()) as f32, (e * ph.sin()) as f32);
            }
        }
    }
    SimOutput { iq, pairs, keydown }
}

/// Score detections against the truth: (true positives, false positives).
/// A detection matches a transmitted pair within `tol_us`.
pub fn score(truth: &[TruthPair], detected_us: &[f64], tol_us: f64) -> (usize, usize) {
    let mut d: Vec<f64> = detected_us.to_vec();
    d.sort_by(|a, b| a.total_cmp(b));
    let mut used = vec![false; truth.len()];
    let mut tp = 0;
    let mut fp = 0;
    for &x in &d {
        let i = truth.partition_point(|p| p.t_us < x - tol_us);
        let mut hit = false;
        let mut j = i;
        while j < truth.len() && truth[j].t_us <= x + tol_us {
            if !used[j] {
                used[j] = true;
                hit = true;
                break;
            }
            j += 1;
        }
        if hit {
            tp += 1;
        } else {
            fp += 1;
        }
    }
    (tp, fp)
}
