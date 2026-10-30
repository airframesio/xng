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

use crate::avlc;
use crate::header::{self, HEADER_BITS};
use crate::interleave;
use crate::scramble::Scrambler;
use num_complex::Complex;
use std::f32::consts::PI;
use std::sync::atomic::{AtomicUsize, Ordering as AOrd};

/// Failure-funnel counters for off-air studies (see examples/offair.rs).
pub static STAT_FIT_PASS: AtomicUsize = AtomicUsize::new(0);
pub static STAT_HDR_FAIL: AtomicUsize = AtomicUsize::new(0);
pub static STAT_RS_FAIL: AtomicUsize = AtomicUsize::new(0);
pub static STAT_SOFT_OK: AtomicUsize = AtomicUsize::new(0);
pub static STAT_BURST_OK: AtomicUsize = AtomicUsize::new(0);
/// RS-corrected bursts that carried no FCS-valid AVLC frame (RS miscorrection).
pub static STAT_FCS_FAIL: AtomicUsize = AtomicUsize::new(0);
/// Collections abandoned mid-burst by the energy-dropout gate (PERF-21):
/// the header passed FEC, but received power fell near the noise floor for
/// several consecutive blocks before the declared length was reached — a
/// false lock whose tail decayed into silence (or a pure noise lock with a
/// large bogus declared length), not a real transmission.
pub static STAT_COLLECT_DROPOUT: AtomicUsize = AtomicUsize::new(0);
use xng_dsp::rs::ReedSolomon;

pub const SYMBOL_RATE: f64 = 10_500.0;
/// Representative VDL2 carrier (band 136.7–137.0 MHz) for converting a
/// measured CFO in Hz to ppm — exact channel doesn't matter at ppm scale.
const VDL2_BAND_HZ: f64 = 136_975_000.0;

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

const CORR_THRESHOLD: f32 = 0.6;
/// Maximum weighted residual variance (rad²) of the coherent preamble
/// fit for a candidate to count as a UW (true preambles on the off-air
/// capture fit below ~0.11; random data above ~0.5).
const FIT_COST_MAX: f32 = 0.25;
/// `Collect`'s energy-dropout gate (PERF-21): block size in symbols. Short
/// enough that a false lock whose tail decays into silence (or that locks
/// on a dip between bursts) is caught within a block or two instead of
/// running up to the header-declared length; 32 symbols is ≈3 ms at
/// 100 kS/s, far shorter than any real inter-burst gap this matters for.
const DROPOUT_BLOCK_SYMS: usize = 32;
/// Reject (abandon) a `Collect` once `DROPOUT_BLOCKS` consecutive blocks
/// average below this multiple of the hunt's noise-floor estimate at lock
/// time (`Collecting::lock_noise`, a snapshot of `Vdl2Demod::noise` — the
/// same value the hunt's energy gate uses). Chosen from measured
/// block-power/floor ratios (measured with `examples/vdl2_late.rs`, PERF-21);
/// every genuine decode across two off-air fixtures and a synthetic sweep (30/20 dB
/// in-channel SNR, isolated and busy-channel) had a worst-case block ratio
/// of 29.3 (and otherwise ≥41); `DROPOUT_RATIO = 8.0` leaves a 3.7× margin
/// below that floor while still catching the great majority of tail/noise
/// locks, whose blocks sit within ~1–2× the floor once they're off any real
/// signal.
const DROPOUT_RATIO: f32 = 8.0;
/// Consecutive low blocks required before abandoning (PERF-21): no observed
/// genuine decode ever had even a single block below `DROPOUT_RATIO` (so
/// `DROPOUT_BLOCKS = 1` was also zero-false-positive on every measured
/// corpus), but requiring two independent confirmations costs nothing in
/// practice and guards against a one-off noisy reading mid-burst.
const DROPOUT_BLOCKS: u32 = 2;
const ENERGY_FACTOR: f32 = 12.0;
const NOISE_ALPHA: f32 = 1e-4;
const PHASE_GAIN: f32 = 0.1;
/// Channel samples per noise-floor block (0.64 ms at 100 kS/s): short
/// enough that a 5 ms inter-burst gap holds several noise-only blocks.
const FLOOR_BLOCK: usize = 64;
/// Floor subsampling stride: the channel is oversampled ~5× against its
/// noise bandwidth, so every 4th sample keeps the block statistics.
const FLOOR_STEP: usize = 4;
/// Sliding window of block means the reference floor is a percentile of.
const FLOOR_WINDOW_S: f64 = 1.0;
/// Percentile of the window's block means taken as the reference floor: it
/// stays on noise while more than 5% of the window's blocks are idle.
const FLOOR_PERCENTILE: f64 = 0.05;
/// Ceiling on the tracking floor as a multiple of the reference. On
/// stationary noise the tracking floor sits ~1.75× the reference (2–6× on
/// the non-stationary vdl2_105k fixture), so the ceiling only engages when
/// the tracking floor has been dragged toward signal power (occupancy ×
/// burst power, a second stable state of the below-gate EMA).
const FLOOR_CEILING_RATIO: f32 = 8.0;
/// Blocks whose minimum seeds the tracking floor (replacing the 1e-6 start,
/// which left the gate open on noise for seconds after every cold start).
const FLOOR_SEED_BLOCKS: usize = 8;
/// Floor histogram: 0.25 dB bins over [-160, +40] dB.
const FLOOR_BINS_PER_DB: f32 = 4.0;
const FLOOR_DB_MIN: f32 = -160.0;
const FLOOR_BINS: usize = 800;

/// Dip-robust reference noise floor: a low percentile of block mean powers
/// over the last `FLOOR_WINDOW_S`. Averaging over a block removes the deep
/// single-sample noise dips that make a window minimum useless as a gate
/// reference; the percentile ignores bursts. Block means live in a ring
/// (arrival order) and a log histogram, so a completed block costs one
/// increment and one decrement, and the percentile is read once per chunk.
/// Exactly zero samples (zero-padded file starts) are skipped.
struct NoiseFloor {
    block_sum: f32,
    block_len: usize,
    /// Input position modulo `FLOOR_STEP`, keeping the subsample grid
    /// continuous across chunks.
    phase: usize,
    /// Histogram bin of each completed block, in arrival order.
    ring: Box<[u16]>,
    next: usize,
    filled: usize,
    hist: Box<[u32]>,
    /// Minimum of the first `FLOOR_SEED_BLOCKS` block means.
    seed_min: f32,
    /// Seed for the tracking floor, until taken.
    seed: Option<f32>,
    /// The percentile (linear power), once `FLOOR_SEED_BLOCKS` blocks exist.
    value: Option<f32>,
}

impl NoiseFloor {
    fn new(channel_rate: f64) -> Self {
        let blocks = (FLOOR_WINDOW_S * channel_rate / FLOOR_BLOCK as f64).ceil().max(1.0) as usize;
        Self {
            block_sum: 0.0,
            block_len: 0,
            phase: 0,
            ring: vec![0; blocks].into_boxed_slice(),
            next: 0,
            filled: 0,
            hist: vec![0; FLOOR_BINS].into_boxed_slice(),
            seed_min: f32::MAX,
            seed: None,
            value: None,
        }
    }

    fn push(&mut self, input: &[Complex<f32>]) {
        let start = (FLOOR_STEP - self.phase) % FLOOR_STEP;
        for x in input.iter().skip(start).step_by(FLOOR_STEP) {
            let p = x.norm_sqr();
            if p == 0.0 {
                continue;
            }
            self.block_sum += p;
            self.block_len += 1;
            if self.block_len == FLOOR_BLOCK / FLOOR_STEP {
                self.complete_block();
            }
        }
        self.phase = (self.phase + input.len()) % FLOOR_STEP;
        self.refresh();
    }

    fn complete_block(&mut self) {
        let mean = self.block_sum / (FLOOR_BLOCK / FLOOR_STEP) as f32;
        self.block_sum = 0.0;
        self.block_len = 0;
        if self.filled < FLOOR_SEED_BLOCKS {
            self.seed_min = self.seed_min.min(mean);
            if self.filled + 1 == FLOOR_SEED_BLOCKS {
                self.seed = Some(self.seed_min);
            }
        }
        let bin = ((10.0 * mean.log10() - FLOOR_DB_MIN) * FLOOR_BINS_PER_DB)
            .clamp(0.0, (FLOOR_BINS - 1) as f32) as usize;
        if self.filled == self.ring.len() {
            self.hist[usize::from(self.ring[self.next])] -= 1;
        } else {
            self.filled += 1;
        }
        self.ring[self.next] = bin as u16;
        self.next = (self.next + 1) % self.ring.len();
        self.hist[bin] += 1;
    }

    fn refresh(&mut self) {
        if self.filled < FLOOR_SEED_BLOCKS {
            return;
        }
        let target = ((self.filled - 1) as f64 * FLOOR_PERCENTILE).round() as u32;
        let mut acc = 0u32;
        for (b, &n) in self.hist.iter().enumerate() {
            acc += n;
            if acc > target {
                let db = FLOOR_DB_MIN + (b as f32 + 0.5) / FLOOR_BINS_PER_DB;
                self.value = Some(10f32.powf(db / 10.0));
                return;
            }
        }
    }
}

struct Collecting {
    /// Start of the UW that began this collection (+1 sample is the
    /// re-hunt point when the burst turns out to be a false lock).
    uw_start: f64,
    /// Absolute sample position of the next symbol center.
    next_pos: f64,
    /// Per-symbol carrier rotation estimate (radians); PLL-tracked during
    /// collection.
    theta: f32,
    /// The initial preamble-fit carrier-rotation estimate (rad/symbol),
    /// preserved (unlike `theta`, which drifts) for the burst's freq skew.
    cfo: f32,
    prev: Complex<f32>,
    /// Descrambled bits collected so far.
    bits: Vec<u8>,
    /// Per-symbol decision confidence: |phase residual| at the π/4 grid
    /// (small = confident). One entry per data symbol.
    conf: Vec<f32>,
    scr: Scrambler,
    /// Total bits to collect once the header is decoded.
    needed: Option<usize>,
    /// Energy-dropout gate (PERF-21): running power sum of the current
    /// `DROPOUT_BLOCK_SYMS`-symbol block, and how many symbols it holds.
    dropout_energy: f32,
    dropout_syms: u32,
    /// Consecutive blocks whose mean power fell below the gate.
    dropout_low_run: u32,
    /// The hunt's noise-floor estimate (`Vdl2Demod::noise`) at the moment
    /// this lock was accepted, not re-read live: the floor is also updated
    /// once per `process()` call from the chunk's own samples (including,
    /// once a long collection has run a while, this very burst's own
    /// power), so a live read could end up comparing the burst to itself.
    /// A snapshot at lock time is also truer to "the hunt's noise floor":
    /// what the gate saw when it accepted this candidate, not whatever it
    /// has drifted to since.
    lock_noise: f32,
}

/// Why a `Collect` ended without a decoded burst.
enum CollectFail {
    /// The header-declared length failed its FEC check.
    Header,
    /// The energy-dropout gate fired (PERF-21): see `DROPOUT_RATIO`.
    Dropout,
}

enum State {
    Hunt,
    Collect(Box<Collecting>),
}

pub struct Vdl2Demod {
    /// Refined UW position of the last collection that failed RS: a
    /// re-hunt that refines back to the same position is skipped past
    /// instead of retried (the decode is deterministic — retrying the
    /// identical burst livelocks until the noise floor rises).
    last_rs_fail: f64,
    sps: f64,
    /// Channel samples; index 0 is absolute sample `start_abs`.
    buf: Vec<Complex<f32>>,
    start_abs: f64,
    /// Hunt cursor (absolute).
    cursor: f64,
    /// Tracking noise floor: below-gate EMA with a slow up-creep, seeded
    /// from and capped by `floor`.
    noise: f32,
    floor: NoiseFloor,
    state: State,
    level: f32,
    /// Optional CFO reject: candidates whose preamble-fit carrier offset
    /// exceeds this many ppm (relative to the VDL2 band) are skipped (VDL2-7).
    max_ppm: Option<f64>,
    /// Conjugated expected UW phasor per Δφ (index 0 unused).
    uw_ref_conj: [Complex<f32>; 16],
    /// Cumulative UW phase ramp for the preamble fit.
    uw_ramp: [f32; 16],
    /// Bit / confidence buffers of the last finished collection, reused by
    /// the next one so steady-state bursts do not reallocate.
    spare: (Vec<u8>, Vec<f32>),
    /// `VDL2_DEBUG` set (read once at construction).
    debug: bool,
    /// `VDL2_DUMP_BITS` directory (read once at construction).
    dump_dir: Option<String>,
}

/// A demodulated, descrambled, RS-corrected burst: the AVLC bit stream.
pub struct Burst {
    pub bits: Vec<u8>,
    /// FCS-valid AVLC frames scanned from `bits` (never empty).
    pub frames: Vec<avlc::AvlcFrame>,
    pub rs_corrected: usize,
    /// Carrier frequency offset (Hz) measured from the preamble fit (VDL2-7).
    pub freq_skew_hz: f32,
    /// EVM-derived per-burst SNR (dB) from the symbol decision residuals (VDL2-8).
    pub snr_db: f32,
}

impl Vdl2Demod {
    pub fn new(channel_rate: f64) -> Self {
        let mut uw_ref_conj = [Complex::new(0.0f32, 0.0); 16];
        let mut uw_ramp = [0.0f32; 16];
        for k in 1..16 {
            uw_ref_conj[k] = Complex::from_polar(1.0, UW_DELTAS[k] as f32 * PI / 4.0).conj();
            uw_ramp[k] = uw_ramp[k - 1] + UW_DELTAS[k] as f32 * PI / 4.0;
        }
        Self {
            uw_ref_conj,
            uw_ramp,
            spare: (Vec::new(), Vec::new()),
            debug: std::env::var("VDL2_DEBUG").is_ok(),
            dump_dir: std::env::var("VDL2_DUMP_BITS").ok(),
            sps: channel_rate / SYMBOL_RATE,
            buf: Vec::new(),
            start_abs: 0.0,
            cursor: 0.0,
            noise: 1e-6,
            floor: NoiseFloor::new(channel_rate),
            state: State::Hunt,
            last_rs_fail: f64::NEG_INFINITY,
            level: 0.0,
            max_ppm: None,
        }
    }

    /// Set the CFO reject threshold (ppm relative to the VDL2 band); `None`
    /// disables it (the default — every CFO-fit candidate is accepted).
    pub fn set_max_ppm(&mut self, ppm: Option<f64>) {
        self.max_ppm = ppm;
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
        for (j, &expected_conj) in self.uw_ref_conj.iter().enumerate().skip(1) {
            let s = self.sample(pos + j as f64 * self.sps)?;
            let d = s * prev.conj();
            prev = s;
            corr += d * expected_conj;
            let dn = d.norm();
            norm += dn;
            min_d = min_d.min(dn);
        }
        if norm < 1e-9 {
            return None;
        }
        // Weak per-symbol energy consistency gate: a lock straddling the
        // burst edge has near-zero products against silence (kill it), but
        // at 4.76 samples/symbol real preambles legitimately dip on phase
        // transitions — the original 0.25·mean gate rejected most off-air
        // bursts on the sigidwiki capture (2 vs 11 frames decoded), and
        // even 0.05 costs real bursts; 0.01 keeps the edge protection
        // without the sensitivity loss.
        let mean_d = norm / (UW_DELTAS.len() - 1) as f32;
        if min_d < 0.01 * mean_d {
            return Some((0.0, 0.0));
        }
        Some((corr.norm() / norm, corr.arg()))
    }

    /// Coherent preamble fit (dumpvdl2's approach, in least-squares
    /// form): over a fine timing grid around `pos`, compare the
    /// unwrapped per-symbol phase trajectory of the 16 UW symbols
    /// against the known cumulative UW phase ramp, and fit
    /// residual ≈ a + b·k weighted by sample energy. The minimum-cost
    /// grid point jointly yields timing, carrier phase (a, absorbed by
    /// the differential decisions) and per-symbol CFO (b) — far less
    /// noisy than the argument of the differential correlation, which
    /// only uses 15 transitions non-coherently.
    /// Returns (uw_pos, theta) or None when the buffer cannot cover the
    /// search window yet.
    fn preamble_fit(&self, pos: f64) -> Option<(f64, f32, f32)> {
        let pr = &self.uw_ramp;
        let mut best: Option<(f32, f64, f32)> = None; // (cost, pos, theta)
        // Search ±0.7 symbol (at least ±3 samples): the differential
        // trigger localizes the UW no better than a fraction of a symbol,
        // and its peak width in samples scales with the channel rate.
        let half = (0.63 * self.sps).max(3.0);
        let mut t = -half;
        while t <= half {
            let cand = pos + t;
            t += 0.25;
            if cand < self.start_abs {
                continue;
            }
            let mut r = [0.0f32; 16];
            let mut w = [0.0f32; 16];
            for k in 0..16 {
                let s = self.sample(cand + k as f64 * self.sps)?;
                r[k] = s.arg() - pr[k];
                w[k] = s.norm_sqr();
            }
            // Sequential unwrap of the residual trajectory.
            for k in 1..16 {
                let mut d = r[k] - r[k - 1];
                while d > PI {
                    r[k] -= 2.0 * PI;
                    d = r[k] - r[k - 1];
                }
                while d < -PI {
                    r[k] += 2.0 * PI;
                    d = r[k] - r[k - 1];
                }
            }
            // Weighted least squares r_k ≈ a + b·k.
            let sw: f32 = w.iter().sum();
            if sw < 1e-12 {
                continue;
            }
            let kbar = w.iter().enumerate().map(|(k, &wk)| wk * k as f32).sum::<f32>() / sw;
            let rbar = w.iter().zip(&r).map(|(&wk, &rk)| wk * rk).sum::<f32>() / sw;
            let mut num = 0.0f32;
            let mut den = 0.0f32;
            for k in 0..16 {
                let dk = k as f32 - kbar;
                num += w[k] * dk * (r[k] - rbar);
                den += w[k] * dk * dk;
            }
            if den < 1e-12 {
                continue;
            }
            let b = num / den;
            let a = rbar - b * kbar;
            let mut cost = 0.0f32;
            for k in 0..16 {
                let e = r[k] - a - b * k as f32;
                cost += w[k] * e * e;
            }
            cost /= sw;
            if best.map(|(c, _, _)| cost < c).unwrap_or(true) {
                best = Some((cost, cand, b));
            }
        }
        best.map(|(c, p, th)| (p, th, c))
    }

    /// Hunt for a UW; returns the refined first-UW-symbol position.
    fn hunt(&mut self) -> Option<(f64, f32)> {
        // Span covers the preamble-fit search window (pos+3 + 15 symbols).
        let span = 19.0 * self.sps;
        let end_abs = self.start_abs + self.buf.len() as f64 - span - 4.0;
        while self.cursor < end_abs {
            let pos = self.cursor;
            self.cursor += 1.0;
            let rel = (pos - self.start_abs) as usize;
            let p = self.buf[rel].norm_sqr();
            // Gated noise estimator: never learn the floor from burst
            // power (a strong burst otherwise inflates the floor for
            // ~0.1 s and shadows rapid back-to-back transmissions —
            // measured: 17 → 20+ frames on the off-air capture).
            if p < self.noise * ENERGY_FACTOR {
                self.noise += NOISE_ALPHA * (p - self.noise);
                continue;
            }
            // Tiny up-creep so the floor can re-converge if it ever
            // starts far too low.
            self.noise *= 1.0 + NOISE_ALPHA * 0.1;
            if let Some((metric, _)) = self.uw_correlate(pos) {
                if metric > CORR_THRESHOLD {
                    // Coherent refinement: joint timing/CFO fit over the
                    // whole preamble (the differential metric's peak is
                    // broad and its CFO estimate noisy — both degrade
                    // every later symbol decision). The fit residual
                    // arbitrates true UWs; with collection-time buffer
                    // retention, an accepted false candidate costs only
                    // wasted work, never a lost burst.
                    if let Some((p, th, cost)) = self.preamble_fit(pos) {
                        if cost < FIT_COST_MAX {
                            // CFO reject (VDL2-7): `th` is the per-symbol
                            // carrier rotation (rad/symbol); convert to a ppm
                            // offset against the ~137 MHz band and skip bursts
                            // beyond the limit, continuing the hunt.
                            if let Some(max) = self.max_ppm {
                                let cfo_hz = th as f64 * SYMBOL_RATE / std::f64::consts::TAU;
                                let ppm = cfo_hz.abs() / VDL2_BAND_HZ * 1e6;
                                if ppm > max {
                                    continue;
                                }
                            }
                            STAT_FIT_PASS.fetch_add(1, AOrd::Relaxed);
                            return Some((p, th));
                        }
                    }
                }
            }
        }
        None
    }

    /// Advance a collection; returns Some(result) when the burst ends.
    /// `Ok(bits)` = complete, `Err(CollectFail)` = abandoned (header FEC
    /// failure, or the PERF-21 energy-dropout gate).
    fn collect(&self, c: &mut Collecting) -> Option<Result<(), CollectFail>> {
        loop {
            if c.needed.is_none() && c.bits.len() >= HEADER_BITS {
                let hdr: [u8; HEADER_BITS] = c.bits[..HEADER_BITS].try_into().unwrap();
                // Real VDL2 bursts top out near a couple of thousand
                // bits; a huge length is a false lock whose bogus header
                // passed FEC — collecting seconds of "burst" for it
                // starves hunting and pollutes the failure statistics.
                match header::decode(&hdr)
                    .filter(|&tl| tl <= 16_000)
                    .and_then(|tl| interleave::layout(tl as usize))
                {
                    Some(lay) => {
                        let n = HEADER_BITS + lay.total_tx_bits;
                        c.needed = Some(n);
                        // Whole symbols: up to 2 bits past `n`.
                        c.bits.reserve((n + 2).saturating_sub(c.bits.len()));
                        c.conf.reserve((n / 3 + 1).saturating_sub(c.conf.len()));
                    }
                    None => return Some(Err(CollectFail::Header)),
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
            // Energy-dropout gate (PERF-21), post-header only: a false lock
            // whose tail decays into silence (or a pure noise lock that
            // happened to pass the header FEC with a large bogus declared
            // length) shows received power near the hunt's own noise-floor
            // estimate within a block or two; a real transmission holds
            // signal power for its whole length. This costs one
            // multiply-accumulate per symbol and nothing extra on the
            // common, fast-resolving path.
            if c.needed.is_some() {
                c.dropout_energy += s.norm_sqr();
                c.dropout_syms += 1;
                if c.dropout_syms as usize == DROPOUT_BLOCK_SYMS {
                    let mean = c.dropout_energy / DROPOUT_BLOCK_SYMS as f32;
                    c.dropout_low_run =
                        if mean < DROPOUT_RATIO * c.lock_noise { c.dropout_low_run + 1 } else { 0 };
                    c.dropout_energy = 0.0;
                    c.dropout_syms = 0;
                    if c.dropout_low_run >= DROPOUT_BLOCKS {
                        return Some(Err(CollectFail::Dropout));
                    }
                }
            }
            let d = s * c.prev.conj();
            c.prev = s;
            c.next_pos += self.sps;
            let ph = d.arg() - c.theta;
            let idx_f = (ph / (PI / 4.0)).round();
            let idx = (idx_f as i32).rem_euclid(8) as usize;
            let residual = ph - idx_f * (PI / 4.0);
            c.theta += PHASE_GAIN * residual;
            c.conf.push(residual.abs());
            let (x, y, z) = GRAY_INV[idx];
            for b in [x, y, z] {
                c.bits.push(b ^ c.scr.next_bit());
            }
        }
    }

    /// Keep a finished collection's buffers (emptied) for the next one.
    fn recycle(&mut self, c: Collecting) {
        let Collecting { mut bits, mut conf, .. } = c;
        bits.clear();
        conf.clear();
        self.spare = (bits, conf);
    }

    pub fn process(&mut self, input: &[Complex<f32>], rs: &ReedSolomon) -> Vec<Burst> {
        self.buf.extend_from_slice(input);
        for x in input {
            self.level += NOISE_ALPHA * (x.norm_sqr() - self.level);
        }
        self.floor.push(input);
        if let Some(seed) = self.floor.seed.take() {
            self.noise = self.noise.max(seed);
        }
        if let Some(f) = self.floor.value {
            self.noise = self.noise.min(f * FLOOR_CEILING_RATIO);
        }
        let mut out = Vec::new();

        loop {
            match std::mem::replace(&mut self.state, State::Hunt) {
                State::Hunt => match self.hunt() {
                    Some((uw_pos, _)) if (uw_pos - self.last_rs_fail).abs() < 1.5 => {
                        // Deterministic re-detection of a burst that
                        // already failed RS: skip past its UW entirely.
                        self.cursor = uw_pos + 17.0 * self.sps;
                    }
                    Some((uw_pos, theta)) => {
                        let last_uw = uw_pos + 15.0 * self.sps;
                        let prev = self.sample(last_uw).unwrap();
                        self.state = State::Collect(Box::new(Collecting {
                            uw_start: uw_pos,
                            next_pos: last_uw + self.sps,
                            theta,
                            cfo: theta, // preamble-fit CFO, kept undrifted
                            prev,
                            bits: std::mem::take(&mut self.spare.0),
                            conf: std::mem::take(&mut self.spare.1),
                            scr: Scrambler::new(),
                            needed: None,
                            dropout_energy: 0.0,
                            dropout_syms: 0,
                            dropout_low_run: 0,
                            lock_noise: self.noise,
                        }));
                    }
                    None => break, // need more samples
                },
                State::Collect(mut c) => match self.collect(&mut c) {
                    None => {
                        self.state = State::Collect(c);
                        break; // need more samples
                    }
                    Some(Err(CollectFail::Header)) => {
                        STAT_HDR_FAIL.fetch_add(1, AOrd::Relaxed);
                        // Bad header: resume hunting just past this UW.
                        self.recycle(*c);
                        self.state = State::Hunt;
                    }
                    Some(Err(CollectFail::Dropout)) => {
                        if self.debug {
                            eprintln!("DROPOUT uw={}", c.uw_start);
                        }
                        STAT_COLLECT_DROPOUT.fetch_add(1, AOrd::Relaxed);
                        // Same rewind as an RS/FCS failure: the header
                        // passed FEC on this lock, so it is deterministic
                        // and would immediately re-dropout if re-hunted
                        // from the same position.
                        self.last_rs_fail = c.uw_start;
                        self.cursor = c.uw_start + 1.0;
                        self.recycle(*c);
                        self.state = State::Hunt;
                    }
                    Some(Ok(())) => {
                        let n = c.needed.unwrap();
                        let hdr: [u8; HEADER_BITS] = c.bits[..HEADER_BITS].try_into().unwrap();
                        let tl_bits = header::decode(&hdr).unwrap() as usize;
                        match interleave::deinterleave_soft(
                            &c.bits[HEADER_BITS..n],
                            &c.conf,
                            HEADER_BITS,
                            tl_bits,
                            rs,
                        )
                        .map(|(avlc_bits, fixed, soft)| {
                            let frames = avlc::scan(&avlc_bits);
                            (avlc_bits, fixed, soft, frames)
                        }) {
                            Some((avlc_bits, fixed, soft, frames)) if !frames.is_empty() => {
                                if self.debug {
                                    eprintln!("RSOK   tl_bits={tl_bits} syms={} soft={soft}", n / 3);
                                }
                                STAT_BURST_OK.fetch_add(1, AOrd::Relaxed);
                                if soft {
                                    STAT_SOFT_OK.fetch_add(1, AOrd::Relaxed);
                                }
                                let freq_skew_hz =
                                    (c.cfo as f64 * SYMBOL_RATE / std::f64::consts::TAU) as f32;
                                out.push(Burst {
                                    bits: avlc_bits,
                                    frames,
                                    rs_corrected: fixed,
                                    freq_skew_hz,
                                    snr_db: evm_snr_db(&c.conf),
                                });
                                // An erasure-assisted pass may be a
                                // miscorrection (the AVLC FCS arbitrates);
                                // never let it swallow a later burst —
                                // rewind like an RS failure instead of
                                // skipping ahead.
                                if soft {
                                    self.last_rs_fail = c.uw_start;
                                    self.cursor = c.uw_start + 1.0;
                                } else {
                                    self.cursor = c.next_pos; // skip past the burst
                                }
                            }
                            Some(_) => {
                                // RS passed but no frame has a valid FCS. RS
                                // alone can miscorrect a false lock (e.g. on a
                                // burst's decaying tail); skipping its
                                // header-declared length would swallow the
                                // real bursts inside it, so rewind like an RS
                                // failure.
                                if self.debug {
                                    eprintln!("FCSFAIL tl_bits={tl_bits} syms={}", n / 3);
                                }
                                STAT_FCS_FAIL.fetch_add(1, AOrd::Relaxed);
                                self.last_rs_fail = c.uw_start;
                                self.cursor = c.uw_start + 1.0;
                            }
                            None => {
                                if self.debug {
                                    eprintln!("RSFAIL tl_bits={tl_bits} syms={}", n / 3);
                                }
                                if let Some(dir) = &self.dump_dir {
                                    use std::sync::atomic::AtomicU32;
                                    static N: AtomicU32 = AtomicU32::new(0);
                                    let k = N.fetch_add(1, AOrd::Relaxed);
                                    let _ = std::fs::write(
                                        format!(
                                            "{dir}/rx_tl{tl_bits}_{k}_at{}.bits",
                                            c.uw_start as u64
                                        ),
                                        &c.bits[HEADER_BITS..n],
                                    );
                                    let conf_txt: String = c
                                        .conf
                                        .iter()
                                        .map(|v| format!("{v:.3}\n"))
                                        .collect();
                                    let _ = std::fs::write(
                                        format!(
                                            "{dir}/rx_tl{tl_bits}_{k}_at{}.conf",
                                            c.uw_start as u64
                                        ),
                                        conf_txt,
                                    );
                                }
                                STAT_RS_FAIL.fetch_add(1, AOrd::Relaxed);
                                self.last_rs_fail = c.uw_start;
                                // A false UW lock (e.g. on a burst edge) can
                                // pass the header FEC with a bogus length and
                                // swallow the real burst; resume right after
                                // the false UW start so the true preamble
                                // (which may begin within those 16 symbols)
                                // is retried.
                                self.cursor = c.uw_start + 1.0;
                            }
                        }
                        self.recycle(*c);
                        self.state = State::Hunt;
                    }
                },
            }
        }

        // Drop consumed samples (keep a tail behind the active position).
        // While collecting, retain everything back to the UW that started
        // the collection: if the header was a false decode with a bogus
        // length, the RS failure rewinds the hunt to uw_start + 1, and
        // any real burst inside the consumed span must still be in the
        // buffer. Worst case (max TL ≈ 6240 symbols) this holds ~150 KB
        // of samples at the 50 kHz channel rate.
        let active = match &self.state {
            State::Collect(c) => c.uw_start.min(c.next_pos),
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

/// EVM-derived SNR (dB) from the per-symbol phase-decision residuals (radians)
/// accumulated during a burst: `snr ≈ 1 / EVM²`, with `EVM² = mean(residual²)`
/// (small-angle PSK). A pure quality metric — higher means cleaner symbols.
fn evm_snr_db(residuals: &[f32]) -> f32 {
    if residuals.is_empty() {
        return 0.0;
    }
    let evm2 = residuals.iter().map(|r| r * r).sum::<f32>() / residuals.len() as f32;
    -10.0 * evm2.max(1e-9).log10()
}
