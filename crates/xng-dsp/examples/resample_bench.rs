//! Throughput benchmark for the streaming fractional [`Resampler`] across the
//! rate ratios and block sizes the decoders actually run.
//!
//! Usage:
//!
//! ```text
//! cargo run --release -p xng-dsp --example resample_bench
//! ```
//!
//! Each config resamples 4 s of deterministic complex noise at its input rate
//! in fixed-size blocks (output `Vec` reused), and reports the best of 7 runs
//! after one warm-up. "% core" is wall time over capture time — the share of
//! one core this stage costs in real time.

use std::hint::black_box;
use std::time::{Duration, Instant};
use xng_dsp::{IqSample, Resampler};

const SECONDS: usize = 4;
const RUNS: usize = 7;
const BLOCKS: &[usize] = &[256, 2_730, 65_536];

/// (input rate, output rate, description)
const CONFIGS: &[(f64, f64, &str)] = &[
    (100_000.0, 105_000.0, "VDL2 channelized 2.4M/24"),
    (150_000.0, 105_000.0, "VDL2 channelized post-AA"),
    (2_500_000.0 / 24.0, 105_000.0, "VDL2 channelized 2.5M/24"),
    (2_500_000.0 / 52.0, 48_000.0, "AIS 2.5M/52"),
    (2_500_000.0 / 104.0, 24_000.0, "ACARS 2.5M/104"),
    (50_000.0, 48_000.0, "AIS 2.4M/48"),
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
    in_rate: f64,
    out_rate: f64,
    block: usize,
    input: &[IqSample],
    out: &mut Vec<IqSample>,
) -> (Duration, usize, usize) {
    let mut rs = Resampler::new(in_rate, out_rate);
    let mut outputs = 0;
    let mut calls = 0;
    let start = Instant::now();
    for blk in input.chunks(block) {
        out.clear();
        rs.process(black_box(blk), out);
        outputs += out.len();
        calls += 1;
        black_box(&*out);
    }
    (start.elapsed(), outputs, calls)
}

fn main() {
    println!(
        "{:>10} {:>10} {:>6} {:>9} {:>10} {:>10} {:>8}  config",
        "in rate", "out rate", "block", "ms", "ns/output", "ns/call", "% core"
    );
    let mut out = Vec::with_capacity(BLOCKS.iter().copied().max().unwrap_or(0) * 2);
    for &(in_rate, out_rate, what) in CONFIGS {
        let input = noise(in_rate as usize * SECONDS);
        for &block in BLOCKS {
            run_once(in_rate, out_rate, block, &input, &mut out);
            let mut best = Duration::MAX;
            let mut outputs = 0;
            let mut calls = 0;
            for _ in 0..RUNS {
                let (t, n, c) = run_once(in_rate, out_rate, block, &input, &mut out);
                best = best.min(t);
                outputs = n;
                calls = c;
            }
            let ms = best.as_secs_f64() * 1e3;
            let ns_per_out = best.as_secs_f64() * 1e9 / outputs as f64;
            let ns_per_call = best.as_secs_f64() * 1e9 / calls as f64;
            let core = best.as_secs_f64() / SECONDS as f64 * 100.0;
            println!(
                "{in_rate:>10.0} {out_rate:>10.0} {block:>6} {ms:>9.3} {ns_per_out:>10.2} \
                 {ns_per_call:>10.1} {core:>7.3}%  {what}"
            );
        }
    }
}
