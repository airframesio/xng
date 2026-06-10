//! HFDL burst demodulator: LMS equalizer trained on the T segments plus
//! decision-directed carrier tracking (see PROVENANCE.md).
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

/// T/2-spaced 15-tap LMS feed-forward equalizer (dumphfdl runs liquid's
/// eqlms_cccf the same way: push at 2 samples/symbol, output at symbol
/// instants, train on the known T training symbols).
struct Lms {
    w: Vec<Complex<f32>>,
    x: Vec<Complex<f32>>,
    pos: usize,
    mu: f32,
}

impl Lms {
    fn new(taps: usize, mu: f32) -> Self {
        // Identity initialization (unit center tap): the equalizer starts
        // as a pass-through of the symbol-center sample — exactly the
        // pre-equalizer behavior — and training can only improve on it.
        // (dumphfdl initializes as a lowpass instead, but its input is
        // matched-filtered at 2 samples/symbol; ours is not.)
        let mut w = vec![Complex::new(0.0f32, 0.0); taps];
        w[taps / 2] = Complex::new(1.0, 0.0);
        Self { w, x: vec![Complex::new(0.0, 0.0); taps], pos: 0, mu }
    }

    fn push(&mut self, s: Complex<f32>) {
        self.pos = (self.pos + 1) % self.x.len();
        self.x[self.pos] = s;
    }

    /// Equalizer output for the current window.
    fn exec(&self) -> Complex<f32> {
        let n = self.x.len();
        let mut y = Complex::new(0.0f32, 0.0);
        for k in 0..n {
            y += self.w[k] * self.x[(self.pos + n - k) % n];
        }
        y
    }

    /// One LMS step toward reference `d` given output `y`.
    fn step(&mut self, d: Complex<f32>, y: Complex<f32>) {
        let n = self.x.len();
        let e = d - y;
        let energy: f32 =
            self.x.iter().map(|v| v.norm_sqr()).sum::<f32>().max(1e-9);
        let g = self.mu / energy;
        for k in 0..n {
            let xk = self.x[(self.pos + n - k) % n];
            self.w[k] += g * e * xk.conj();
        }
    }
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
            // 133-output first in each coded pair, as off-air validation
            // showed for Aero; confirmed for HFDL against the sigidwiki
            // 21931 kHz capture (171-first yields no valid FCS).
            viterbi: Viterbi::new(7, 0o133, 0o171),
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
        self.coherent_with_energy(pos, bits, theta).map(|(c, _)| c)
    }

    /// Coherent correlation plus a scale-invariant quality metric in
    /// 0..1 (|corr| normalized by the window's signal energy) — gates
    /// must not depend on absolute signal amplitude (off-air captures
    /// are far below the synthetic unit level).
    fn coherent_with_energy(
        &self,
        pos: f64,
        bits: &[u8],
        theta: f32,
    ) -> Option<(Complex<f32>, f32)> {
        let mut corr = Complex::new(0.0f32, 0.0);
        let mut energy = 0.0f32;
        for (j, &b) in bits.iter().enumerate() {
            let s = self.sample(pos + j as f64 * self.sps)?;
            energy += s.norm_sqr();
            let derot = s * Complex::from_polar(1.0, -theta * j as f32);
            corr += if b == 1 { -derot } else { derot };
        }
        let metric = corr.norm() / (energy * bits.len() as f32).sqrt().max(1e-12);
        Some((corr, metric))
    }

    /// Coherent A1 fit: over a fine timing grid around `pos`, take the
    /// per-symbol phases of the 127 known BPSK chips (sign removed),
    /// unwrap, and fit residual ≈ a + b·k weighted by sample energy.
    /// The minimum-cost grid point yields timing and per-symbol CFO (b)
    /// jointly — the same coherent preamble sync that recovered the VDL2
    /// XID bursts, and unlike the A1→A2 dphi refinement it has no
    /// 2π/127-per-symbol aliasing.
    fn a1_fit(&self, pos: f64, theta0: f32) -> Option<(f64, f32)> {
        let a = fec::bits_of(fec::A_BITS);
        let mut best: Option<(f32, f64, f32)> = None;
        let mut t = -4.0f64;
        while t <= 4.0 {
            let cand = pos + t;
            t += 0.25;
            if cand < self.start_abs {
                continue;
            }
            let mut r = vec![0.0f32; 127];
            let mut w = vec![0.0f32; 127];
            let mut prev = 0.0f32;
            for k in 0..127 {
                let s = self.sample(cand + k as f64 * self.sps)?;
                let sign = if a[k] == 1 { -1.0f32 } else { 1.0 };
                // Remove the expected per-symbol rotation estimate so the
                // unwrap only has to track the residual.
                let mut ph = (s * Complex::from_polar(sign, -theta0 * k as f32)).arg();
                while ph - prev > PI {
                    ph -= 2.0 * PI;
                }
                while ph - prev < -PI {
                    ph += 2.0 * PI;
                }
                prev = ph;
                r[k] = ph;
                w[k] = s.norm_sqr();
            }
            let sw: f32 = w.iter().sum();
            if sw < 1e-12 {
                continue;
            }
            let kbar = w.iter().enumerate().map(|(k, &wk)| wk * k as f32).sum::<f32>() / sw;
            let rbar = w.iter().zip(&r).map(|(&wk, &rk)| wk * rk).sum::<f32>() / sw;
            let mut num = 0.0f32;
            let mut den = 0.0f32;
            for k in 0..127 {
                let dk = k as f32 - kbar;
                num += w[k] * dk * (r[k] - rbar);
                den += w[k] * dk * dk;
            }
            if den < 1e-12 {
                continue;
            }
            let b = num / den;
            let aoff = rbar - b * kbar;
            let mut cost = 0.0f32;
            for (k, (&wk, &rk)) in w.iter().zip(&r).enumerate() {
                let e = rk - aoff - b * k as f32;
                cost += wk * e * e;
            }
            cost /= sw;
            if best.map(|(c, _, _)| cost < c).unwrap_or(true) {
                best = Some((cost, cand, theta0 + b));
            }
        }
        best.map(|(_, p, th)| (p, th))
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
                    // Coherent joint timing/CFO refinement over the whole
                    // A1 (the differential peak is broad; a ~3-sample
                    // timing error nulls the coherent M1 correlation at
                    // 6.67 samples/symbol, and the differential CFO
                    // estimate is noisy).
                    if let Some(fit) = self.a1_fit(best.1, theta2) {
                        return Some(fit);
                    }
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
        let phase = (c1r * flip).arg();
        // Mean symbol amplitude from the A correlation, for normalizing
        // the equalizer input to a unit constellation.
        let amp = (c1r.norm() / 127.0).max(1e-9);

        // Verify M1 with the refined carrier (sanity).
        let m1_pos = a1_pos + 254.0 * self.sps;
        let m1_bits: Vec<u8> = (0..127).map(|j| m[(s.m1_shift + j) % 127]).collect();
        let (_, m1_metric) = self.coherent_with_energy(m1_pos, &m1_bits, theta)?;
        if m1_metric < 0.2 {
            return None;
        }

        // Walk the burst with a T/2-spaced LMS equalizer (15 taps, as in
        // dumphfdl): CFO is removed by derotating with theta relative to
        // a1_pos; the equalizer absorbs the constant phase, the global pi
        // flip (its training references include `flip`), and multipath
        // ISI. It trains on the 9 preamble T segments and retrains on
        // every embedded T segment, which replaces the previous per-T
        // phase re-estimation.
        // Remove CFO ramp, constant phase, the global pi flip, and the
        // amplitude — the equalizer then starts from a unit constellation
        // and the identity-initialized taps are already correct. `extra`
        // carries the decision-directed carrier correction: the A1→A2
        // theta refinement aliases at ±pi/127 per symbol, so a residual
        // rotation of up to ~0.025 rad/symbol can survive it (observed on
        // synthetic bursts); the DD loop below tracks it out.
        let derot_at = |this: &Self, p: f64, extra: f32| -> Option<Complex<f32>> {
            let y = this.sample(p)?;
            let rel = ((p - a1_pos) / this.sps) as f32;
            Some(y * Complex::from_polar(flip / amp, -theta * rel - phase - extra))
        };
        let mut eq = Lms::new(7, 0.10);
        // Decision-directed 2nd-order carrier loop (per symbol), applied
        // to the equalizer input.
        let mut carr_ph = 0.0f32;
        let mut carr_fr = 0.0f32;
        // Symbol-spaced (T-spaced) taps with the decision at the window
        // center: the delay line runs 3 symbols ahead of the symbol being
        // decided.
        let seg0 = a1_pos + (127.0 * 2.0 + 127.0 + 15.0) * self.sps;
        let mut tap_pos = seg0 - 3.0 * self.sps;
        for _ in 0..7 {
            eq.push(derot_at(self, tap_pos, carr_ph)?);
            tap_pos += self.sps;
        }

        let step = std::f32::consts::TAU / (1u32 << s.bps_per_sym) as f32;
        // Process one symbol: window already contains the lookahead;
        // exec, train (known T bit) or slice (data), update the carrier
        // loop, push the next sample.
        let mut symbol = |this: &Self,
                          eq: &mut Lms,
                          tap_pos: &mut f64,
                          train: Option<u8>|
         -> Option<Complex<f32>> {
            let y = eq.exec();
            let perr = match train {
                Some(bit) => {
                    let d = Complex::new(if bit == 1 { -1.0 } else { 1.0 }, 0.0);
                    eq.step(d, y);
                    (y * d.conj()).arg()
                }
                None => {
                    // Phase error to the nearest constellation point.
                    let ang = y.arg();
                    ang - (ang / step).round() * step
                }
            };
            carr_fr += 0.002 * perr;
            carr_ph += carr_fr + 0.08 * perr;
            eq.push(derot_at(this, *tap_pos, carr_ph)?);
            *tap_pos += this.sps;
            Some(y)
        };

        // Train over the 9 preamble T segments (135 known BPSK symbols).
        for _ in 0..9 {
            for &tb in t.iter() {
                symbol(self, &mut eq, &mut tap_pos, Some(tb))?;
            }
        }

        let flips = fec::scramble_flips(s.data_segments() * 30);
        let m_levels = 1u32 << s.bps_per_sym;
        let mut soft: Vec<f32> = Vec::with_capacity(s.chips());
        let mut data_idx = 0usize;
        for _ in 0..s.data_segments() {
            for _ in 0..30 {
                let y = symbol(self, &mut eq, &mut tap_pos, None)?;
                let mut ang = y.arg();
                if flips[data_idx] == 1 {
                    ang += PI;
                }
                data_idx += 1;
                // Per-bit soft decisions over the Gray ring.
                for bit in 0..s.bps_per_sym {
                    soft.push(gray_soft(ang, m_levels, bit));
                }
            }
            // Retrain on the embedded T segment.
            for &tb in t.iter() {
                symbol(self, &mut eq, &mut tap_pos, Some(tb))?;
            }
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
                            if let Some((_, metric)) = self.coherent_with_energy(m1_pos, &bits, theta) {
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
                    // +6 symbols: the LMS equalizer looks 3.5 symbols ahead of the
                    // last trained T symbol.
                    let end = a1_pos + (total as f64 + 6.0) * self.sps;
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

#[cfg(test)]
mod lms_tests {
    use super::*;

    #[test]
    fn lms_converges_on_scalar_channel() {
        // Channel = pure rotation+gain (1.46j); BPSK symbols; the eq must
        // converge to y ≈ d within ~100 trained symbols.
        let h = Complex::new(0.0, 1.46f32);
        let mut rng = 0x12345u64;
        let mut bit = || {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((rng >> 33) & 1) as u8
        };
        let mut eq = Lms::new(7, 0.10);
        // Prime 7 samples.
        let mut syms: Vec<f32> = Vec::new();
        for _ in 0..7 {
            let b = bit();
            let s = if b == 1 { -1.0 } else { 1.0 };
            syms.push(s);
            eq.push(h * s);
        }
        // The decision corresponds to the window center (3 lookahead).
        let mut errs = Vec::new();
        for n in 0..200 {
            let y = eq.exec();
            let d_sym = syms[syms.len() - 4]; // center of 7-tap window
            let d = Complex::new(d_sym, 0.0);
            eq.step(d, y);
            errs.push((d - y).norm());
            let b = bit();
            let s = if b == 1 { -1.0 } else { 1.0 };
            syms.push(s);
            eq.push(h * s);
            let _ = n;
        }
        let early: f32 = errs[..20].iter().sum::<f32>() / 20.0;
        let late: f32 = errs[180..].iter().sum::<f32>() / 20.0;
        eprintln!("early err {early:.3} late err {late:.3}");
        assert!(late < 0.2, "LMS must converge (late err {late})");
    }
}
