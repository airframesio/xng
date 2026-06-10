//! HFDL burst demodulator (v1: per-T-segment phase tracking instead of
//! an LMS equalizer — see PROVENANCE.md).
//!
//! Hunt: differential correlation against the 127-chip A sequence (CFO-
//! immune); the correlation phase gives the per-symbol carrier rotation,
//! A1→A2 coherent phases refine it, the coherent correlation sign
//! resolves the global π ambiguity. M1 is matched against all 8 cyclic
//! shifts to learn the rate/slot setting. Data segments are demodulated
//! with the phase re-estimated at every 15-symbol T training segment;
//! per-symbol scrambler π flips are removed before Gray soft demod.

use crate::fec::{self, Setting, SETTINGS};
use num_complex::Complex;
use std::f32::consts::PI;
use xng_dsp::viterbi::Viterbi;

pub const SYMBOL_RATE: f64 = 1_800.0;
const CORR_A1: f32 = 0.4;
const CORR_M1: f32 = 0.4;
const PREAMBLE_SYMS: usize = 127 + 127 + 127 + 15 + 135;

pub struct Burst {
    pub bps: u32,
    pub payload: Vec<u8>,
}

enum State {
    Hunt,
    /// A1 found at this absolute position (first A1 symbol center).
    Collect { a1_pos: f64, theta: f32, needed_syms: Option<(Setting, usize)> },
}

pub struct HfdlDemod {
    sps: f64,
    buf: Vec<Complex<f32>>,
    start_abs: f64,
    cursor: f64,
    noise: f32,
    state: State,
    level: f32,
    viterbi: Viterbi,
}

impl HfdlDemod {
    pub fn new(channel_rate: f64) -> Self {
        Self {
            sps: channel_rate / SYMBOL_RATE,
            buf: Vec::new(),
            start_abs: 0.0,
            cursor: 0.0,
            noise: 1e-6,
            state: State::Hunt,
            level: 0.0,
            viterbi: Viterbi::k7(),
        }
    }

    fn sample(&self, abs_pos: f64) -> Option<Complex<f32>> {
        let rel = abs_pos - self.start_abs;
        if rel < 0.0 {
            return None;
        }
        let i = rel.floor() as usize;
        if i + 1 >= self.buf.len() {
            return None;
        }
        let frac = (rel - i as f64) as f32;
        Some(self.buf[i] * (1.0 - frac) + self.buf[i + 1] * frac)
    }

    /// Differential correlation of the A sequence at `pos` (first symbol
    /// center): returns (metric, per-symbol rotation).
    fn a_diff_correlate(&self, pos: f64) -> Option<(f32, f32)> {
        let a = fec::bits_of(fec::A_BITS);
        let mut corr = Complex::new(0.0f32, 0.0);
        let mut norm = 0.0f32;
        let mut prev = self.sample(pos)?;
        for j in 1..127 {
            let s = self.sample(pos + j as f64 * self.sps)?;
            let d = s * prev.conj();
            prev = s;
            // Expected differential: ±1 depending on bit change.
            let sign = if a[j] != a[j - 1] { -1.0 } else { 1.0 };
            corr += d * sign;
            norm += d.norm();
        }
        // No per-point consistency gate here: at fractional samples per
        // symbol, some symbol-spaced samples interpolate across phase
        // transitions and legitimately dip to ~0. False locks are caught
        // by the metric threshold and the M1 confirmation stage.
        if norm < 1e-9 {
            return None;
        }
        Some((corr.norm() / norm, corr.arg()))
    }

    /// Coherent correlation against a ±1 sequence with rotation `theta`
    /// per symbol from `pos`. Returns the complex correlation.
    fn coherent(&self, pos: f64, bits: &[u8], theta: f32) -> Option<Complex<f32>> {
        let mut corr = Complex::new(0.0f32, 0.0);
        for (j, &b) in bits.iter().enumerate() {
            let s = self.sample(pos + j as f64 * self.sps)?;
            let derot = s * Complex::from_polar(1.0, -theta * j as f32);
            corr += if b == 1 { -derot } else { derot };
        }
        Some(corr)
    }

    fn hunt(&mut self) -> Option<(f64, f32)> {
        let span = 130.0 * self.sps;
        let end_abs = self.start_abs + self.buf.len() as f64 - span - 2.0;
        while self.cursor < end_abs {
            let pos = self.cursor;
            self.cursor += 1.0;
            let rel = (pos - self.start_abs) as usize;
            let p = self.buf[rel].norm_sqr();
            // Asymmetric floor tracking: fall fast, rise very slowly, so
            // sweeping across a burst cannot raise the gate above it.
            if p < self.noise {
                self.noise += 0.01 * (p - self.noise);
            } else {
                self.noise += 1e-5 * (p - self.noise);
            }
            if p < self.noise * 4.0 {
                continue;
            }
            if let Some((metric, theta)) = self.a_diff_correlate(pos) {
                if metric > CORR_A1 {
                    let mut best = (metric, pos);
                    for k in [-2.0f64, -1.0, -0.5, 0.5, 1.0, 2.0] {
                        if let Some((m, _)) = self.a_diff_correlate(pos + k) {
                            if m > best.0 {
                                best = (m, pos + k);
                            }
                        }
                    }
                    let (_, theta2) = self.a_diff_correlate(best.1).unwrap_or((0.0, theta));
                    return Some((best.1, theta2));
                }
            }
        }
        None
    }

    /// Demodulate the whole burst once enough samples are buffered.
    fn finish(&mut self, a1_pos: f64, theta0: f32, s: Setting) -> Option<Burst> {
        let a = fec::bits_of(fec::A_BITS);
        let t = fec::bits_of(fec::T_BITS);
        let m = fec::bits_of(fec::M_BITS);

        // Refine carrier: coherent A1/A2 phases 127 symbols apart.
        let c1 = self.coherent(a1_pos, &a, theta0)?;
        let c2 = self.coherent(a1_pos + 127.0 * self.sps, &a, theta0)?;
        let dphi = (c2 * c1.conj()).arg();
        let theta = theta0 + dphi / 127.0;
        // Global π ambiguity: sign of the coherent correlation.
        let c1r = self.coherent(a1_pos, &a, theta)?;
        let flip = if c1r.re < 0.0 { -1.0f32 } else { 1.0 };
        let mut phase = (c1r * flip).arg();

        // Verify M1 with the refined carrier (sanity).
        let m1_pos = a1_pos + 254.0 * self.sps;
        let m1_bits: Vec<u8> = (0..127).map(|j| m[(s.m1_shift + j) % 127]).collect();
        let cm = self.coherent(m1_pos, &m1_bits, theta)?;
        if cm.norm() / 127.0 < 0.2 {
            return None;
        }

        // Walk the burst: phase re-estimated at each T segment. The
        // measured phase includes the carrier ramp up to the segment
        // start, so data symbols are derotated RELATIVE to the segment
        // where the phase was measured.
        let seg0 = a1_pos + (127.0 * 2.0 + 127.0 + 15.0) * self.sps;
        let mut pos = seg0;
        let mut phase_ref = a1_pos; // position where `phase` was measured
        for _ in 0..9 {
            if let Some(c) = self.coherent(pos, &t, theta) {
                phase = (c * flip).arg();
                phase_ref = pos;
            }
            pos += 15.0 * self.sps;
        }

        let flips = fec::scramble_flips(s.data_segments() * 30);
        let m_levels = 1u32 << s.bps_per_sym;
        let mut soft: Vec<f32> = Vec::with_capacity(s.chips());
        let mut data_idx = 0usize;
        for _ in 0..s.data_segments() {
            for k in 0..30 {
                let sp = pos + k as f64 * self.sps;
                let y = self.sample(sp)?;
                let rel_syms = ((sp - phase_ref) / self.sps) as f32;
                let derot =
                    y * Complex::from_polar(1.0, -(theta * rel_syms) - phase) * flip;
                let mut ang = derot.arg();
                if flips[data_idx] == 1 {
                    ang += PI;
                }
                data_idx += 1;
                // Per-bit soft decisions over the Gray ring.
                for bit in 0..s.bps_per_sym {
                    soft.push(gray_soft(ang, m_levels, bit));
                }
            }
            pos += 30.0 * self.sps;
            if let Some(c) = self.coherent(pos, &t, theta) {
                let cf = c * flip;
                if cf.norm() / 15.0 > 0.1 {
                    phase = cf.arg();
                    phase_ref = pos;
                }
            }
            pos += 15.0 * self.sps;
        }

        // Deinterleave → (rate-1/4 average) → Viterbi → LSB-first bytes.
        let deleaved = fec::deinterleave(&soft, &s);
        let vit_in: Vec<f32> = if s.rate_quarter {
            deleaved.chunks_exact(2).map(|p| (p[0] + p[1]) / 2.0).collect()
        } else {
            deleaved
        };
        let bits = self.viterbi.decode(&vit_in);
        let payload: Vec<u8> = bits
            .chunks(8)
            .map(|c| c.iter().enumerate().fold(0u8, |b, (i, &v)| b | (v << i)))
            .collect();
        Some(Burst { bps: s.bps, payload })
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<Burst> {
        self.buf.extend_from_slice(input);
        for x in input {
            self.level += 1e-4 * (x.norm_sqr() - self.level);
        }
        let mut out = Vec::new();
        loop {
            match std::mem::replace(&mut self.state, State::Hunt) {
                State::Hunt => match self.hunt() {
                    Some((a1_pos, theta)) => {
                        #[cfg(feature = "demod-debug")]
                        eprintln!("DBG hunt: a1 at {a1_pos:.1}, theta {theta:.5}");
                        self.state = State::Collect { a1_pos, theta, needed_syms: None };
                    }
                    None => break,
                },
                State::Collect { a1_pos, theta, mut needed_syms } => {
                    // Detect M1 once the preamble region is buffered.
                    if needed_syms.is_none() {
                        let m1_end = a1_pos + 384.0 * self.sps;
                        if self.sample(m1_end).is_none() {
                            self.state = State::Collect { a1_pos, theta, needed_syms };
                            break;
                        }
                        let m = fec::bits_of(fec::M_BITS);
                        let m1_pos = a1_pos + 254.0 * self.sps;
                        let mut best: Option<(f32, Setting)> = None;
                        for s in &SETTINGS {
                            let bits: Vec<u8> =
                                (0..127).map(|j| m[(s.m1_shift + j) % 127]).collect();
                            if let Some(c) = self.coherent(m1_pos, &bits, theta) {
                                let metric = c.norm() / 127.0;
                                if best.map(|(b, _)| metric > b).unwrap_or(true) {
                                    best = Some((metric, *s));
                                }
                            }
                        }
                        #[cfg(feature = "demod-debug")]
                        eprintln!("DBG m1: best {:?}", best.map(|(m, s)| (m, s.bps, s.double_slot)));
                        match best {
                            Some((metric, s)) if metric > CORR_M1 * 0.5 => {
                                let total = PREAMBLE_SYMS + s.data_segments() * 45;
                                needed_syms = Some((s, total));
                            }
                            _ => {
                                self.cursor = a1_pos + 64.0;
                                self.state = State::Hunt;
                                continue;
                            }
                        }
                    }
                    let (s, total) = needed_syms.unwrap();
                    let end = a1_pos + (total as f64 + 2.0) * self.sps;
                    if self.sample(end).is_none() {
                        self.state = State::Collect { a1_pos, theta, needed_syms };
                        break;
                    }
                    let fin = self.finish(a1_pos, theta, s);
                    #[cfg(feature = "demod-debug")]
                    eprintln!("DBG finish: ok={} ", fin.is_some());
                    if let Some(b) = fin {
                        #[cfg(feature = "demod-debug")]
                        eprintln!("DBG payload head: {:02X?}", &b.payload[..16.min(b.payload.len())]);
                        out.push(b);
                    }
                    self.cursor = end;
                    self.state = State::Hunt;
                }
            }
        }
        // Drop consumed samples.
        let active = match &self.state {
            State::Collect { a1_pos, .. } => *a1_pos - 4.0 * self.sps,
            State::Hunt => self.cursor - 4.0 * self.sps,
        };
        let keep_from = (active - self.start_abs).max(0.0) as usize;
        if keep_from > 0 && keep_from <= self.buf.len() {
            self.buf.drain(..keep_from);
            self.start_abs += keep_from as f64;
        }
        out
    }

    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }
}

/// Soft decision for bit position `bit` (MSB first) of a Gray M-PSK
/// symbol at angle `ang`: min-distance difference between the nearest
/// constellation points labelled 0 and 1 in that bit.
fn gray_soft(ang: f32, m: u32, bit: u32) -> f32 {
    let nbits = m.trailing_zeros();
    let mut d0 = f32::MAX;
    let mut d1 = f32::MAX;
    for n in 0..m {
        let label = n ^ (n >> 1);
        let pa = std::f32::consts::TAU * n as f32 / m as f32;
        let mut diff = (ang - pa).rem_euclid(std::f32::consts::TAU);
        if diff > PI {
            diff = std::f32::consts::TAU - diff;
        }
        let d = diff * diff;
        if (label >> (nbits - 1 - bit)) & 1 == 1 {
            d1 = d1.min(d);
        } else {
            d0 = d0.min(d);
        }
    }
    ((d0 - d1) / 2.0).clamp(-1.0, 1.0)
}
