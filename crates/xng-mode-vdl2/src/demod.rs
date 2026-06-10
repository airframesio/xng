//! VDL2 D8PSK burst demodulator.
//!
//! Input: complex channel IQ at 50 kHz (≈4.76 samples/symbol at
//! 10 500 sym/s — symbol instants are interpolated, so no integer
//! relationship is required between channel rate and symbol rate).
//!
//! Acquisition: differential correlation against the 16-symbol unique
//! word; the correlation phase yields the per-symbol carrier offset
//! rotation. Demod: symbol-spaced differential phase → nearest π/4
//! multiple → inverse Gray triplet, with decision-directed phase-drift
//! tracking; bits are descrambled on the fly, the 25-bit header gives the
//! transmission length, and the burst completes through deinterleave/RS
//! into an AVLC bit stream.

use crate::header::{self, HEADER_BITS};
use crate::interleave;
use crate::scramble::Scrambler;
use num_complex::Complex;
use std::f32::consts::PI;
use xng_dsp::rs::ReedSolomon;

pub const SYMBOL_RATE: f64 = 10_500.0;

/// Unique word as Δφ multiples of π/4 (Annex 10 §6.4.3.1.1.2).
pub(crate) const UW_DELTAS: [u8; 16] = [0, 3, 2, 4, 0, 1, 6, 4, 1, 7, 2, 5, 6, 5, 7, 3];

/// Gray map (Table 6-1): triplet index (X | Y<<1 | Z<<2, X first/LSB)
/// → Δφ in π/4 units.
pub const GRAY_FWD: [u8; 8] = [0, 7, 3, 4, 1, 6, 2, 5];
/// Δφ index → (X, Y, Z).
pub const GRAY_INV: [(u8, u8, u8); 8] = [
    (0, 0, 0),
    (0, 0, 1),
    (0, 1, 1),
    (0, 1, 0),
    (1, 1, 0),
    (1, 1, 1),
    (1, 0, 1),
    (1, 0, 0),
];

const CORR_THRESHOLD: f32 = 0.88;
const ENERGY_FACTOR: f32 = 6.0;
const NOISE_ALPHA: f32 = 1e-4;
const PHASE_GAIN: f32 = 0.1;

struct Collecting {
    /// Absolute sample position of the next symbol center.
    next_pos: f64,
    /// Per-symbol carrier rotation estimate (radians).
    theta: f32,
    prev: Complex<f32>,
    /// Descrambled bits collected so far.
    bits: Vec<u8>,
    scr: Scrambler,
    /// Total bits to collect once the header is decoded.
    needed: Option<usize>,
}

enum State {
    Hunt,
    Collect(Box<Collecting>),
}

pub struct Vdl2Demod {
    sps: f64,
    /// Channel samples; index 0 is absolute sample `start_abs`.
    buf: Vec<Complex<f32>>,
    start_abs: f64,
    /// Hunt cursor (absolute).
    cursor: f64,
    noise: f32,
    state: State,
    level: f32,
}

/// A demodulated, descrambled, RS-corrected burst: the AVLC bit stream.
pub struct Burst {
    pub bits: Vec<u8>,
    pub rs_corrected: usize,
}

impl Vdl2Demod {
    pub fn new(channel_rate: f64) -> Self {
        Self {
            sps: channel_rate / SYMBOL_RATE,
            buf: Vec::new(),
            start_abs: 0.0,
            cursor: 0.0,
            noise: 1e-6,
            state: State::Hunt,
            level: 0.0,
        }
    }

    /// Linear interpolation at an absolute sample position.
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

    /// Correlate the UW assuming the first UW symbol center is at `pos`.
    /// Returns (metric 0..1, carrier rotation per symbol). The metric is
    /// zeroed when the per-symbol energies are wildly uneven — a high
    /// normalized correlation from a couple of strong points (burst edge
    /// against silence) is a false lock, not a preamble.
    fn uw_correlate(&self, pos: f64) -> Option<(f32, f32)> {
        let mut corr = Complex::new(0.0f32, 0.0);
        let mut norm = 0.0f32;
        let mut min_d = f32::MAX;
        let mut prev = self.sample(pos)?;
        for (j, &delta) in UW_DELTAS.iter().enumerate().skip(1) {
            let s = self.sample(pos + j as f64 * self.sps)?;
            let d = s * prev.conj();
            prev = s;
            let expected = Complex::from_polar(1.0, delta as f32 * PI / 4.0);
            corr += d * expected.conj();
            norm += d.norm();
            min_d = min_d.min(d.norm());
        }
        if norm < 1e-9 {
            return None;
        }
        let mean_d = norm / (UW_DELTAS.len() - 1) as f32;
        if min_d < 0.25 * mean_d {
            return Some((0.0, 0.0));
        }
        Some((corr.norm() / norm, corr.arg()))
    }

    /// Hunt for a UW; returns the refined first-UW-symbol position.
    fn hunt(&mut self) -> Option<f64> {
        let span = 18.0 * self.sps;
        let end_abs = self.start_abs + self.buf.len() as f64 - span - 2.0;
        while self.cursor < end_abs {
            let pos = self.cursor;
            self.cursor += 1.0;
            let rel = (pos - self.start_abs) as usize;
            let p = self.buf[rel].norm_sqr();
            self.noise += NOISE_ALPHA * (p - self.noise);
            if p < self.noise * ENERGY_FACTOR {
                continue;
            }
            if let Some((metric, _)) = self.uw_correlate(pos) {
                if metric > CORR_THRESHOLD {
                    let mut best = (metric, pos);
                    for k in [-2.0f64, -1.0, -0.5, 0.5, 1.0, 2.0] {
                        if let Some((m, _)) = self.uw_correlate(pos + k) {
                            if m > best.0 {
                                best = (m, pos + k);
                            }
                        }
                    }
                    return Some(best.1);
                }
            }
        }
        None
    }

    /// Advance a collection; returns Some(result) when the burst ends.
    /// `Ok(bits)` = complete, `Err(())` = header failure.
    fn collect(&self, c: &mut Collecting) -> Option<Result<(), ()>> {
        loop {
            if c.needed.is_none() && c.bits.len() >= HEADER_BITS {
                let hdr: [u8; HEADER_BITS] = c.bits[..HEADER_BITS].try_into().unwrap();
                match header::decode(&hdr)
                    .and_then(|tl| interleave::layout(tl as usize))
                {
                    Some(lay) => c.needed = Some(HEADER_BITS + lay.total_tx_bits),
                    None => return Some(Err(())),
                }
            }
            if let Some(n) = c.needed {
                if c.bits.len() >= n {
                    return Some(Ok(()));
                }
            }
            let Some(s) = self.sample(c.next_pos) else {
                return None; // wait for more samples
            };
            let d = s * c.prev.conj();
            c.prev = s;
            c.next_pos += self.sps;
            let ph = d.arg() - c.theta;
            let idx_f = (ph / (PI / 4.0)).round();
            let idx = (idx_f as i32).rem_euclid(8) as usize;
            let residual = ph - idx_f * (PI / 4.0);
            c.theta += PHASE_GAIN * residual;
            let (x, y, z) = GRAY_INV[idx];
            for b in [x, y, z] {
                c.bits.push(b ^ c.scr.next_bit());
            }
        }
    }

    pub fn process(&mut self, input: &[Complex<f32>], rs: &ReedSolomon) -> Vec<Burst> {
        self.buf.extend_from_slice(input);
        for x in input {
            self.level += NOISE_ALPHA * (x.norm_sqr() - self.level);
        }
        let mut out = Vec::new();

        loop {
            match std::mem::replace(&mut self.state, State::Hunt) {
                State::Hunt => match self.hunt() {
                    Some(uw_pos) => {
                        let (_, theta) = self.uw_correlate(uw_pos).unwrap();
                        let last_uw = uw_pos + 15.0 * self.sps;
                        let prev = self.sample(last_uw).unwrap();
                        self.state = State::Collect(Box::new(Collecting {
                            next_pos: last_uw + self.sps,
                            theta,
                            prev,
                            bits: Vec::new(),
                            scr: Scrambler::new(),
                            needed: None,
                        }));
                    }
                    None => break, // need more samples
                },
                State::Collect(mut c) => match self.collect(&mut c) {
                    None => {
                        self.state = State::Collect(c);
                        break; // need more samples
                    }
                    Some(Err(())) => {
                        // Bad header: resume hunting just past this UW.
                        self.state = State::Hunt;
                    }
                    Some(Ok(())) => {
                        let n = c.needed.unwrap();
                        let hdr: [u8; HEADER_BITS] = c.bits[..HEADER_BITS].try_into().unwrap();
                        let tl_bits = header::decode(&hdr).unwrap() as usize;
                        if let Some((avlc_bits, fixed)) =
                            interleave::deinterleave(&c.bits[HEADER_BITS..n], tl_bits, rs)
                        {
                            out.push(Burst { bits: avlc_bits, rs_corrected: fixed });
                        }
                        self.cursor = c.next_pos; // skip past the burst
                        self.state = State::Hunt;
                    }
                },
            }
        }

        // Drop consumed samples (keep a tail behind the active position).
        let active = match &self.state {
            State::Collect(c) => c.next_pos,
            State::Hunt => self.cursor,
        };
        let keep_from = (active - self.start_abs - 4.0 * self.sps).max(0.0) as usize;
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
