//! ILS localizer and glideslope measurement (ICAO Annex 10 Vol I 3.1.3.5,
//! 3.1.5.5).
//!
//! The carrier is amplitude-modulated by 90 Hz and 150 Hz tones (20% each on
//! a localizer, 40% on a glideslope at the course line). Annex 10 lets the
//! tones sit up to 2.5% off nominal (Category I), which is a whole DFT bin of
//! a 0.5 s block, so the tone frequencies are measured first: every 2 s a
//! zero-padded spectrum of the last 8 s finds each tone's peak near its
//! nominal frequency (nominal is used where a tone does not stand clear;
//! the first 2 s of blocks wait for the first estimate). Per 0.5 s
//! block at [`TONE_RATE`] a least-squares fit of DC and the two tones at
//! those frequencies gives the depths (at exactly 90 and 150 Hz this is the
//! plain DFT: 45 and 75 whole cycles), from which:
//!
//! - DDM = m90 − m150 (positive: 90 Hz dominant, i.e. left of a localizer
//!   course as seen on approach, or above a glide path);
//! - SDM = m90 + m150;
//! - `phase_lock_deg` = 5·φ90 − 3·φ150, which is constant when the two tones
//!   are phase-locked to a common 30 Hz as Annex 10 requires (only from
//!   blocks where both tones are measurable).
//!
//! Full-scale needle deflection (150 µA) is 0.155 DDM on a localizer and
//! 0.175 DDM on a glideslope.

use crate::catalog::NavaidKind;
use crate::paths::{MixDecim, TONE_RATE};
use num_complex::Complex;
use rustfft::FftPlanner;
use serde::{Deserialize, Serialize};
use std::f64::consts::TAU;

const BLOCK: usize = 1_200; // 0.5 s
/// Tone frequency search: 8 s of history, zero-padded to 0.037 Hz bins.
const HIST_N: usize = 19_200;
const EST_FFT_N: usize = 65_536;
const EST_EVERY_BLOCKS: u64 = 4;
/// The first estimate waits for this much history (2 s, 0.5 Hz bins before
/// padding: enough to find a tone 2.5% off nominal).
const FIRST_EST_BLOCKS: usize = 4;
/// Searched either side of nominal (fraction): 2.5% plus margin.
const TONE_TOL: f64 = 0.03;
/// A tone is measurable in a block when its amplitude is this many standard
/// errors above the fit's noise.
const MEASURABLE_Z: f64 = 4.0;

/// One reporting interval's ILS measurement for one carrier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IlsMeasure {
    /// Carrier offset from the channel center (Hz).
    pub carrier_offset_hz: f64,
    /// Carrier level relative to the strongest carrier in the channel (dB).
    pub relative_db: f64,
    /// Difference in depth of modulation, m90 − m150 (fraction).
    pub ddm: f64,
    /// The same DDM as needle current (µA, 150 µA full scale).
    pub ddm_ua: f64,
    /// Standard deviation of the 0.5 s DDM values.
    pub ddm_std: f64,
    /// Sum of depths of modulation (%).
    pub sdm_pct: f64,
    pub mod90_pct: f64,
    pub mod150_pct: f64,
    /// 5·φ90 − 3·φ150 (°): constant when the tones are phase-locked; absent
    /// when no block had both tones.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_lock_deg: Option<f64>,
    /// Measured tone frequencies (Hz, nominal 90 and 150).
    pub tone90_hz: f64,
    pub tone150_hz: f64,
    pub blocks: u32,
}

pub fn full_scale_ddm(kind: NavaidKind) -> f64 {
    match kind {
        NavaidKind::Glideslope => 0.175,
        _ => 0.155,
    }
}

#[derive(Clone, Copy)]
struct Block {
    m90: f64,
    m150: f64,
    lock: Option<Complex<f64>>,
}

pub struct IlsAnalyzer {
    kind: NavaidKind,
    path: MixDecim,
    buf: Vec<Complex<f32>>,
    block: Vec<f32>,
    n: u64,
    results: Vec<Block>,
    hist: Vec<f32>,
    blocks_seen: u64,
    f90: f64,
    f150: f64,
    /// Inverse Gram matrix of the fit's basis, for (f90, f150).
    fit: Option<(f64, f64, [[f64; 5]; 5])>,
    /// The tones have been measured at least once.
    estimated: bool,
    /// Blocks waiting for the first tone estimate.
    pending: Vec<Vec<f32>>,
}

impl IlsAnalyzer {
    pub fn new(kind: NavaidKind) -> Self {
        Self {
            kind,
            path: MixDecim::new(0.0),
            buf: Vec::new(),
            block: Vec::with_capacity(BLOCK),
            n: 0,
            results: Vec::new(),
            hist: Vec::with_capacity(HIST_N + BLOCK),
            blocks_seen: 0,
            f90: 90.0,
            f150: 150.0,
            fit: None,
            estimated: false,
            pending: Vec::new(),
        }
    }

    /// Feed coherent in-phase samples at 48 kHz.
    pub fn process(&mut self, x: &[f32]) {
        self.buf.clear();
        self.path.process(x, &mut self.buf);
        for i in 0..self.buf.len() {
            if self.block.is_empty() && !self.n.is_multiple_of(80) {
                self.n += 1;
                continue;
            }
            self.block.push(self.buf[i].re);
            self.n += 1;
            if self.block.len() == BLOCK {
                self.finish_block();
            }
        }
    }

    fn finish_block(&mut self) {
        let b = std::mem::take(&mut self.block);
        self.hist.extend_from_slice(&b);
        if self.hist.len() > HIST_N {
            let d = self.hist.len() - HIST_N;
            self.hist.drain(..d);
        }
        self.blocks_seen += 1;
        if self.hist.len() >= FIRST_EST_BLOCKS * BLOCK
            && self.blocks_seen.is_multiple_of(EST_EVERY_BLOCKS)
        {
            self.estimate_tones();
        }
        if self.estimated {
            self.fit_block(&b);
        } else {
            // Until the tones are measured, a fit at nominal could be a
            // bin off: keep the block for the first estimate.
            self.pending.push(b);
        }
    }

    /// Fit every block held back for the first tone estimate.
    fn fit_pending(&mut self) {
        for b in std::mem::take(&mut self.pending) {
            self.fit_block(&b);
        }
    }

    fn fit_block(&mut self, b: &[f32]) {
        let (f90, f150) = (self.f90, self.f150);
        let ginv = match self.fit {
            Some((a, c, g)) if a == f90 && c == f150 => g,
            _ => {
                let Some(g) = invert5(gram(f90, f150)) else {
                    return;
                };
                self.fit = Some((f90, f150, g));
                g
            }
        };
        // Least squares: coefficients of [1, cos90, sin90, cos150, sin150].
        let (w90, w150) = (TAU * f90 / TONE_RATE, TAU * f150 / TONE_RATE);
        let mut atb = [0.0f64; 5];
        let mut energy = 0.0;
        for (i, &v) in b.iter().enumerate() {
            let v = v as f64;
            let (s9, c9) = (w90 * i as f64).sin_cos();
            let (s15, c15) = (w150 * i as f64).sin_cos();
            for (acc, basis) in atb.iter_mut().zip([1.0, c9, s9, c15, s15]) {
                *acc += v * basis;
            }
            energy += v * v;
        }
        let mut coef = [0.0f64; 5];
        for (r, c) in coef.iter_mut().enumerate() {
            *c = (0..5).map(|k| ginv[r][k] * atb[k]).sum();
        }
        let amp = coef[0];
        if amp <= 0.0 {
            return;
        }
        let nf = BLOCK as f64;
        let fitted: f64 = coef.iter().zip(&atb).map(|(c, a)| c * a).sum();
        let resid = ((energy - fitted) / (nf - 5.0)).max(0.0);
        // Tone phasors (phase at the block start) and the noise on each.
        let x90 = Complex::new(coef[1], -coef[2]);
        let x150 = Complex::new(coef[3], -coef[4]);
        let se = (2.0 * resid / nf).sqrt();
        let m90 = x90.norm() / amp;
        let m150 = x150.norm() / amp;
        let both = x90.norm() > MEASURABLE_Z * se && x150.norm() > MEASURABLE_Z * se;
        let lock = both.then(|| Complex::from_polar(1.0, 5.0 * x90.arg() - 3.0 * x150.arg()));
        self.results.push(Block { m90, m150, lock });
    }

    /// Find each tone's frequency near nominal from the history spectrum.
    fn estimate_tones(&mut self) {
        let n = self.hist.len();
        let mean = self.hist.iter().map(|&v| v as f64).sum::<f64>() / n as f64;
        let mut buf: Vec<Complex<f64>> = self
            .hist
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let w = 0.5 - 0.5 * (TAU * i as f64 / n as f64).cos();
                Complex::new((v as f64 - mean) * w, 0.0)
            })
            .collect();
        buf.resize(EST_FFT_N, Complex::new(0.0, 0.0));
        FftPlanner::new()
            .plan_fft_forward(EST_FFT_N)
            .process(&mut buf);
        let bin = TONE_RATE / EST_FFT_N as f64;
        let p = |k: usize| buf[k].norm_sqr();
        // Noise floor between the tones (nothing is modulated there).
        let mut floor: Vec<f64> = ((105.0 / bin) as usize..(135.0 / bin) as usize)
            .map(p)
            .collect();
        floor.sort_by(|a, b| a.total_cmp(b));
        let floor = floor[floor.len() / 2].max(1e-300);
        let find = |nominal: f64| -> f64 {
            let lo = (nominal * (1.0 - TONE_TOL) / bin) as usize;
            let hi = (nominal * (1.0 + TONE_TOL) / bin) as usize;
            let k = (lo..=hi)
                .max_by(|&a, &b| p(a).total_cmp(&p(b)))
                .unwrap_or(lo);
            // 15 dB over the floor, else keep nominal.
            if k <= lo || k >= hi || p(k) < 30.0 * floor {
                return nominal;
            }
            let (l, c, r) = (p(k - 1).ln(), p(k).ln(), p(k + 1).ln());
            let den = l - 2.0 * c + r;
            let d = if den.abs() > 1e-12 {
                0.5 * (l - r) / den
            } else {
                0.0
            };
            let f = (k as f64 + d.clamp(-0.5, 0.5)) * bin;
            // Within the estimate's own error of nominal: use nominal, where
            // the fit is an exact DFT.
            if (f - nominal).abs() < 0.01 {
                nominal
            } else {
                f
            }
        };
        self.f90 = find(90.0);
        self.f150 = find(150.0);
        self.estimated = true;
        self.fit_pending();
    }

    /// Close the interval.
    pub fn take(&mut self, carrier_offset_hz: f64, relative_db: f64) -> Option<IlsMeasure> {
        if !self.pending.is_empty() {
            // Closing before the first estimate is due: measure the tones
            // now if there is enough history, else fit at nominal.
            if self.hist.len() >= FIRST_EST_BLOCKS * BLOCK {
                self.estimate_tones();
            } else {
                self.fit_pending();
            }
        }
        if self.results.is_empty() {
            return None;
        }
        let r = std::mem::take(&mut self.results);
        let n = r.len() as f64;
        let m90 = r.iter().map(|b| b.m90).sum::<f64>() / n;
        let m150 = r.iter().map(|b| b.m150).sum::<f64>() / n;
        let ddm = m90 - m150;
        let var = r
            .iter()
            .map(|b| (b.m90 - b.m150 - ddm).powi(2))
            .sum::<f64>()
            / n;
        let locks: Vec<Complex<f64>> = r.iter().filter_map(|b| b.lock).collect();
        let lock: Complex<f64> = locks.iter().sum();
        Some(IlsMeasure {
            carrier_offset_hz,
            relative_db,
            ddm,
            ddm_ua: ddm * 150.0 / full_scale_ddm(self.kind),
            ddm_std: var.sqrt(),
            sdm_pct: 100.0 * (m90 + m150),
            mod90_pct: 100.0 * m90,
            mod150_pct: 100.0 * m150,
            phase_lock_deg: (!locks.is_empty()).then(|| lock.arg().to_degrees()),
            tone90_hz: self.f90,
            tone150_hz: self.f150,
            blocks: r.len() as u32,
        })
    }
}

/// Gram matrix of the fit basis over one block.
fn gram(f90: f64, f150: f64) -> [[f64; 5]; 5] {
    let (w90, w150) = (TAU * f90 / TONE_RATE, TAU * f150 / TONE_RATE);
    let mut g = [[0.0; 5]; 5];
    for i in 0..BLOCK {
        let (s9, c9) = (w90 * i as f64).sin_cos();
        let (s15, c15) = (w150 * i as f64).sin_cos();
        let v = [1.0, c9, s9, c15, s15];
        for r in 0..5 {
            for c in 0..5 {
                g[r][c] += v[r] * v[c];
            }
        }
    }
    g
}

/// Gauss-Jordan inverse with partial pivoting.
fn invert5(mut a: [[f64; 5]; 5]) -> Option<[[f64; 5]; 5]> {
    let mut inv = [[0.0; 5]; 5];
    for (i, row) in inv.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    for col in 0..5 {
        let piv = (col..5).max_by(|&x, &y| a[x][col].abs().total_cmp(&a[y][col].abs()))?;
        if a[piv][col].abs() < 1e-9 {
            return None;
        }
        a.swap(col, piv);
        inv.swap(col, piv);
        let d = a[col][col];
        for k in 0..5 {
            a[col][k] /= d;
            inv[col][k] /= d;
        }
        for r in 0..5 {
            if r != col {
                let f = a[r][col];
                for k in 0..5 {
                    a[r][k] -= f * a[col][k];
                    inv[r][k] -= f * inv[col][k];
                }
            }
        }
    }
    Some(inv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_of_the_gram_matrix() {
        let g = gram(91.2, 152.0);
        let inv = invert5(g).unwrap();
        for r in 0..5 {
            for c in 0..5 {
                let v: f64 = (0..5).map(|k| g[r][k] * inv[k][c]).sum();
                assert!((v - if r == c { 1.0 } else { 0.0 }).abs() < 1e-9);
            }
        }
    }
}
