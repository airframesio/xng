//! Iridium DQPSK burst demodulator (single 25 kHz channel at 10
//! samples/symbol). Pipeline facts from gr-iridium/iridium-sniffer (GPL,
//! facts only): tone preamble → 12-symbol UW (DL/UL) → payload, 25 000
//! sym/s, RRC matched filter, differential decode with
//! `dqpsk_map = [0,2,3,1]`, two MSB-first bits per mapped symbol.
//!
//! Acquisition reuses the coherent preamble fit proven on VDL2/HFDL:
//! after a power trigger and tone-based coarse CFO, the 12 known UW
//! symbols (absolute QPSK phases) are fit jointly for timing, carrier
//! phase, and residual CFO over a fine grid.

use crate::frame::{ACCESS_DL, ACCESS_UL};
use num_complex::Complex;
use std::f32::consts::PI;

pub const SYMBOL_RATE: f64 = 25_000.0;
/// UW absolute QPSK symbol phases (units of π/2).
const UW_DL: [u8; 12] = [0, 2, 2, 2, 2, 0, 0, 0, 2, 0, 0, 2];
const UW_UL: [u8; 12] = [2, 2, 0, 0, 0, 2, 0, 0, 2, 0, 2, 2];
/// Differential symbol → mapped symbol (gr-iridium).
const DQPSK_MAP: [u8; 4] = [0, 2, 3, 1];
/// Maximum burst length in symbols (90 ms at 25 ksym/s).
const MAX_BURST_SYMS: usize = 2_250;
/// Preamble tone length to search across (long preamble = 64 symbols).
const PRE_SYMS: f64 = 64.0;

/// One demodulated burst: bits (beginning at the access code) and the
/// measured carrier offset within the channel.
pub struct DemodBurst {
    pub bits: Vec<u8>,
    pub cfo_hz: f64,
}

pub struct IridiumDemod {
    sps: f64,
    buf: Vec<Complex<f32>>,
    start_abs: f64,
    cursor: f64,
    noise: f32,
    /// 16-sample boxcar of power for the burst gate (single noise
    /// samples have an exponential tail; bursts are sustained).
    pwr_win: [f32; 16],
    pwr_pos: usize,
    pwr_sum: f32,
    level: f32,
    /// Emit per-trigger acquisition diagnostics (XNG_IRIDIUM_DEBUG set).
    debug: bool,
}

impl IridiumDemod {
    pub fn new(channel_rate: f64) -> Self {
        let sps = channel_rate / SYMBOL_RATE;
        Self {
            sps,
            buf: Vec::new(),
            start_abs: 0.0,
            cursor: 0.0,
            // Initialized high: falls to the true floor within ~1000
            // samples (the asymmetric EMA falls fast, rises slowly).
            noise: 1.0,
            pwr_win: [0.0; 16],
            pwr_pos: 0,
            pwr_sum: 0.0,
            level: 0.0,
            debug: std::env::var("XNG_IRIDIUM_DEBUG").is_ok(),
        }
    }

    /// Seed the noise floor. The asymmetric EMA starts at 1.0 and needs
    /// ~1400 samples of quiet to converge; a continuous stream gives it
    /// that, but an isolated wideband-extracted burst has only a short
    /// pre-roll, so without seeding the floor freezes ~18 dB high when the
    /// burst arrives and the acquisition gate (`noise·8`) sits above the
    /// signal. The wideband front end seeds this from the channel's own
    /// measured noise.
    pub fn seed_noise(&mut self, noise: f32) {
        self.noise = noise.max(1e-12);
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

    /// Coarse CFO from the preamble tone: FFT peak over the tone window.
    fn tone_cfo(&self, pos: f64, syms: f64) -> Option<f64> {
        let n = (syms * self.sps) as usize;
        let rel = (pos - self.start_abs) as usize;
        if rel + n > self.buf.len() {
            return None;
        }
        let mut best = (0.0f32, 0.0f64);
        // Coarse DFT scan ±30 kHz in 50 Hz steps (tone is strong; the
        // wideband front end's detection centroid can sit tens of kHz
        // off the true channel center under spectral leakage).
        let mut f = -30_000.0;
        while f <= 30_000.0 {
            let mut acc = Complex::new(0.0f32, 0.0);
            let step = -2.0 * std::f64::consts::PI * f / (SYMBOL_RATE * self.sps / 1.0);
            for (k, s) in self.buf[rel..rel + n].iter().enumerate() {
                acc += s * Complex::from_polar(1.0, (step * k as f64) as f32);
            }
            let m = acc.norm();
            if m > best.0 {
                best = (m, f);
            }
            f += 50.0;
        }
        Some(best.1)
    }

    /// Coherent UW fit (timing/phase/CFO jointly) for one UW variant.
    /// Returns (cost, uw_pos, theta_per_symbol, carrier_phase).
    fn uw_fit(&self, pos: f64, uw: &[u8; 12], theta0: f32) -> Option<(f32, f64, f32, f32)> {
        let mut best: Option<(f32, f64, f32, f32)> = None;
        let mut t = -self.sps;
        while t <= self.sps {
            let cand = pos + t;
            t += 0.25;
            if cand < self.start_abs {
                continue;
            }
            let mut r = [0.0f32; 12];
            let mut w = [0.0f32; 12];
            let mut prev = 0.0f32;
            for k in 0..12 {
                let s = self.sample(cand + k as f64 * self.sps)?;
                let expect = uw[k] as f32 * PI / 2.0;
                let mut ph = (s * Complex::from_polar(1.0, -expect - theta0 * k as f32)).arg();
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
            let mut num = 0.0;
            let mut den = 0.0;
            for k in 0..12 {
                let dk = k as f32 - kbar;
                num += w[k] * dk * (r[k] - rbar);
                den += w[k] * dk * dk;
            }
            if den < 1e-12 {
                continue;
            }
            let b = num / den;
            let a = rbar - b * kbar;
            let mut cost = 0.0;
            for (k, (&wk, &rk)) in w.iter().zip(&r).enumerate() {
                let e = rk - a - b * k as f32;
                cost += wk * e * e;
            }
            cost /= sw;
            if best.map(|(c, _, _, _)| cost < c).unwrap_or(true) {
                best = Some((cost, cand, theta0 + b, a));
            }
        }
        best
    }

    /// Feed channel samples; returns demodulated bursts.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<DemodBurst> {
        self.buf.extend_from_slice(input);
        for x in input {
            self.level += 1e-4 * (x.norm_sqr() - self.level);
        }
        let mut out = Vec::new();
        let span = (PRE_SYMS + 14.0 + MAX_BURST_SYMS as f64) * self.sps;
        loop {
            let end_abs = self.start_abs + self.buf.len() as f64 - span;
            if self.cursor >= end_abs {
                break;
            }
            let pos = self.cursor;
            self.cursor += 1.0;
            let rel = (pos - self.start_abs) as usize;
            let p = self.buf[rel].norm_sqr();
            // Asymmetric noise floor (burst sweep must not lift it).
            if p < self.noise {
                self.noise += 0.01 * (p - self.noise);
            } else {
                self.noise += 1e-5 * (p - self.noise);
            }
            self.pwr_sum += p - self.pwr_win[self.pwr_pos];
            self.pwr_win[self.pwr_pos] = p;
            self.pwr_pos = (self.pwr_pos + 1) % 16;
            if self.pwr_sum < self.noise * 8.0 * 16.0 {
                continue;
            }
            // The boxcar trigger fires ~16 samples into the burst; the
            // tone preamble starts at roughly pos − 16. Measure CFO on a
            // window that is certainly inside the tone, then hunt the UW
            // over the possible preamble lengths (16 short, 64 long).
            let bstart = pos - 16.0;
            let Some(cfo) = self.tone_cfo(bstart + 2.0 * self.sps, 10.0) else { break };
            let theta0 = (2.0 * std::f64::consts::PI * cfo / SYMBOL_RATE) as f32;
            let burst_gate = self.noise * 8.0;
            let mut found: Option<(f32, f64, f32, f32, &'static [u8; 24])> = None;
            let mut dbg_best = f32::INFINITY;
            let mut dbg_energetic = 0u32;
            let mut hunt = 6.0 * self.sps;
            while hunt < (PRE_SYMS + 26.0) * self.sps {
                let cand = bstart + hunt;
                hunt += 2.0 * self.sps;
                // The whole 12-symbol window must carry burst power.
                let mut energetic = true;
                for k in 0..12 {
                    match self.sample(cand + k as f64 * self.sps) {
                        Some(s) if s.norm_sqr() > burst_gate => {}
                        Some(_) => {
                            energetic = false;
                            break;
                        }
                        None => {
                            energetic = false;
                            break;
                        }
                    }
                }
                if !energetic {
                    continue;
                }
                dbg_energetic += 1;
                for (uw, access) in [(&UW_DL, ACCESS_DL), (&UW_UL, ACCESS_UL)] {
                    if let Some((cost, p2, th, ph)) = self.uw_fit(cand, uw, theta0) {
                        dbg_best = dbg_best.min(cost);
                        if cost < 0.05 && found.map(|(c, ..)| cost < c).unwrap_or(true) {
                            found = Some((cost, p2, th, ph, access));
                        }
                    }
                }
                if found.is_some() && hunt > 10.0 * self.sps {
                    break;
                }
            }
            if self.debug {
                eprintln!(
                    "  trigger @ {:.0} (t={:.4}s): noise {:.2e} gate {:.2e}, cfo {:+.0} Hz, {} energetic windows, best UW cost {:.4}{}",
                    pos,
                    pos / (SYMBOL_RATE * self.sps),
                    self.noise,
                    burst_gate,
                    cfo,
                    dbg_energetic,
                    dbg_best,
                    if found.is_some() { "  SYNC" } else { "" }
                );
            }
            let Some((_, uw_pos, theta, phase, access)) = found else {
                // No UW: skip past this energy region.
                self.cursor = pos + 32.0 * self.sps;
                continue;
            };

            // Demodulate symbols until the burst power drops or max len.
            let mut symbols: Vec<u8> = Vec::new();
            let mut k = 0usize;
            let mut carr = phase;
            loop {
                let sp = uw_pos + k as f64 * self.sps;
                let Some(s) = self.sample(sp) else { break };
                if k >= 12 {
                    // End of burst: power gone for this symbol.
                    if s.norm_sqr() < self.noise * 4.0 {
                        break;
                    }
                }
                let derot = s * Complex::from_polar(1.0, -(theta * k as f32) - carr);
                // Hard QPSK decision in units of π/2.
                let ang = derot.arg();
                // Signed rounding so the residual wraps correctly at ±π
                // (q·π/2 after mod-4 would put −π against +π → −2π).
                let idx_f = (ang / (PI / 2.0)).round();
                let q = (idx_f as i32).rem_euclid(4) as u8;
                // Decision-directed phase trim (PLL α≈0.2 per gr-iridium).
                let residual = ang - idx_f * (PI / 2.0);
                carr += 0.2 * residual;
                symbols.push(q);
                k += 1;
                if k >= 12 + MAX_BURST_SYMS {
                    break;
                }
            }
            if symbols.len() < 12 + 32 {
                self.cursor = uw_pos + symbols.len().max(1) as f64 * self.sps;
                continue;
            }

            // Differential decode (the first UW symbol acts as reference;
            // toolkit bit streams start at the access code, i.e. the UW
            // itself differentially decoded from old_sym = 0).
            let mut bits = Vec::with_capacity(symbols.len() * 2);
            let mut old = 0u8;
            for &s in &symbols {
                let d = (s + 4 - old) % 4;
                old = s;
                let m = DQPSK_MAP[d as usize];
                bits.push(m >> 1);
                bits.push(m & 1);
            }
            // Sanity: access code must match what the UW fit promised.
            if bits.len() >= 24 && bits[..24] == access[..] {
                // Total carrier offset: per-symbol rotation → Hz.
                let cfo_hz = theta as f64 * SYMBOL_RATE / std::f64::consts::TAU;
                out.push(DemodBurst { bits, cfo_hz });
            }
            self.cursor = uw_pos + symbols.len() as f64 * self.sps;
        }

        // Drop consumed samples.
        let keep_from = (self.cursor - self.start_abs - 4.0 * self.sps).max(0.0) as usize;
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
