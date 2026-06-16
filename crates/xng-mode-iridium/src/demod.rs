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
use std::cell::RefCell;
use std::f32::consts::PI;

thread_local! {
    /// Cached FFT planner for the fine-CFO FFT. `IridiumDemod` is constructed
    /// per burst, so a per-thread cached planner avoids re-planning every time
    /// (rustfft caches plans internally, so a same-size request is cheap).
    static CFO_PLANNER: RefCell<rustfft::FftPlanner<f32>> = RefCell::new(rustfft::FftPlanner::new());
}

pub const SYMBOL_RATE: f64 = 25_000.0;
/// UW absolute QPSK symbol phases (units of π/2).
const UW_DL: [u8; 12] = [0, 2, 2, 2, 2, 0, 0, 0, 2, 0, 0, 2];
const UW_UL: [u8; 12] = [2, 2, 0, 0, 0, 2, 0, 0, 2, 0, 2, 2];
/// Differential symbol → mapped symbol (gr-iridium).
const DQPSK_MAP: [u8; 4] = [0, 2, 3, 1];
/// Maximum burst length in symbols (90 ms at 25 ksym/s).
const MAX_BURST_SYMS: usize = 2_250;
/// Tolerated bit errors in the differentially-decoded 24-bit access code. A
/// single absolute UW-symbol slip flips up to ~2 differential bits, so a small
/// tolerance lets weak-but-real bursts through; the downstream BCH/CRC rejects
/// anything spurious that slips past (gr-iridium's UW check tolerates diffs≤2).
const ACCESS_TOL: usize = 4;

/// Read an `f32` tuning knob from the environment (for sensitivity sweeps),
/// falling back to `default` when unset or unparseable.
pub(crate) fn env_f32(key: &str, default: f32) -> f32 {
    std::env::var(key).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}
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
    /// UW coherent-correlation acceptance (0..1; higher = stricter). Tunable
    /// via XNG_IRIDIUM_UWCORR for sensitivity sweeps.
    uw_corr: f32,
    /// Burst power-gate multiplier over the noise floor. Tunable via
    /// XNG_IRIDIUM_GATE.
    gate_mult: f32,
    /// Use gr-iridium's squared-FFT fine CFO (Blackman-windowed, over the
    /// preamble+UW) instead of the plain preamble-tone DFT. Squaring removes
    /// the BPSK on the UW symbols (and the alternating UL preamble), leaving a
    /// clean tone at 2·CFO. Default on; set XNG_IRIDIUM_SQCFO=0 to disable.
    sq_cfo: bool,
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
            uw_corr: env_f32("XNG_IRIDIUM_UWCORR", 0.97),
            gate_mult: env_f32("XNG_IRIDIUM_GATE", 8.0),
            // Default on (gr-iridium's squared-FFT fine CFO; +16% IDA / +19%
            // CRC-OK on the 300 s benchmark). Set XNG_IRIDIUM_SQCFO=0 to disable.
            sq_cfo: std::env::var("XNG_IRIDIUM_SQCFO").map(|v| v != "0").unwrap_or(true),
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

    /// Fine CFO from the preamble tone. A DFT scan ±30 kHz in 50 Hz steps
    /// (the tone is strong; the wideband front end's detection centroid can sit
    /// tens of kHz off the true channel center under spectral leakage), then a
    /// quadratic interpolation across the peak bin and its neighbours to recover
    /// sub-step (≈1 Hz) precision. The 50 Hz grid alone leaves up to ±25 Hz of
    /// residual CFO, which the carrier loop must chase over the whole burst;
    /// gr-iridium interpolates its CFO FFT peak the same way so the per-symbol
    /// rotation is right from the first symbol.
    fn tone_cfo(&self, pos: f64, syms: f64) -> Option<f64> {
        let n = (syms * self.sps) as usize;
        let rel = (pos - self.start_abs) as usize;
        if rel + n > self.buf.len() {
            return None;
        }
        const STEP_HZ: f64 = 50.0;
        const F0: f64 = -30_000.0;
        const NF: usize = 1201; // -30k..=30k inclusive
        let mut mags = [0.0f32; NF];
        for (i, m) in mags.iter_mut().enumerate() {
            let f = F0 + i as f64 * STEP_HZ;
            let mut acc = Complex::new(0.0f32, 0.0);
            let step = -2.0 * std::f64::consts::PI * f / (SYMBOL_RATE * self.sps);
            for (k, s) in self.buf[rel..rel + n].iter().enumerate() {
                acc += s * Complex::from_polar(1.0, (step * k as f64) as f32);
            }
            *m = acc.norm();
        }
        let peak = (0..NF).max_by(|&a, &b| mags[a].total_cmp(&mags[b]))?;
        // Quadratic interpolation of the peak (skip if it is at an edge).
        let frac = if peak > 0 && peak < NF - 1 {
            let (a, b, c) = (mags[peak - 1], mags[peak], mags[peak + 1]);
            let denom = a - 2.0 * b + c;
            if denom.abs() > 1e-12 {
                (0.5 * (a - c) / denom).clamp(-1.0, 1.0) as f64
            } else {
                0.0
            }
        } else {
            0.0
        };
        Some(F0 + (peak as f64 + frac) * STEP_HZ)
    }

    /// gr-iridium `burst_downmix` fine CFO (faithful port). Square the
    /// preamble+UW window (removes the BPSK on the UW and the alternating UL
    /// preamble, leaving a tone at 2·CFO), Blackman-window it, zero-pad 16×,
    /// FFT, take the global magnitude peak, quadratically interpolate, and
    /// halve. FFT length = floor-pow2(sps·(short preamble 16 + 10 UW)), 16×
    /// oversampled — exactly `burst_downmix_impl.cc`'s `d_cfo_est_fft`.
    fn tone_cfo_sq(&self, pos: f64) -> Option<f64> {
        let base = (self.sps * (16.0 + 10.0)) as usize; // PREAMBLE_LENGTH_SHORT + 10 UW
        if base < 4 {
            return None;
        }
        let n = 1usize << (usize::BITS - 1 - base.leading_zeros()); // floor power of two
        let m = n * 16; // d_fft_over_size_facor
        let rel = (pos - self.start_abs) as usize;
        if rel + n > self.buf.len() {
            return None;
        }
        // Square + Blackman-window the first n samples into a zero-padded buffer.
        let mut buf = vec![Complex::new(0.0f32, 0.0); m];
        let denom = (n - 1) as f32;
        for k in 0..n {
            let s = self.buf[rel + k];
            let w = 0.42 - 0.5 * (2.0 * PI * k as f32 / denom).cos()
                + 0.08 * (4.0 * PI * k as f32 / denom).cos();
            buf[k] = s * s * w;
        }
        let fft = CFO_PLANNER.with(|p| p.borrow_mut().plan_fft_forward(m));
        fft.process(&mut buf);
        // Global peak of |·|² over the oversampled spectrum, quadratic interp.
        let peak = (0..m).max_by(|&a, &b| buf[a].norm_sqr().total_cmp(&buf[b].norm_sqr()))?;
        let a = buf[(peak + m - 1) % m].norm_sqr();
        let b = buf[peak].norm_sqr();
        let c = buf[(peak + 1) % m].norm_sqr();
        let d = a - 2.0 * b + c;
        let frac = if d.abs() > 1e-20 { (0.5 * (a - c) / d).clamp(-1.0, 1.0) as f64 } else { 0.0 };
        // Unshift [0,m) → [-m/2, m/2), normalize to cycles/sample, halve (undo
        // squaring), convert to Hz.
        let mut idx = peak as f64 + frac;
        if idx >= m as f64 / 2.0 {
            idx -= m as f64;
        }
        Some(idx / m as f64 / 2.0 * (SYMBOL_RATE * self.sps))
    }

    /// Matched-filter UW correlation for one UW variant, over a fine timing
    /// grid. Returns (corr, uw_pos, theta_per_symbol, carrier_phase), where
    /// `corr` is the normalized coherent correlation in [0, 1] (1 = perfect UW
    /// match). This is gr-iridium's UW detector: derotate each sample by the
    /// expected UW symbol and the CFO, sum coherently, and normalize by the
    /// total amplitude. The carrier phase is free (absorbed by the sum's
    /// magnitude), so only one degree of freedom is fit — noise correlates near
    /// 1/√12, while a real UW (with the fine CFO removed) approaches 1. The old
    /// per-symbol phase-fit instead trusted each symbol's noisy phase and fit a
    /// free CFO slope, which let noise masquerade as a weak burst.
    fn uw_fit(&self, pos: f64, uw: &[u8; 12], theta0: f32) -> Option<(f32, f64, f32, f32)> {
        // Derotate the 12 symbols at `cand` by the expected UW and the tone CFO
        // (plus an optional residual-CFO slope `db`); return (aligned sum,
        // Σ|s|). For a real UW the terms collapse to |s|·exp(iφ).
        let project = |cand: f64, db: f32| -> Option<(Complex<f32>, f32)> {
            let mut acc = Complex::new(0.0f32, 0.0);
            let mut mag_sum = 0.0f32;
            for k in 0..12 {
                let s = self.sample(cand + k as f64 * self.sps)?;
                let expect = uw[k] as f32 * PI / 2.0;
                acc += s * Complex::from_polar(1.0, -expect - (theta0 + db) * k as f32);
                mag_sum += s.norm();
            }
            (mag_sum > 1e-12).then_some((acc, mag_sum))
        };

        // Jointly search symbol timing and a clamped residual-CFO slope: at
        // each candidate timing, estimate the residual CFO (mean symbol-to-
        // symbol phase step) and score the CFO-corrected coherent sum. The
        // joint search matters — the best timing depends on the CFO — and the
        // clamp keeps noise from fitting a large slope.
        let mut best: Option<(f32, f64, f32, f32)> = None;
        let mut t = -self.sps;
        while t <= self.sps {
            let cand = pos + t;
            t += 0.25;
            if cand < self.start_abs {
                continue;
            }
            // Differential residual-CFO estimate from the UW-derotated symbols.
            let mut dsum = Complex::new(0.0f32, 0.0);
            let mut prev = match self.sample(cand) {
                Some(s) => s * Complex::from_polar(1.0, -(uw[0] as f32 * PI / 2.0)),
                None => break,
            };
            let mut ok = true;
            for k in 1..12 {
                match self.sample(cand + k as f64 * self.sps) {
                    Some(s) => {
                        let cur =
                            s * Complex::from_polar(1.0, -(uw[k] as f32 * PI / 2.0) - theta0 * k as f32);
                        dsum += cur * prev.conj();
                        prev = cur;
                    }
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            let db = dsum.arg().clamp(-0.5, 0.5);
            let Some((acc, mag_sum)) = project(cand, db) else { continue };
            let corr = acc.norm() / mag_sum;
            if best.map(|(c, _, _, _)| corr > c).unwrap_or(true) {
                // theta = tone CFO + recovered residual; the demod's decision-
                // directed loop tracks anything left. Carrier phase = arg(acc).
                best = Some((corr, cand, theta0 + db, acc.arg()));
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
            if self.pwr_sum < self.noise * self.gate_mult * 16.0 {
                continue;
            }
            // The boxcar trigger fires ~16 samples into the burst; the
            // tone preamble starts at roughly pos − 16. Measure CFO on a
            // window that is certainly inside the tone, then hunt the UW
            // over the possible preamble lengths (16 short, 64 long).
            let bstart = pos - 16.0;
            // Fine CFO: gr-iridium's squared-FFT estimate over the preamble+UW
            // by default (finer, and handles the alternating UL preamble); the
            // plain preamble-tone DFT is the fallback path (XNG_IRIDIUM_SQCFO=0).
            let cfo = if self.sq_cfo {
                self.tone_cfo_sq(bstart)
            } else {
                self.tone_cfo(bstart + 2.0 * self.sps, 10.0)
            };
            let Some(cfo) = cfo else { break };
            let theta0 = (2.0 * std::f64::consts::PI * cfo / SYMBOL_RATE) as f32;
            let mut found: Option<(f32, f64, f32, f32, &'static [u8; 24])> = None;
            let mut dbg_best = 0.0f32;
            let mut hunt = 6.0 * self.sps;
            while hunt < (PRE_SYMS + 26.0) * self.sps {
                let cand = bstart + hunt;
                hunt += 2.0 * self.sps;
                // Cheap reject: the candidate's first symbol must carry some
                // power. The coherent UW correlation below tolerates individual
                // weak symbols, so we no longer require the whole 12-symbol
                // window above the gate (that dropped weak bursts outright).
                match self.sample(cand) {
                    Some(s) if s.norm_sqr() > self.noise => {}
                    _ => continue,
                }
                for (uw, access) in [(&UW_DL, ACCESS_DL), (&UW_UL, ACCESS_UL)] {
                    if let Some((corr, p2, th, ph)) = self.uw_fit(cand, uw, theta0) {
                        dbg_best = dbg_best.max(corr);
                        if corr > self.uw_corr && found.map(|(c, ..)| corr > c).unwrap_or(true) {
                            found = Some((corr, p2, th, ph, access));
                        }
                    }
                }
                // Stop early once a strong UW is locked (cheap on clean bursts);
                // keep hunting when only a marginal one is found so a better
                // position later in the preamble can still win.
                if found.map(|(c, ..)| c > self.uw_corr + 0.2).unwrap_or(false) && hunt > 10.0 * self.sps {
                    break;
                }
            }
            if self.debug {
                eprintln!(
                    "  trigger @ {:.0} (t={:.4}s): noise {:.2e}, cfo {:+.0} Hz, best UW corr {:.3}{}",
                    pos,
                    pos / (SYMBOL_RATE * self.sps),
                    self.noise,
                    cfo,
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
                // Decision-directed phase trim (PLL α≈0.2 per gr-iridium), but
                // only over the payload: the UW correlation already set the
                // carrier across the 12 UW symbols, so running the loop there
                // just lets it chase the UW's own residual and drift out of the
                // quadrant by the last UW symbol (corrupting the access code).
                let residual = ang - idx_f * (PI / 2.0);
                if k >= 12 {
                    carr += 0.2 * residual;
                }
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
                // Emit each symbol's two bits in iridium-toolkit
                // (`symbol_reverse`d) order so the downstream BCH
                // de-interleavers see the canonical stream. gr-iridium's
                // native "RAW" order is the un-reversed `m>>1, m&1`; only
                // the all-00/11 access code and ITL/IMS headers are
                // invariant under the swap, which is why those decode
                // either way and the BCH frames (RA/IBC/LCW/IDA) do not.
                // ITL recovers absolute symbols by inverting this in its
                // own pair read.
                bits.push(m & 1);
                bits.push(m >> 1);
            }
            // Sanity: the differentially-decoded access code must match the UW
            // the correlation locked, tolerating a few bit errors so weak-but-
            // real bursts are not dropped over a single symbol slip.
            let access_errs = bits.iter().take(24).zip(access).filter(|(a, b)| a != b).count();
            if bits.len() >= 24 && access_errs <= ACCESS_TOL {
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
