//! Throughput benchmark for the polyphase filter-bank channelizer
//! ([`PfbChannelizer`]) at the channel counts the decoders run.
//!
//! Usage:
//!
//! ```text
//! cargo run --release -p xng-dsp --example pfb_bench
//! ```
//!
//! Each config channelizes 4 s of deterministic complex noise at 2.4 MS/s in
//! 65 536-sample blocks (output `Vec`s reused) with 16 taps per branch, and
//! reports the best of 7 runs after one warm-up. "% core" is wall time over
//! capture time — the share of one core this costs in real time.
//!
//! The "ifft" column times the same rustfft inverse plan alone, once per
//! output block, so "rest" (total − ifft) is the polyphase sum plus the
//! buffering / output-push overhead.
//!
//! The "M/act" rows (e.g. `48/16`) restrict output to that many evenly spread
//! bins with `set_active_bins`, as `ChannelizedDdc` does for its channels.
//!
//! The second table runs the 2× oversampled bank (`Oversample::Twice`, one
//! output per `M/2` inputs) at the taps per branch `ChannelizedDdc` picks for
//! it; its "ifft" column times twice as many transforms.

use rustfft::{FftDirection, FftPlanner};
use std::hint::black_box;
use std::time::{Duration, Instant};
use xng_dsp::{IqSample, Oversample, PfbChannelizer};

const SECONDS: usize = 4;
const RATE: usize = 2_400_000;
const BLOCK: usize = 65_536;
const RUNS: usize = 7;
const TAPS_PER_BRANCH: usize = 16;
/// (bins, active bins); active == bins means all active (the default).
const CHANNELS: &[(usize, usize)] = &[(24, 24), (24, 7), (48, 48), (48, 16), (96, 96)];
/// 2× oversampled: (bins, taps per branch, active bins).
const OVERSAMPLED: &[(usize, usize, usize)] = &[
    (48, 8, 48),
    (48, 8, 7),
    (96, 6, 96),
    (96, 6, 8),
    (96, 7, 96),
    (96, 7, 8),
    (192, 7, 192),
    (192, 7, 8),
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

fn run_pfb(nch: usize, active: usize, input: &[IqSample], out: &mut [Vec<IqSample>]) -> Duration {
    run_pfb_with(PfbChannelizer::new(nch, TAPS_PER_BRANCH), active, input, out)
}

fn run_pfb_with(
    mut pfb: PfbChannelizer,
    active: usize,
    input: &[IqSample],
    out: &mut [Vec<IqSample>],
) -> Duration {
    let nch = pfb.num_channels();
    if active < nch {
        pfb.set_active_bins(&(0..active).map(|i| i * nch / active).collect::<Vec<_>>());
    }
    let start = Instant::now();
    for blk in input.chunks(BLOCK) {
        for o in out.iter_mut() {
            o.clear();
        }
        pfb.process(blk, out);
        black_box(&*out);
    }
    start.elapsed()
}

fn run_ifft(nch: usize, steps: usize, buf: &mut [IqSample]) -> Duration {
    let ifft = FftPlanner::new().plan_fft(nch, FftDirection::Inverse);
    let start = Instant::now();
    for _ in 0..steps {
        ifft.process(buf);
        black_box(&*buf);
    }
    start.elapsed()
}

fn best_of(mut f: impl FnMut() -> Duration) -> Duration {
    f();
    (0..RUNS).map(|_| f()).min().unwrap_or(Duration::MAX)
}

fn main() {
    let input = noise(RATE * SECONDS);
    println!(
        "{:>6} {:>9} {:>9} {:>9} {:>11} {:>8}",
        "M/act", "total ms", "ifft ms", "rest ms", "ns/in samp", "% core"
    );
    for &(nch, active) in CHANNELS {
        let mut out: Vec<Vec<IqSample>> = (0..nch)
            .map(|_| Vec::with_capacity(BLOCK / nch + 1))
            .collect();
        let total = best_of(|| run_pfb(nch, active, &input, &mut out));
        // Repeated unnormalized IDFTs overflow to inf/NaN, which (unlike
        // subnormals) costs nothing extra on SSE/AVX, so no rescaling.
        let mut buf = noise(nch);
        let ifft = best_of(|| run_ifft(nch, input.len() / nch, &mut buf));
        let ms = total.as_secs_f64() * 1e3;
        let ifft_ms = ifft.as_secs_f64() * 1e3;
        let ns = total.as_secs_f64() * 1e9 / input.len() as f64;
        let core = total.as_secs_f64() / SECONDS as f64 * 100.0;
        let label = if active < nch { format!("{nch}/{active}") } else { nch.to_string() };
        println!(
            "{label:>6} {ms:>9.3} {ifft_ms:>9.3} {:>9.3} {ns:>11.3} {core:>7.3}%",
            ms - ifft_ms
        );
    }

    println!("\n2x oversampled:");
    println!(
        "{:>7} {:>4} {:>9} {:>9} {:>9} {:>11} {:>8}",
        "M/act", "tpb", "total ms", "ifft ms", "rest ms", "ns/in samp", "% core"
    );
    for &(nch, tpb, active) in OVERSAMPLED {
        let mut out: Vec<Vec<IqSample>> =
            (0..nch).map(|_| Vec::with_capacity(2 * BLOCK / nch + 1)).collect();
        let total = best_of(|| {
            let pfb = PfbChannelizer::with_oversample(nch, tpb, Oversample::Twice);
            run_pfb_with(pfb, active, &input, &mut out)
        });
        let mut buf = noise(nch);
        let ifft = best_of(|| run_ifft(nch, 2 * input.len() / nch, &mut buf));
        let ms = total.as_secs_f64() * 1e3;
        let ifft_ms = ifft.as_secs_f64() * 1e3;
        let ns = total.as_secs_f64() * 1e9 / input.len() as f64;
        let core = total.as_secs_f64() / SECONDS as f64 * 100.0;
        let label = if active < nch { format!("{nch}/{active}") } else { nch.to_string() };
        println!(
            "{label:>7} {tpb:>4} {ms:>9.3} {ifft_ms:>9.3} {:>9.3} {ns:>11.3} {core:>7.3}%",
            ms - ifft_ms
        );
    }
}
