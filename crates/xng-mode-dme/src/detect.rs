//! Pulse-pair detector (channel IQ at [`crate::CHANNEL_RATE`] → pulse pairs).
//!
//! DME pulses are Gaussian-like, 3.5 µs wide at half amplitude (Annex 10
//! §3.5.4.1.3), sent in pairs 12, 30 or 36 µs apart. The chain:
//!
//!  1. **Matched filter.** A real Gaussian FIR with the pulse's own shape
//!     (σ = 3.5 / 2.355 µs), so `z[n] = |y[n]|²` peaks at each pulse with the
//!     best per-pulse SNR. Real taps tolerate the SDR's carrier offset.
//!  2. **Noise floor.** The median of `z` over each block (pulses fill a few
//!     percent of samples, so the median is noise) divided by ln 2 is the mean
//!     noise power of an exponential variable; smoothed across blocks.
//!  3. **Pair statistic.** For each spacing `D`, `s[n] = z[n−D] + z[n]`: the
//!     energy of both pulses is added before the decision, instead of making
//!     two hard per-pulse decisions and requiring both. That is a 2-sample
//!     non-coherent integration and gains about 2–3 dB at the same false-alarm
//!     rate over an AND of two single-pulse detections.
//!  4. **Balance gate.** Both pulses of a pair are sent at the same power. A
//!     lone strong pulse lifts `s` too, so each half must also carry at least
//!     `min_half` × noise and at least [`BALANCE`] of the stronger half.
//!  5. **Peak pick**: the strongest candidate within the spacing plus
//!     [`PEAK_EXTRA`] samples, with a parabolic fit for sub-sample timing.
//!
//! Two thresholds run on one pass: the report threshold (pairs counted in
//! rates and messages) and a lower one feeding only the ident detector, which
//! demands exact 1350 Hz periodicity and so tolerates many more false pairs.
//!
//! Written from the Annex 10 signal description and textbook detection
//! theory; no code from other DME decoders.

use num_complex::Complex;

/// Fraction of the stronger half the weaker half of a pair must reach.
const BALANCE: f32 = 0.12;
/// Peak-pick window beyond the pulse spacing, samples. One transponder never
/// overlaps its own pairs, so within `spacing + PEAK_EXTRA` of a pair only the
/// strongest candidate survives: the skirts of a strong pulse plus a noise
/// bump 12 µs away would otherwise pass as a second, weak pair.
const PEAK_EXTRA: u64 = 8;
/// Noise-floor block length, samples.
const NOISE_BLOCK: usize = 4096;
/// Smoothing of the per-block noise estimate.
const NOISE_ALPHA: f32 = 0.2;
/// Matched-filter power ring length (power of two > the largest spacing + 2).
const RING: usize = 128;
const RING_MASK: u64 = RING as u64 - 1;

/// Detection thresholds, in units of the mean noise power of `z`.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    /// `z1 + z2` must exceed this (report threshold).
    pub pair: f32,
    /// Each half must exceed this.
    pub min_half: f32,
    /// Lower pair threshold for ident candidates.
    pub weak_pair: f32,
    /// Lower per-half threshold for ident candidates.
    pub weak_min_half: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        // Pure noise: z1 + z2 is Gamma(2, 1), P(> t) = e^-t (1 + t).
        // t = 19 → 1.1e-7 per sample per spacing before the balance gate and
        // peak picking: well under one false pair per second at 1 MS/s.
        Self {
            pair: 19.0,
            min_half: 3.0,
            weak_pair: 12.0,
            weak_min_half: 2.0,
        }
    }
}

/// One detected pulse pair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PulsePair {
    /// Time of the first pulse peak, µs from the start of the stream.
    pub t_us: f64,
    /// Pulse spacing, µs.
    pub spacing_us: u32,
    /// Matched-filter SNR of the pair (mean of both halves over noise), dB.
    pub snr_db: f32,
    /// Peak power of the pair (mean of both halves), linear, channel units.
    pub power: f32,
    /// Passed only the weak (ident) threshold.
    pub weak: bool,
}

struct Spacing {
    us: u32,
    d: usize,
    /// Pending peak: (sample index of the second pulse, statistic, previous,
    /// next, z1, z2, weak).
    pending: Option<Peak>,
}

#[derive(Clone, Copy)]
struct Peak {
    n: u64,
    s: f32,
    s_prev: f32,
    s_next: Option<f32>,
    z1: f32,
    z2: f32,
    weak: bool,
}

/// Streaming pulse-pair detector at a fixed channel rate.
pub struct PairDetector {
    rate: f64,
    taps: Vec<f32>,
    /// Input history for the FIR (taps.len() − 1 samples).
    x_hist: Vec<Complex<f32>>,
    /// Matched-filter power ring, indexed by absolute sample & [`RING_MASK`].
    ring: [f32; RING],
    /// FIR input scratch (history + new block), reused across calls.
    scratch: Vec<Complex<f32>>,
    spacings: Vec<Spacing>,
    th: Thresholds,
    noise: f32,
    noise_block: Vec<f32>,
    /// Absolute index of the next matched-filter output sample.
    n: u64,
    /// Filter group delay, samples.
    delay: f64,
    level: f32,
}

impl PairDetector {
    /// `rate` = channel sample rate (Hz); `spacings_us` = pulse spacings to
    /// look for (12, 30, 36).
    pub fn new(rate: f64, spacings_us: &[u32], th: Thresholds) -> Self {
        let sigma = 3.5e-6 / (2.0 * (2.0f64 * 2.0f64.ln()).sqrt()) * rate;
        let half = (4.0 * sigma).ceil() as i64;
        let mut taps: Vec<f32> = (-half..=half)
            .map(|k| (-(k as f64).powi(2) / (2.0 * sigma * sigma)).exp() as f32)
            .collect();
        let e: f32 = taps.iter().map(|t| t * t).sum::<f32>().sqrt();
        taps.iter_mut().for_each(|t| *t /= e);
        let max_d = spacings_us
            .iter()
            .map(|&us| (us as f64 * 1e-6 * rate).round() as usize)
            .max()
            .unwrap_or(0);
        assert!(
            max_d + 2 < RING,
            "pulse spacing too long for the detector ring at this rate"
        );
        let spacings = spacings_us
            .iter()
            .map(|&us| Spacing {
                us,
                d: (us as f64 * 1e-6 * rate).round() as usize,
                pending: None,
            })
            .collect();
        let ntaps = taps.len();
        Self {
            rate,
            taps,
            x_hist: vec![Complex::new(0.0, 0.0); ntaps - 1],
            ring: [0.0; RING],
            scratch: Vec::new(),
            spacings,
            th,
            noise: 0.0,
            noise_block: Vec::with_capacity(NOISE_BLOCK),
            n: 0,
            delay: half as f64,
            level: 0.0,
        }
    }

    /// Mean noise power of the matched-filter output (0 until the first block).
    pub fn noise(&self) -> f32 {
        self.noise
    }

    /// Smoothed channel power, dBFS.
    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-20).log10()
    }

    /// Feed channel IQ; append detected pairs (report and weak) to `out`.
    pub fn process(&mut self, input: &[Complex<f32>], out: &mut Vec<PulsePair>) {
        let ntaps = self.taps.len();
        let mut buf = std::mem::take(&mut self.scratch);
        buf.clear();
        buf.extend_from_slice(&self.x_hist);
        buf.extend_from_slice(input);
        let mut power_acc = 0.0f32;
        for i in 0..input.len() {
            let w = &buf[i..i + ntaps];
            let mut re = 0.0f32;
            let mut im = 0.0f32;
            for (x, &t) in w.iter().zip(self.taps.iter()) {
                re += x.re * t;
                im += x.im * t;
            }
            let z = re * re + im * im;
            power_acc += input[i].norm_sqr();
            self.push_z(z, out);
        }
        if !input.is_empty() {
            self.level += 0.05 * (power_acc / input.len() as f32 - self.level);
        }
        let keep = ntaps - 1;
        self.x_hist.clear();
        self.x_hist.extend_from_slice(&buf[buf.len() - keep..]);
        self.scratch = buf;
    }

    fn push_z(&mut self, z: f32, out: &mut Vec<PulsePair>) {
        // Noise floor from block medians.
        self.noise_block.push(z);
        if self.noise_block.len() == NOISE_BLOCK {
            let mid = NOISE_BLOCK / 2;
            let (_, m, _) = self
                .noise_block
                .select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
            let est = *m / std::f32::consts::LN_2;
            self.noise = if self.noise == 0.0 {
                est
            } else {
                self.noise + NOISE_ALPHA * (est - self.noise)
            };
            self.noise_block.clear();
        }

        let n = self.n;
        self.ring[(n & RING_MASK) as usize] = z;
        self.n += 1;
        if self.noise <= 0.0 {
            return;
        }
        let nz = self.noise;
        let th = self.th;
        let ring = &self.ring;
        let at = |k: usize| ring[(n.wrapping_sub(k as u64) & RING_MASK) as usize];
        let rate = self.rate;
        let delay = self.delay;
        for sp in self.spacings.iter_mut() {
            let z2 = z;
            let z1 = at(sp.d);
            let s = z1 + z2;
            // Previous statistic (for the parabolic fit) and the pending peak.
            let s_prev = at(1) + at(1 + sp.d);
            if let Some(p) = sp.pending.as_mut() {
                if p.s_next.is_none() && p.n + 1 == n {
                    p.s_next = Some(s);
                }
                if n > p.n + sp.d as u64 + PEAK_EXTRA {
                    let p = sp.pending.take().unwrap();
                    out.push(finish(p, sp.us, sp.d, nz, rate, delay));
                }
            }
            if s < th.weak_pair * nz {
                continue;
            }
            let lo = z1.min(z2);
            let hi = z1.max(z2);
            if lo < BALANCE * hi || lo < th.weak_min_half * nz {
                continue;
            }
            let weak = s < th.pair * nz || lo < th.min_half * nz;
            let cand = Peak {
                n,
                s,
                s_prev,
                s_next: None,
                z1,
                z2,
                weak,
            };
            match sp.pending.as_mut() {
                Some(p) if n <= p.n + sp.d as u64 + PEAK_EXTRA => {
                    if s > p.s {
                        *p = cand;
                    } else if p.s_next.is_none() && p.n + 1 == n {
                        p.s_next = Some(s);
                    }
                }
                _ => sp.pending = Some(cand),
            }
        }
    }

    /// Flush pending peaks at end of stream.
    pub fn flush(&mut self, out: &mut Vec<PulsePair>) {
        let nz = self.noise;
        for sp in self.spacings.iter_mut() {
            if let Some(p) = sp.pending.take() {
                out.push(finish(p, sp.us, sp.d, nz, self.rate, self.delay));
            }
        }
    }
}

fn finish(p: Peak, us: u32, d: usize, noise: f32, rate: f64, delay: f64) -> PulsePair {
    // Parabolic interpolation of the peak.
    let frac = match p.s_next {
        Some(nx) => {
            let den = p.s_prev - 2.0 * p.s + nx;
            if den.abs() > 1e-12 {
                (0.5 * (p.s_prev - nx) / den).clamp(-0.5, 0.5) as f64
            } else {
                0.0
            }
        }
        None => 0.0,
    };
    // `p.n` indexes the second pulse at the filter output; the first pulse
    // is `d` samples earlier; remove the filter's group delay.
    let first = p.n as f64 + frac - d as f64 - delay;
    let mean = 0.5 * (p.z1 + p.z2);
    PulsePair {
        t_us: first / rate * 1e6,
        spacing_us: us,
        snr_db: 10.0 * (mean / noise.max(1e-30)).log10(),
        power: mean,
        weak: p.weak,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modulate::{generate, score, SimConfig};

    /// A strong pair must give exactly one detection: the skirts of its
    /// pulses plus noise 12 µs away must not pass as extra pairs.
    #[test]
    fn one_detection_per_strong_pair() {
        for snr in [20.0, 30.0, 40.0] {
            let cfg = SimConfig {
                sample_rate: 1e6,
                duration_s: 0.5,
                snr_db: snr,
                seed: 2,
                ..Default::default()
            };
            let sim = generate(&cfg);
            let mut d = PairDetector::new(1e6, &[12, 30, 36], Thresholds::default());
            let mut out = Vec::new();
            d.process(&sim.iq, &mut out);
            d.flush(&mut out);
            // The noise floor needs one 4096-sample block first.
            let truth: Vec<_> = sim
                .pairs
                .iter()
                .copied()
                .filter(|p| p.t_us > 5000.0)
                .collect();
            let t: Vec<f64> = out
                .iter()
                .filter(|p| !p.weak && p.spacing_us == 12 && p.t_us > 4990.0)
                .map(|p| p.t_us)
                .collect();
            let (tp, fp) = score(&truth, &t, 0.5);
            assert_eq!(tp, truth.len(), "snr {snr}");
            assert_eq!(fp, 0, "snr {snr}");
            // Timing within a tenth of a microsecond at high SNR.
            let i = sim.pairs.len() / 2;
            let near = t
                .iter()
                .map(|x| (x - sim.pairs[i].t_us).abs())
                .fold(f64::MAX, f64::min);
            assert!(near < 0.1, "snr {snr}: {near}");
        }
    }
}
