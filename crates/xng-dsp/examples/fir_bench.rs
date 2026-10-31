//! Throughput benchmark for the streaming [`Fir`] filter across the tap /
//! decimation / rate configurations the decoders actually run.
//!
//! Usage:
//!
//! ```text
//! cargo run --release -p xng-dsp --example fir_bench
//! ```
//!
//! Each config filters 4 s of deterministic complex noise at its input rate in
//! 65 536-sample blocks (output `Vec` reused), and reports the best of 7 runs
//! after one warm-up. "% core" is wall time over capture time — the share of
//! one core this filter costs in real time.

use std::hint::black_box;
use std::time::{Duration, Instant};
use xng_dsp::fir::{lowpass_taps, Fir};
use xng_dsp::IqSample;

const SECONDS: usize = 4;
const BLOCK: usize = 65_536;
const RUNS: usize = 7;

/// (taps, decimation, input rate, description)
const CONFIGS: &[(usize, usize, usize, &str)] = &[
    (101, 1, 105_000, "VDL2 selectivity no-DDC"),
    (101, 1, 100_000, "VDL2 selectivity live"),
    (121, 1, 24_000, "ACARS demod LPF"),
    (13, 2, 2_400_000, "Ddc stage 1 @2.4M"),
    (81, 12, 1_200_000, "VDL2 Ddc stage 2"),
    (59, 10, 2_400_000, "ACARS-era Ddc stage 1"),
    (95, 10, 240_000, "Ddc stage 2"),
    (21, 2, 50_000, "channelizer AA stage"),
    (777, 11, 1_200_000, "sharp final stage"),
];

fn noise(len: usize) -> Vec<IqSample> {
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    };
    (0..len).map(|_| IqSample::new(next(), next())).collect()
}

fn run_once(
    taps: &[f32],
    decim: usize,
    input: &[IqSample],
    out: &mut Vec<IqSample>,
) -> (Duration, usize) {
    let mut fir = Fir::with_decimation(taps.to_vec(), decim);
    let mut outputs = 0;
    let start = Instant::now();
    for blk in input.chunks(BLOCK) {
        out.clear();
        fir.process(blk, out);
        outputs += out.len();
        black_box(&*out);
    }
    (start.elapsed(), outputs)
}

fn main() {
    println!(
        "{:>5} {:>6} {:>10} {:>9} {:>10} {:>8}  config",
        "taps", "decim", "rate", "ms", "ns/output", "% core"
    );
    let mut out = Vec::with_capacity(BLOCK);
    for &(ntaps, decim, rate, what) in CONFIGS {
        // Cutoff a little inside the decimated Nyquist; the value does not
        // affect cost, only realism.
        let cutoff = (0.4 / decim as f64).min(0.2);
        let taps = lowpass_taps(cutoff, ntaps);
        let input = noise(rate * SECONDS);
        run_once(&taps, decim, &input, &mut out);
        let mut best = Duration::MAX;
        let mut outputs = 0;
        for _ in 0..RUNS {
            let (t, n) = run_once(&taps, decim, &input, &mut out);
            best = best.min(t);
            outputs = n;
        }
        let ms = best.as_secs_f64() * 1e3;
        let ns_per_out = best.as_secs_f64() * 1e9 / outputs as f64;
        let core = best.as_secs_f64() / SECONDS as f64 * 100.0;
        println!(
            "{ntaps:>5} {decim:>6} {rate:>10} {ms:>9.3} {ns_per_out:>10.2} {core:>7.3}%  {what}"
        );
    }
}
