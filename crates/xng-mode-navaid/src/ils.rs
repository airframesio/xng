//! ILS localizer and glideslope measurement (ICAO Annex 10 Vol I 3.1.3.5,
//! 3.1.5.5).
//!
//! The carrier is amplitude-modulated by 90 Hz and 150 Hz tones (20% each on
//! a localizer, 40% on a glideslope at the course line). Per 0.5 s block at
//! [`TONE_RATE`] (45 and 75 whole cycles) one DFT bin each gives the two
//! depths, from which:
//!
//! - DDM = m90 − m150 (positive: 90 Hz dominant, i.e. left of a localizer
//!   course as seen on approach, or above a glide path);
//! - SDM = m90 + m150;
//! - `phase_lock_deg` = 5·φ90 − 3·φ150, which is constant when the two tones
//!   are phase-locked to a common 30 Hz as Annex 10 requires.
//!
//! Full-scale needle deflection (150 µA) is 0.155 DDM on a localizer and
//! 0.175 DDM on a glideslope.

use crate::catalog::NavaidKind;
use crate::paths::{MixDecim, TONE_RATE};
use num_complex::Complex;
use serde::{Deserialize, Serialize};
use std::f64::consts::TAU;

const BLOCK: usize = 1_200; // 0.5 s

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
    /// 5·φ90 − 3·φ150 (°): constant when the tones are phase-locked.
    pub phase_lock_deg: f64,
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
    lock: Complex<f64>,
}

pub struct IlsAnalyzer {
    kind: NavaidKind,
    path: MixDecim,
    buf: Vec<Complex<f32>>,
    block: Vec<f32>,
    n: u64,
    results: Vec<Block>,
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
        let start = self.n - BLOCK as u64;
        let nf = BLOCK as f64;
        let amp = b.iter().map(|&v| v as f64).sum::<f64>() / nf;
        let bin = |f: f64| {
            let w = TAU * f / TONE_RATE;
            let mut acc = Complex::new(0.0, 0.0);
            for (i, &v) in b.iter().enumerate() {
                let t = ((start + i as u64) % 80) as f64; // 80 samples = 1/30 s
                acc += Complex::from_polar(v as f64, -w * t);
            }
            acc
        };
        let x90 = bin(90.0);
        let x150 = bin(150.0);
        if amp <= 0.0 {
            return;
        }
        let m90 = 2.0 * x90.norm() / (nf * amp);
        let m150 = 2.0 * x150.norm() / (nf * amp);
        let lock = Complex::from_polar(1.0, 5.0 * x90.arg() - 3.0 * x150.arg());
        self.results.push(Block { m90, m150, lock });
    }

    /// Close the interval.
    pub fn take(&mut self, carrier_offset_hz: f64, relative_db: f64) -> Option<IlsMeasure> {
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
        let lock: Complex<f64> = r.iter().map(|b| b.lock).sum();
        Some(IlsMeasure {
            carrier_offset_hz,
            relative_db,
            ddm,
            ddm_ua: ddm * 150.0 / full_scale_ddm(self.kind),
            ddm_std: var.sqrt(),
            sdm_pct: 100.0 * (m90 + m150),
            mod90_pct: 100.0 * m90,
            mod150_pct: 100.0 * m150,
            phase_lock_deg: lock.arg().to_degrees(),
            blocks: r.len() as u32,
        })
    }
}
