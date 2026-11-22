//! VDL2 burst emission latency (PERF-21): a scheduled run of many distinct
//! bursts at a known sample timeline, decoded by the unmodified
//! `Vdl2ChannelDecoder` public API, records how many samples after each
//! burst's true end its frame comes out of `process()`. A false UW lock on
//! the decaying tail of the previous burst can pass the header FEC and hold
//! `Collect` on garbage until RS/FCS fails and rewinds; the real burst
//! behind it then only reaches the AVLC scan after the demod's own re-hunt
//! finds it, which shows up here directly as emission latency. No demod
//! instrumentation is needed: everything measured is either external timing
//! (the burst schedule is known) or the already-public
//! `demod::STAT_FIT_PASS` / `STAT_HDR_FAIL` / `STAT_RS_FAIL` /
//! `STAT_FCS_FAIL` / `STAT_BURST_OK` / `STAT_SOFT_OK` failure-funnel
//! counters, so this harness works unmodified against HEAD — including
//! against variants that add their OWN abandon path with a new, harness-blind
//! counter (e.g. the energy-dropout gate's `demod::STAT_COLLECT_DROPOUT`):
//! see `Stats::other_abandon`'s doc for what it can and cannot tell you
//! about such a counter without importing it.
//!
//! A new example rather than a mode of an existing lab: `burstlab` and
//! `sensitivity`/`sensitivity_wide` exercise one isolated burst (or one
//! burst at a sweep of noise), and `offair`/`offair_multi` replay a fixed
//! capture — none of them has a synthetic *scheduled, many-burst* timeline,
//! which is the thing lateness needs (a burst is only "late" relative to
//! when the next burst would otherwise have reached the AVLC scan).
//!
//! Two modes:
//!
//! ```bash
//! cargo run --release -p xng-mode-vdl2 --example vdl2_late            # scan: full SNR x gap sweep
//! cargo run --release -p xng-mode-vdl2 --example vdl2_late -- trace   # trace: fine event localization
//! ```
//!
//! `scan` feeds each (SNR, gap) configuration in realistic read-sized
//! chunks (`CHUNK`, chosen to match the live multi-channel plan's
//! DDC-decimated chunk size: the runtime reads `READ_CHUNK = 65_536`
//! wideband samples per call — see `src/runtime.rs` — and a 2.4 MS/s capture
//! at the live 7-channel plan decimates 24:1 to the 100 kS/s channel rate,
//! i.e. ≈2_730 channel samples per call; `tests/noise_floor.rs` uses the
//! same value for the same reason). It reports, per configuration: bursts
//! decoded/missed, % emitted later than the 83 ms PERF-21 threshold,
//! latency percentiles, and the failure-funnel accounting: `false_locks()`
//! (`STAT_HDR_FAIL` + `STAT_RS_FAIL` + `STAT_FCS_FAIL`, each exact — every
//! one of those increments is a real, counted event) and
//! `false_locks_past_header()` (the `STAT_RS_FAIL` + `STAT_FCS_FAIL`
//! subset). `STAT_FIT_PASS` does NOT equal the sum of those three plus
//! `STAT_BURST_OK` even on an unmodified demod — see `Stats::other_abandon`'s
//! doc for the (harmless, pre-existing) reason.
//!
//! `scan` also prints the SNR convention it uses (`print_snr_convention`):
//! in-channel, referenced to the symbol rate, matching
//! `sensitivity_wide.rs` / `noise_floor.rs` / `end_to_end.rs` — NOT the
//! same as `examples/sensitivity.rs`'s full-channel convention (no
//! symbol-rate normalization), which reads lower for the same noise; a
//! TODO.md note written from this harness's numbers should say which one.
//!
//! `trace` reruns one configuration at a much smaller burst count through 32
//! sample sub-chunks (fine enough to localize every `STAT_FIT_PASS` /
//! `STAT_HDR_FAIL` / `STAT_RS_FAIL` / `STAT_FCS_FAIL` / `STAT_BURST_OK` event
//! to within 32 samples) and prints, for each event, its offset from the
//! nearest preceding burst end — directly checking the PERF-21 hypothesis
//! that false locks cluster within ~40 samples of a burst's end — plus,
//! joining consecutive events, how long each false `Collect` ran before it
//! resolved.
use num_complex::Complex;
use std::sync::atomic::Ordering;
use xng_mode_vdl2::avlc::{encode_address, AddressType};
use xng_mode_vdl2::demod::{
    STAT_BURST_OK, STAT_FCS_FAIL, STAT_FIT_PASS, STAT_HDR_FAIL, STAT_RS_FAIL, STAT_SOFT_OK,
    SYMBOL_RATE,
};
use xng_mode_vdl2::modulate::burst_iq_shaped;
use xng_mode_vdl2::{Vdl2ChannelDecoder, CHANNEL_RATE_HI};

const FS: f64 = CHANNEL_RATE_HI;
/// PERF-21's lateness threshold, in samples at `FS`.
const LATE_SAMPLES: i64 = (0.083 * FS) as i64;
/// Realistic read chunk: see the module doc.
const CHUNK: usize = 2_730;
const LEAD: usize = 20_000;
const TAIL: usize = 100_000;
const SEED: u64 = 0x5eed_1546_2021_0001;

/// Fixed-seed uniform +/-1 noise (xorshift64), as in `tests/noise_floor.rs`
/// and `examples/sensitivity_wide.rs`; the in-channel SNR formula below
/// assumes this distribution (per-component variance amp^2/3).
struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f32 / u64::MAX as f32) * 2.0 - 1.0
    }
}

/// Downlink ACARS-over-AVLC UI frame, unique per `seq` (aircraft address and
/// text both vary), so a decoded `AvlcFrame::raw` identifies which
/// scheduled burst it came from (same shape as `tests/busy_traffic.rs`).
fn aoa_frame(seq: usize) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend(encode_address(AddressType::GroundIcao, 0x10A234, false, false));
    f.extend(encode_address(AddressType::Aircraft, 0xA0_0000 + seq as u32, false, true));
    f.push(0x03); // UI
    f.push(0xFF); // AOA IPI
    f.extend(xng_acars::block::build(
        '2',
        "N700XG",
        None,
        "H1",
        '3',
        Some(&format!("M{:02}A", seq % 100)),
        Some("XG0700"),
        &format!("CH0 SEQ {seq:04} LATE HARNESS PAYLOAD TEXT"),
        false,
    ));
    f
}

/// Snapshot of the demod's public failure-funnel counters (process-wide,
/// monotonic — a difference between two snapshots is the event count that
/// occurred between them, valid as long as only one decoder runs at a time).
#[derive(Clone, Copy, Default)]
struct Stats {
    fit_pass: usize,
    hdr_fail: usize,
    rs_fail: usize,
    fcs_fail: usize,
    burst_ok: usize,
    soft_ok: usize,
}

impl Stats {
    fn snapshot() -> Self {
        Self {
            fit_pass: STAT_FIT_PASS.load(Ordering::Relaxed),
            hdr_fail: STAT_HDR_FAIL.load(Ordering::Relaxed),
            rs_fail: STAT_RS_FAIL.load(Ordering::Relaxed),
            fcs_fail: STAT_FCS_FAIL.load(Ordering::Relaxed),
            burst_ok: STAT_BURST_OK.load(Ordering::Relaxed),
            soft_ok: STAT_SOFT_OK.load(Ordering::Relaxed),
        }
    }

    fn sub(&self, base: &Self) -> Self {
        Self {
            fit_pass: self.fit_pass - base.fit_pass,
            hdr_fail: self.hdr_fail - base.hdr_fail,
            rs_fail: self.rs_fail - base.rs_fail,
            fcs_fail: self.fcs_fail - base.fcs_fail,
            burst_ok: self.burst_ok - base.burst_ok,
            soft_ok: self.soft_ok - base.soft_ok,
        }
    }

    /// Locks resolved by the original four failure-funnel buckets (header
    /// FEC failure, RS failure, RS-passed-but-no-valid-FCS) or a decode.
    fn resolved4(&self) -> usize {
        self.hdr_fail + self.rs_fail + self.fcs_fail + self.burst_ok
    }

    /// `fit_pass` minus `resolved4`: NOT a clean "abandoned some other way"
    /// signal — confirmed (by direct instrumentation while building this
    /// harness) to be dominated by a pre-existing, harmless property of the
    /// UNMODIFIED demod's accounting, present on every demod variant: `hunt`
    /// increments `STAT_FIT_PASS` before `process`'s deterministic
    /// re-detection check (`(uw_pos - self.last_rs_fail).abs() < 1.5`,
    /// `src/demod.rs`) can skip a candidate it already knows failed, without
    /// that skip ever reaching one of the four tracked buckets. This is
    /// common (tens of events per busy config) and does not indicate a
    /// missed or mishandled burst — `missed`/`late` above are unaffected.
    /// A demod variant's own NEW abandon path (e.g. the energy-dropout
    /// gate's `demod::STAT_COLLECT_DROPOUT`) also contributes here, but is
    /// NOT separable from the dedup artifact by this generic count alone —
    /// read the variant-specific counter directly (it doesn't exist on an
    /// unmodified demod, so this harness does not import it) for an exact
    /// count.
    fn other_abandon(&self) -> usize {
        self.fit_pass.saturating_sub(self.resolved4())
    }

    /// Locks that did not resolve into a decoded frame.
    fn false_locks(&self) -> usize {
        self.hdr_fail + self.rs_fail + self.fcs_fail + self.other_abandon()
    }

    /// Of those, the ones that passed the header FEC (reached Collect's
    /// RS/FCS stage, or were abandoned some other way, instead of failing
    /// the header immediately — `other_abandon` is always post-header: see
    /// its doc).
    fn false_locks_past_header(&self) -> usize {
        self.rs_fail + self.fcs_fail + self.other_abandon()
    }
}

/// One scheduled burst's ground truth.
struct Truth {
    frame: Vec<u8>,
    end: usize,
}

/// Build `n` distinct bursts at `snr_db` in-channel SNR (see
/// `examples/sensitivity_wide.rs` for the formula), separated by
/// `gap_samples` of idle noise (0 = back-to-back).
fn build_capture(n: usize, snr_db: f64, gap_samples: usize) -> (Vec<Complex<f32>>, Vec<Truth>) {
    let frames: Vec<Vec<u8>> = (0..n).map(aoa_frame).collect();
    let bursts: Vec<Vec<Complex<f32>>> =
        frames.iter().map(|f| burst_iq_shaped(std::slice::from_ref(f), FS, 0.0, 0.5)).collect();
    let sig_pow =
        bursts[0].iter().map(|c| f64::from(c.norm_sqr())).sum::<f64>() / bursts[0].len() as f64;
    // In-channel SNR as in examples/sensitivity_wide.rs: amp = sqrt(1.5 *
    // P_sig * FS / (Rs * 10^(SNR/10))).
    let amp = (1.5 * sig_pow * FS / (SYMBOL_RATE * 10f64.powf(snr_db / 10.0))).sqrt() as f32;

    let mut iq = vec![Complex::new(0.0f32, 0.0); LEAD];
    let mut truth = Vec::with_capacity(n);
    for (i, (frame, burst)) in frames.into_iter().zip(&bursts).enumerate() {
        iq.extend_from_slice(burst);
        let end = iq.len();
        truth.push(Truth { frame, end });
        if i + 1 < n {
            iq.resize(iq.len() + gap_samples, Complex::new(0.0, 0.0));
        }
    }
    iq.resize(iq.len() + TAIL, Complex::new(0.0, 0.0));

    let mut noise = Noise(SEED);
    for s in &mut iq {
        *s += Complex::new(noise.next() * amp, noise.next() * amp);
    }
    (iq, truth)
}

/// Percentile (0..=100) of a sorted slice.
fn percentile(sorted: &[i64], p: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

struct ConfigResult {
    snr_db: f64,
    gap_samples: usize,
    n: usize,
    missed: usize,
    late: usize,
    latencies_ms: Vec<f64>,
    stats: Stats,
}

fn run_scan_config(snr_db: f64, gap_samples: usize, n: usize) -> ConfigResult {
    let (iq, truth) = build_capture(n, snr_db, gap_samples);
    let s0 = Stats::snapshot();

    let mut dec = Vdl2ChannelDecoder::new(FS, 0.0).expect("build VDL2 channel decoder");
    let mut emitted: Vec<Option<usize>> = vec![None; n];
    let mut fed = 0usize;
    for chunk in iq.chunks(CHUNK) {
        fed += chunk.len();
        for f in dec.process(chunk) {
            if let Some(i) = truth.iter().position(|t| {
                f.avlc.raw.len() == t.frame.len() + 2 && f.avlc.raw[..t.frame.len()] == t.frame[..]
            }) {
                if emitted[i].is_none() {
                    emitted[i] = Some(fed);
                }
            }
        }
    }
    let stats = Stats::snapshot().sub(&s0);

    let mut missed = 0;
    let mut late = 0;
    let mut latencies_ms = Vec::with_capacity(n);
    for (i, t) in truth.iter().enumerate() {
        match emitted[i] {
            None => missed += 1,
            Some(e) => {
                let delta = e as i64 - t.end as i64;
                if delta > LATE_SAMPLES {
                    late += 1;
                }
                latencies_ms.push(delta as f64 / FS * 1000.0);
            }
        }
    }
    ConfigResult { snr_db, gap_samples, n, missed, late, latencies_ms, stats }
}

fn print_scan_header() {
    println!(
        "{:>6}{:>9}{:>7}{:>8}{:>8}{:>9}{:>9}{:>9}{:>9}{:>10}{:>9}{:>9}{:>10}{:>9}",
        "SNR",
        "gap_ms",
        "n",
        "missed",
        ">83ms",
        "p50 ms",
        "p90 ms",
        "p99 ms",
        "max ms",
        "fit_pass",
        "f_lock",
        "pastHdr",
        "burst_ok",
        "other"
    );
}

fn print_scan_row(r: &ConfigResult) {
    let mut sorted = r.latencies_ms.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let to_i64 = |v: &[f64]| -> Vec<i64> { v.iter().map(|x| (*x * 1000.0) as i64).collect() };
    let sorted_us = to_i64(&sorted); // microsecond-resolution percentile helper
    let p = |pc: f64| percentile(&sorted_us, pc) as f64 / 1000.0;
    println!(
        "{:>6.0}{:>9.1}{:>7}{:>8}{:>8}{:>9.3}{:>9.3}{:>9.3}{:>9.3}{:>9}{:>9}{:>9}{:>10}{:>9}",
        r.snr_db,
        r.gap_samples as f64 / FS * 1000.0,
        r.n,
        r.missed,
        r.late,
        p(50.0),
        p(90.0),
        p(99.0),
        sorted.last().copied().unwrap_or(0.0),
        r.stats.fit_pass,
        r.stats.false_locks(),
        r.stats.false_locks_past_header(),
        r.stats.burst_ok,
        r.stats.other_abandon(),
    );
    // `other` (`Stats::other_abandon`) is normal, non-zero output even on an
    // unmodified demod — see that method's doc for why it is not a clean
    // "abandoned some other way" count, just an upper bound that also
    // includes a demod variant's own new abandon path.
    if r.stats.other_abandon() > 0 {
        println!(
            "  note: {} lock(s) resolved outside the four tracked buckets (fit_pass {} vs \
             resolved4 {}) — see Stats::other_abandon's doc (usually the baseline demod's own \
             deterministic-redetection dedup, not a sign of trouble)",
            r.stats.other_abandon(),
            r.stats.fit_pass,
            r.stats.resolved4()
        );
    }
}

/// `n` bursts/config: enough for single-digit-percent lateness rates to show
/// up as double-digit sample counts without the sweep taking minutes.
const SCAN_BURSTS: usize = 1_500;
// `15 dB` per the PERF-21 request is below this demod's absolute
// sensitivity floor at the in-channel-SNR convention used throughout this
// crate's tests (`examples/sensitivity_wide.rs`, `tests/noise_floor.rs`,
// `tests/end_to_end.rs`: SNR referenced to the 10.5 kBd symbol-rate
// bandwidth) — isolated-burst bisection found 0/40 decoded at 15 dB and
// the 50%-decode point at ~18 dB, matching the
// "~18.6 dB" figure in `src/lib.rs`'s `ChannelBack::new` doc. At 15 dB
// nothing decodes at any gap, which makes "late" undefined, so the noisy
// tier here is 20 dB (≈97.5% single-burst decodable) instead. Note
// `examples/sensitivity.rs` uses a DIFFERENT, non-in-channel SNR
// convention (signal power over total noise power at the full channel
// rate, ~9.8 dB more pessimistic for the same noise) — its "15 dB" would
// correspond to roughly 25 dB here.
const SNRS: [f64; 2] = [30.0, 20.0];
/// Gap between bursts, in samples at `FS` (100 kS/s): back-to-back, ~5 ms,
/// ~20 ms, ~100 ms.
const GAPS: [usize; 4] = [0, 500, 2_000, 10_000];

/// SNR convention used by this harness (and by `sensitivity_wide.rs`,
/// `noise_floor.rs`, `end_to_end.rs`): signal power over the noise's power
/// spectral density integrated across one symbol-rate (`SYMBOL_RATE`)
/// bandwidth, i.e. `SNR_in = P_sig / (P_noise_total * SYMBOL_RATE / FS)`.
/// `examples/sensitivity.rs` instead uses `SNR_full = P_sig / P_noise_total`
/// (no symbol-rate normalization) — the two differ by
/// `10*log10(FS/SYMBOL_RATE)`, a constant ≈9.79 dB at this harness's 100 kS/s
/// (it scales with the channel rate `FS` chosen, not just a fixed offset in
/// general). Printed here so a TODO.md note written from this harness's
/// numbers names the right convention, and so the two don't get silently
/// compared against each other again.
fn print_snr_convention() {
    let full_channel_offset_db = 10.0 * (FS / SYMBOL_RATE).log10();
    println!(
        "SNR convention: in-channel (referenced to the {:.1} kBd symbol rate), \
         matching examples/sensitivity_wide.rs / tests/noise_floor.rs / tests/end_to_end.rs.\n\
         examples/sensitivity.rs's full-channel convention (no symbol-rate normalization) reads \
         {:.2} dB LOWER for the same noise at this harness's {FS:.0} S/s \
         (SNR_full = SNR_in - 10*log10(FS/Rs)); e.g. this harness's {:.0} dB in-channel \
         is {:.1} dB full-channel.",
        SYMBOL_RATE / 1000.0,
        full_channel_offset_db,
        SNRS[0],
        SNRS[0] - full_channel_offset_db,
    );
}

fn scan() {
    println!(
        "VDL2 emission latency scan — {FS:.0} S/s, {SCAN_BURSTS} bursts/config, chunk {CHUNK}"
    );
    print_snr_convention();
    println!();
    print_scan_header();
    for &snr in &SNRS {
        for &gap in &GAPS {
            let r = run_scan_config(snr, gap, SCAN_BURSTS);
            print_scan_row(&r);
        }
    }
}

/// Fine-grained localization: replay one configuration through tiny
/// sub-chunks so every failure-funnel event can be pinned to within
/// `TRACE_CHUNK` samples, and report its offset from the nearest preceding
/// burst end AND the nearest burst start — a lock near a burst's END with a
/// short `Collect` run is the PERF-21 tail-lock; a lock near a burst's START
/// that fails RS/FCS is an ordinary SNR-driven miss of that burst's own true
/// preamble, not a tail lock, even though it adds to the same `STAT_RS_FAIL`
/// / `STAT_FCS_FAIL` counters.
const TRACE_BURSTS: usize = 400;
const TRACE_CHUNK: usize = 32;
/// A lock within this many samples of the previous burst's end is
/// classified as a tail lock (PERF-8/PERF-17 found ~99% within ~40 samples).
const TAIL_WINDOW: i64 = 200;

#[derive(Clone, Copy)]
enum Event {
    FitPass,
    HdrFail,
    RsFail,
    FcsFail,
    BurstOk,
    /// `Stats::other_abandon`'s gap (see its doc): mostly the baseline
    /// demod's own deterministic-redetection dedup firing right after a
    /// fit_pass, not a resolution of this trace's own lock; a variant's own
    /// new abandon path (e.g. PERF-21's energy-dropout gate) also lands
    /// here, indistinguishably, without this harness needing to import that
    /// variant-only counter.
    Other,
}

fn trace_config(snr_db: f64, gap_samples: usize) {
    let (iq, truth) = build_capture(TRACE_BURSTS, snr_db, gap_samples);
    let s0 = Stats::snapshot();
    let mut dec = Vdl2ChannelDecoder::new(FS, 0.0).expect("build VDL2 channel decoder");
    let mut fed = 0usize;
    let mut events: Vec<(usize, Event)> = Vec::new();
    let mut prev = Stats::default();
    for chunk in iq.chunks(TRACE_CHUNK) {
        fed += chunk.len();
        dec.process(chunk);
        let now = Stats::snapshot().sub(&s0);
        let d = now.sub(&prev);
        for _ in 0..d.fit_pass {
            events.push((fed, Event::FitPass));
        }
        for _ in 0..d.hdr_fail {
            events.push((fed, Event::HdrFail));
        }
        for _ in 0..d.rs_fail {
            events.push((fed, Event::RsFail));
        }
        for _ in 0..d.fcs_fail {
            events.push((fed, Event::FcsFail));
        }
        for _ in 0..d.burst_ok {
            events.push((fed, Event::BurstOk));
        }
        let tracked = d.hdr_fail + d.rs_fail + d.fcs_fail + d.burst_ok;
        for _ in 0..d.fit_pass.saturating_sub(tracked) {
            events.push((fed, Event::Other));
        }
        prev = now;
    }

    println!(
        "\ntrace: SNR {snr_db} dB, gap {:.1} ms, {TRACE_BURSTS} bursts, {TRACE_CHUNK}-sample sub-chunks",
        gap_samples as f64 / FS * 1000.0
    );
    println!("{:>10}{:>16}{:>12}  {:<30}class", "pos", "lock_off_end", "dur(samp)", "outcome");
    // The classification keys off the ORIGINATING lock's own offset from the
    // nearest preceding burst end (the defining feature of a tail lock: the
    // UW correlation fired within `TAIL_WINDOW` samples of a real burst's
    // end), not the resolving event's position — a genuine RS/FCS failure
    // on a burst's OWN true preamble naturally resolves near THAT burst's
    // own end (one whole burst length later), which would otherwise look
    // indistinguishable from "far from the previous burst" by position
    // alone.
    let mut last_fit_pass_pos: Option<usize> = None;
    let mut last_fit_pass_offset_end: i64 = i64::MAX;
    let (mut tail_hdr, mut tail_past_hdr, mut other_rs_fcs, mut other_count) =
        (0u32, 0u32, 0u32, 0u32);
    for &(pos, ev) in &events {
        let dur = last_fit_pass_pos.map(|p| pos - p).unwrap_or(0);
        let is_tail = last_fit_pass_offset_end <= TAIL_WINDOW;
        let (label, class) = match ev {
            Event::FitPass => {
                let nearest_end =
                    truth.iter().map(|t| t.end).filter(|&e| e <= pos).max().unwrap_or(0);
                last_fit_pass_offset_end = pos as i64 - nearest_end as i64;
                last_fit_pass_pos = Some(pos);
                let class =
                    if last_fit_pass_offset_end <= TAIL_WINDOW { "near prev end" } else { "" };
                ("FIT_PASS (lock)".to_string(), class)
            }
            Event::HdrFail if is_tail => {
                tail_hdr += 1;
                (format!("HDR_FAIL (collect ran {dur} samp)"), "tail-lock, header fail")
            }
            Event::HdrFail => (format!("HDR_FAIL (collect ran {dur} samp)"), ""),
            Event::RsFail | Event::FcsFail if is_tail => {
                tail_past_hdr += 1;
                let name = if matches!(ev, Event::RsFail) { "RS_FAIL" } else { "FCS_FAIL" };
                (format!("{name} (collect ran {dur} samp)"), "tail-lock PAST HEADER (PERF-21)")
            }
            Event::RsFail | Event::FcsFail => {
                other_rs_fcs += 1;
                let name = if matches!(ev, Event::RsFail) { "RS_FAIL" } else { "FCS_FAIL" };
                (format!("{name} (collect ran {dur} samp)"), "own burst / other SNR miss")
            }
            Event::BurstOk => ("BURST_OK (real frame)".to_string(), ""),
            // See `Event::Other`'s doc: this is usually the baseline
            // demod's own dedup firing on a DIFFERENT, nearby candidate
            // right after `last_fit_pass_pos`, not a resolution of that
            // lock's own Collect, so `dur`/`is_tail` here are not
            // meaningful the way they are for the other variants.
            Event::Other => {
                other_count += 1;
                ("OTHER (dedup, or a variant's own abandon path)".to_string(), "")
            }
        };
        println!("{pos:>10}{last_fit_pass_offset_end:>16}{dur:>12}  {label:<30}{class}");
    }
    println!(
        "\nclassification: tail-lock header fails {tail_hdr}, tail-lock PAST-HEADER fails \
         (PERF-21's mechanism) {tail_past_hdr}, non-tail RS/FCS fails (ordinary SNR-driven \
         misses of a burst's own true preamble) {other_rs_fcs}, OTHER (dedup / a variant's own \
         abandon path, see Event::Other's doc) {other_count}"
    );
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    if mode == "trace" {
        // A busy, noisy configuration (20 dB, 5 ms gaps) to collect enough
        // false-lock events in a small burst count to be worth printing.
        trace_config(20.0, 500);
    } else {
        scan();
    }
}
