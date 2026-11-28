//! VOR bearing measurement (ICAO Annex 10 Vol I 3.3.5).
//!
//! Signal, relative to the carrier:
//!
//! - variable phase: 30 Hz AM, `m30·cos(ωt − θ)` (≈30%);
//! - reference phase: a 9960 Hz subcarrier (≈30%) frequency-modulated by
//!   30 Hz with ±480 Hz deviation, instantaneous frequency
//!   `9960 + 480·cos(ωt)`, so its phase is `β·sin(ωt)` with β = 16;
//! - the radial θ is how far the variable tone lags the reference.
//!
//! A Doppler VOR swaps which antenna makes which tone but reverses the
//! rotation, so the received signal has the same form (plus, sometimes, a
//! little sideband asymmetry that lands in Q and is ignored here).
//!
//! Measurement, per 0.5 s block at [`TONE_RATE`] (15 whole 30 Hz cycles):
//!
//! - variable: one DFT bin at 30 Hz of the coherent in-phase signal;
//! - reference: the subcarrier is mixed to 0 Hz and its 30 Hz FM phase is
//!   estimated by **maximum likelihood**: the block is folded onto the 80
//!   samples of one 30 Hz cycle and correlated against `exp(jβ sin(·+φ))`
//!   for every φ, the peak refined by interpolation. Other decoders run an
//!   FM discriminator, which clicks and fails below about 10 dB subcarrier
//!   SNR; the matched correlation integrates the whole block instead.
//! - the subcarrier's own frequency error (allowed ±1%, about ±100 Hz) is
//!   found from the discriminator mean while unlocked, then tracked by
//!   wiping the FM with the estimated phase and measuring the residual tone.
//!
//! Both paths use the same 48 kHz → 2400 Hz filter, so their group delays
//! cancel in the phase difference.

use crate::paths::{MixDecim, TONE_RATE};
use num_complex::Complex;
use rustfft::FftPlanner;
use serde::{Deserialize, Serialize};
use std::f64::consts::{PI, TAU};

pub const SUBCARRIER_HZ: f64 = 9_960.0;
pub const DEVIATION_HZ: f64 = 480.0;
const SAMPLES_PER_CYCLE: usize = 80;
const BLOCK: usize = 1_200; // 0.5 s
const PHASE_GRID: usize = 360;
/// Subcarrier history for the comb frequency search (6.8 s, 0.146 Hz bins).
const COMB_N: usize = 16_384;
/// FM lines summed either side of the subcarrier (J_n(16) is small beyond 17).
const COMB_LINES: i32 = 20;
/// Allowed subcarrier error (Annex 10: 9960 Hz ± 1%).
const SC_SEARCH_HZ: f64 = 105.0;
/// Reference detection over `n` blocks: the correlation peak must be this
/// many times the noise it would hold with no reference, `REF_NOISE_MEAN +
/// REF_NOISE_SPREAD / sqrt(n)`. With noise only the per-block ratio is the
/// largest of many correlated exponentials: mean about 8.6, standard
/// deviation 2.6 (measured), so this is four of its standard errors.
const REF_NOISE_MEAN: f64 = 9.0;
const REF_NOISE_SPREAD: f64 = 10.4;
/// An unmodulated 9960 Hz tone is not a reference, though the template
/// correlates with it through one Bessel line J_h(ratio). Folded (derotated
/// at any 30 Hz alias) such a tone is a single harmonic of the cycle: the
/// peak is then at most J_h^2 < 0.08 times what that harmonic alone
/// explains, while a frequency-modulated reference, which spreads over many
/// lines, gives 1 / max J_h^2 > 12 times. The peak must be this many times
/// the strongest harmonic's.
const REF_CW_FACTOR: f64 = 2.0;

/// One reporting interval's VOR measurement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VorMeasure {
    /// Magnetic radial FROM the station (°), circular mean over the interval;
    /// absent when no block carried both tones.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radial_deg: Option<f64>,
    /// Circular standard deviation of the 0.5 s radials (°).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radial_std_deg: Option<f64>,
    /// 30 Hz variable AM depth (%).
    pub mod30_pct: f64,
    /// 9960 Hz subcarrier AM depth (%).
    pub mod9960_pct: f64,
    /// Measured FM deviation of the subcarrier (Hz, nominal 480).
    pub deviation_hz: f64,
    /// Subcarrier frequency error (Hz, nominal 0, allowed ±99.6).
    pub subcarrier_offset_hz: f64,
    /// Blocks averaged.
    pub blocks: u32,
}

#[derive(Default, Clone, Copy)]
struct BlockResult {
    radial: f64,
    weight: f64,
    m30: f64,
    msc: f64,
    dev: f64,
    /// Reference correlation peak and the noise it would hold without one.
    ref_c: f64,
    ref_noise: f64,
    ref_cw: f64,
}

pub struct VorAnalyzer {
    var: MixDecim,
    sub: MixDecim,
    vbuf: Vec<Complex<f32>>,
    sbuf: Vec<Complex<f32>>,
    /// Global sample index at TONE_RATE (for the 30 Hz phase reference).
    n: u64,
    /// Current block being filled.
    bv: Vec<f32>,
    bs: Vec<Complex<f32>>,
    /// Which of [`templates`] (deviation ratio) the reference matches.
    ratio: usize,
    sc_off_hz: f64,
    sc_phase: f64,
    /// Raw (not derotated) subcarrier history for the comb search.
    sc_hist: Vec<Complex<f32>>,
    blocks_seen: u64,
    comb_done: bool,
    results: Vec<BlockResult>,
}

impl Default for VorAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl VorAnalyzer {
    pub fn new() -> Self {
        Self {
            var: MixDecim::new(0.0),
            sub: MixDecim::new(SUBCARRIER_HZ),
            vbuf: Vec::new(),
            sbuf: Vec::new(),
            n: 0,
            bv: Vec::with_capacity(BLOCK),
            bs: Vec::with_capacity(BLOCK),
            ratio: NOMINAL_RATIO,
            sc_off_hz: 0.0,
            sc_phase: 0.0,
            sc_hist: Vec::with_capacity(COMB_N + BLOCK),
            blocks_seen: 0,
            comb_done: false,
            results: Vec::new(),
        }
    }

    /// Feed coherent in-phase samples at 48 kHz.
    pub fn process(&mut self, x: &[f32]) {
        self.vbuf.clear();
        self.sbuf.clear();
        self.var.process(x, &mut self.vbuf);
        self.sub.process(x, &mut self.sbuf);
        let n = self.vbuf.len().min(self.sbuf.len());
        for i in 0..n {
            if self.bv.is_empty() && !self.n.is_multiple_of(SAMPLES_PER_CYCLE as u64) {
                // Start blocks on a cycle boundary so folding is aligned.
                self.n += 1;
                continue;
            }
            self.bv.push(self.vbuf[i].re);
            self.bs.push(self.sbuf[i]);
            self.n += 1;
            if self.bv.len() == BLOCK {
                self.finish_block();
            }
        }
    }

    fn finish_block(&mut self) {
        let start = self.n - BLOCK as u64;
        let bv = std::mem::take(&mut self.bv);
        let bs = std::mem::take(&mut self.bs);
        let nf = BLOCK as f64;
        let w30 = TAU / SAMPLES_PER_CYCLE as f64;

        // Carrier amplitude and variable phase.
        let amp: f64 = bv.iter().map(|&v| v as f64).sum::<f64>() / nf;
        let mut pv = Complex::new(0.0, 0.0);
        for (i, &v) in bv.iter().enumerate() {
            let k = ((start + i as u64) % SAMPLES_PER_CYCLE as u64) as f64;
            pv += Complex::from_polar(v as f64, -w30 * k);
        }
        let m30 = if amp > 0.0 {
            2.0 * pv.norm() / (nf * amp)
        } else {
            0.0
        };

        // Subcarrier frequency: discriminator mean until locked.
        let mut disc = 0.0;
        let mut dev_acc = Complex::new(0.0, 0.0);
        for i in 1..bs.len() {
            let d = (bs[i] * bs[i - 1].conj()).arg() as f64 * TONE_RATE / TAU;
            disc += d;
            let k = ((start + i as u64) % SAMPLES_PER_CYCLE as u64) as f64;
            // The difference at i spans i-1..i: centre it half a sample back.
            dev_acc += Complex::from_polar(d, -w30 * (k - 0.5));
        }
        let disc_mean = disc / (bs.len() - 1) as f64;
        let deviation = 2.0 * dev_acc.norm() / (bs.len() - 1) as f64;
        // Subcarrier frequency: the discriminator mean for the first 2 s;
        // then the comb search over the FM line spectrum (it works far
        // below the discriminator's threshold), on every block while the
        // 6.8 s history fills and every 2 s after.
        self.blocks_seen += 1;
        self.sc_hist.extend_from_slice(&bs);
        if self.sc_hist.len() > COMB_N {
            let d = self.sc_hist.len() - COMB_N;
            self.sc_hist.drain(..d);
        }
        let full = self.sc_hist.len() == COMB_N;
        if self.sc_hist.len() >= 4 * BLOCK && (!full || self.blocks_seen.is_multiple_of(4)) {
            if let Some(f) = comb_frequency(&self.sc_hist) {
                // The comb repeats every 30 Hz; the FM template only fits
                // the true line, so pick the 30 Hz alias, and with it the
                // deviation ratio, whose template matches best.
                let hist_start = self.n - self.sc_hist.len() as u64;
                let tail = 4 * BLOCK; // history holds at least this
                let from = self.sc_hist.len() - tail;
                let mut best = (f, 0.0, self.ratio);
                for k in -4i32..=4 {
                    let cand = f + 30.0 * k as f64;
                    if cand.abs() > SC_SEARCH_HZ {
                        continue;
                    }
                    let (m, r) =
                        self.fold_peak(&self.sc_hist[from..], hist_start + from as u64, cand);
                    if m > best.1 {
                        best = (cand, m, r);
                    }
                }
                self.sc_off_hz = best.0;
                self.ratio = best.2;
                self.comb_done = true;
            }
        } else if !self.comb_done {
            self.sc_off_hz = if self.blocks_seen == 1 {
                disc_mean
            } else {
                0.5 * (self.sc_off_hz + disc_mean)
            };
        }

        // Derotate the subcarrier offset and fold onto one 30 Hz cycle.
        let step = TAU * self.sc_off_hz / TONE_RATE;
        let mut fold = [Complex::new(0.0f64, 0.0); SAMPLES_PER_CYCLE];
        for (i, &s) in bs.iter().enumerate() {
            let z =
                Complex::new(s.re as f64, s.im as f64) * Complex::from_polar(1.0, -self.sc_phase);
            self.sc_phase = (self.sc_phase + step) % TAU;
            fold[((start + i as u64) % SAMPLES_PER_CYCLE as u64) as usize] += z;
        }
        // Correlate against every reference phase, with the template of the
        // deviation ratio the alias search found (16 until it has run).
        let mut c = vec![0.0f64; PHASE_GRID];
        for (m, cm) in c.iter_mut().enumerate() {
            let t = &templates()[self.ratio][m * SAMPLES_PER_CYCLE..(m + 1) * SAMPLES_PER_CYCLE];
            let mut acc = Complex::new(0.0, 0.0);
            for k in 0..SAMPLES_PER_CYCLE {
                acc += fold[k] * t[k];
            }
            *cm = acc.norm_sqr();
        }
        let mut best = 0;
        for m in 1..PHASE_GRID {
            if c[m] > c[best] {
                best = m;
            }
        }
        let l = c[(best + PHASE_GRID - 1) % PHASE_GRID];
        let r = c[(best + 1) % PHASE_GRID];
        let den = l - 2.0 * c[best] + r;
        let d = if den.abs() > 0.0 {
            0.5 * (l - r) / den
        } else {
            0.0
        };
        let phi_r = TAU * (best as f64 + d.clamp(-0.5, 0.5)) / PHASE_GRID as f64;
        let msc = if amp > 0.0 {
            2.0 * c[best].sqrt() / (nf * amp)
        } else {
            0.0
        };

        let phi_v = pv.arg();
        let radial = (phi_r - phi_v).rem_euclid(TAU);
        // Whether the 9960 Hz reference is there at all. Derotated, the
        // subcarrier repeats every 30 Hz cycle, so what differs between the
        // cycles folded onto one bin is noise: that gives the noise the
        // correlation peak would hold with no reference. (The peak's ratio
        // to the mean over phases does not work: a mismatched template
        // correlates broadly.)
        let per_bin = (BLOCK / SAMPLES_PER_CYCLE) as f64;
        let energy: f64 = bs.iter().map(|s| s.norm_sqr() as f64).sum();
        let folded: f64 = fold.iter().map(|f| f.norm_sqr()).sum::<f64>() / per_bin;
        let sigma2 = ((energy - folded) / (BLOCK - SAMPLES_PER_CYCLE) as f64).max(0.0);
        let t_best =
            &templates()[self.ratio][best * SAMPLES_PER_CYCLE..(best + 1) * SAMPLES_PER_CYCLE];
        let t_norm: f64 = t_best.iter().map(|t| t.norm_sqr()).sum();
        let ref_noise = per_bin * sigma2 * t_norm;
        // What a single tone (an unmodulated subcarrier) would give the same
        // template: the fold's strongest harmonic, scaled to the template.
        let ref_cw = t_norm
            * (0..SAMPLES_PER_CYCLE)
                .map(|h| {
                    fold.iter()
                        .enumerate()
                        .map(|(k, f)| {
                            f * Complex::from_polar(
                                1.0,
                                -TAU * (h * k % SAMPLES_PER_CYCLE) as f64
                                    / SAMPLES_PER_CYCLE as f64,
                            )
                        })
                        .sum::<Complex<f64>>()
                        .norm_sqr()
                })
                .fold(0.0, f64::max)
            / SAMPLES_PER_CYCLE as f64;
        // Weight by both tones' strength relative to the block's noise: the
        // product of the two depths works well and needs no noise estimate.
        let weight = (m30 * msc).max(0.0);
        self.results.push(BlockResult {
            radial,
            weight,
            m30,
            msc,
            dev: deviation,
            ref_c: c[best],
            ref_noise,
            ref_cw,
        });
    }

    /// Alias score of `x` (first sample at global index `start`) derotated
    /// by `f_hz`: each 0.5 s is folded on its own and the peak ML
    /// correlation over a coarse phase grid is summed, which tolerates the
    /// small residual frequency error left by the comb's resolution.
    fn fold_peak(&self, x: &[Complex<f32>], start: u64, f_hz: f64) -> (f64, usize) {
        let step = TAU * f_hz / TONE_RATE;
        let tm = templates();
        let mut score = vec![0.0f64; tm.len()];
        for chunk in 0..x.len() / BLOCK {
            let mut fold = [Complex::new(0.0f64, 0.0); SAMPLES_PER_CYCLE];
            for i in chunk * BLOCK..(chunk + 1) * BLOCK {
                let z = Complex::new(x[i].re as f64, x[i].im as f64)
                    * Complex::from_polar(1.0, -step * i as f64);
                fold[((start + i as u64) % SAMPLES_PER_CYCLE as u64) as usize] += z;
            }
            for (sc, t) in score.iter_mut().zip(tm) {
                *sc += (0..PHASE_GRID)
                    .step_by(5)
                    .map(|p| {
                        fold.iter()
                            .zip(&t[p * SAMPLES_PER_CYCLE..(p + 1) * SAMPLES_PER_CYCLE])
                            .map(|(f, t)| f * t)
                            .sum::<Complex<f64>>()
                            .norm_sqr()
                    })
                    .fold(0.0, f64::max);
            }
        }
        let (r, &best) = score
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap();
        (best, r)
    }

    /// Close the interval: the measurement over every block since the last
    /// call, or `None` if no block finished.
    pub fn take(&mut self) -> Option<VorMeasure> {
        if self.results.is_empty() {
            return None;
        }
        let res = std::mem::take(&mut self.results);
        let mut s = Complex::new(0.0, 0.0);
        let mut wsum = 0.0;
        for b in &res {
            s += Complex::from_polar(b.weight, b.radial);
            wsum += b.weight;
        }
        // No bearing evidence (no reference, or both tones absent): no
        // radial, but the depths still say what is on the air.
        let n = res.len() as f64;
        let ref_c: f64 = res.iter().map(|b| b.ref_c).sum();
        let ref_noise: f64 = res.iter().map(|b| b.ref_noise).sum();
        let ref_cw: f64 = res.iter().map(|b| b.ref_cw).sum();
        let reference = ref_c >= (REF_NOISE_MEAN + REF_NOISE_SPREAD / n.sqrt()) * ref_noise
            && ref_c >= REF_CW_FACTOR * ref_cw;
        let (radial, std) = if reference && wsum > 0.0 && s.norm() > 0.0 {
            let rbar = (s.norm() / wsum).min(1.0);
            let std = if rbar > 0.0 {
                (-2.0 * rbar.ln()).sqrt()
            } else {
                PI
            };
            (
                Some(s.arg().rem_euclid(TAU).to_degrees()),
                Some(std.to_degrees()),
            )
        } else {
            (None, None)
        };
        Some(VorMeasure {
            radial_deg: radial,
            radial_std_deg: std,
            mod30_pct: 100.0 * res.iter().map(|b| b.m30).sum::<f64>() / n,
            mod9960_pct: 100.0 * res.iter().map(|b| b.msc).sum::<f64>() / n,
            deviation_hz: res.iter().map(|b| b.dev).sum::<f64>() / n,
            subcarrier_offset_hz: self.sc_off_hz,
            blocks: res.len() as u32,
        })
    }
}

/// Deviation ratios with a template: Annex 10 allows 16 ± 1, and a Doppler
/// VOR may fall to 11 above 5° elevation. A template more than about one
/// off the signal's ratio can peak 10–20° from the true phase, so they are
/// one apart.
const RATIOS: [f64; 7] = [11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0];
const NOMINAL_RATIO: usize = 5;

/// The reference templates for [`RATIOS`], shared by every analyzer.
fn templates() -> &'static [Vec<Complex<f64>>] {
    static T: std::sync::OnceLock<Vec<Vec<Complex<f64>>>> = std::sync::OnceLock::new();
    T.get_or_init(|| RATIOS.iter().map(|&b| template(b)).collect())
}

/// `exp(-jβ sin(2πk/80 + φm))` for every reference phase, [m][k].
fn template(beta: f64) -> Vec<Complex<f64>> {
    let mut t = Vec::with_capacity(PHASE_GRID * SAMPLES_PER_CYCLE);
    for m in 0..PHASE_GRID {
        let phi = TAU * m as f64 / PHASE_GRID as f64;
        for k in 0..SAMPLES_PER_CYCLE {
            let a = beta * (TAU * k as f64 / SAMPLES_PER_CYCLE as f64 + phi).sin();
            t.push(Complex::new(a.cos(), -a.sin()));
        }
    }
    t
}

/// Subcarrier frequency error (Hz) from the comb of FM lines at
/// `Δf + n·30 Hz`: the candidate whose lines hold the most energy.
fn comb_frequency(x: &[Complex<f32>]) -> Option<f64> {
    let n = x.len();
    let mut b: Vec<Complex<f64>> = x
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let w = 0.5 - 0.5 * (TAU * i as f64 / n as f64).cos();
            Complex::new(c.re as f64 * w, c.im as f64 * w)
        })
        .collect();
    FftPlanner::new().plan_fft_forward(n).process(&mut b);
    let p: Vec<f64> = b.iter().map(|c| c.norm_sqr()).collect();
    let bin = TONE_RATE / n as f64;
    let at = |f: f64| -> f64 {
        let k = (f / bin).round() as i64;
        p[k.rem_euclid(n as i64) as usize]
    };
    let span = (SC_SEARCH_HZ / bin) as i64;
    let metric = |k: i64| -> f64 {
        let f0 = k as f64 * bin;
        (-COMB_LINES..=COMB_LINES)
            .map(|l| at(f0 + 30.0 * l as f64))
            .sum()
    };
    let m: Vec<f64> = (-span..=span).map(metric).collect();
    let (bi, &best) = m.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    let mut sorted = m.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let median = sorted[sorted.len() / 2];
    if best <= 0.0 || best < 1.5 * median {
        return None; // no subcarrier comb above the noise
    }
    let (l, r) = (
        m.get(bi.wrapping_sub(1)).copied().unwrap_or(best),
        m.get(bi + 1).copied().unwrap_or(best),
    );
    let den = l - 2.0 * best + r;
    let d = if den.abs() > 0.0 {
        0.5 * (l - r) / den
    } else {
        0.0
    };
    Some((bi as i64 - span) as f64 * bin + d.clamp(-0.5, 0.5) * bin)
}
