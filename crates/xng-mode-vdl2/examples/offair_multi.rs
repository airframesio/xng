//! Off-air multi-channel front-end harness: place a real 105 kS/s VDL2
//! capture into the 7-channel 2.4 MS/s plan (center 136.8125 MHz) at several
//! channel offsets, decode it through every `Vdl2MultiChannelDecoder` front
//! end, and print frames decoded and decoder wall time per (offset, front
//! end):
//!
//! ```bash
//! cargo run --release -p xng-mode-vdl2 --example offair_multi
//! cargo run --release -p xng-mode-vdl2 --example offair_multi -- \
//!   bench/data/vdl2_opflasher_105k.cs16 cs16 136800000
//! ```
//!
//! Arguments (all optional): `[fixture] [cs16|cf32] [channel_hz]`. The
//! fixture must be 105 kS/s with the VDL2 channel at its center (as in the
//! `vdl2_offair` / `vdl2_offair2` rows of `bench/run.sh`, both decoded at
//! offset 0); `channel_hz` only labels the output. Defaults:
//! `bench/data/vdl2_105k_conj.s16`, `cs16`, `136975000`.
//!
//! The up-conversion is the same exact 160/7 Kaiser-windowed polyphase
//! interpolator plus mix as the `upconvert` example (no added noise), but run
//! block-wise in memory so the wide capture is never materialized.
//!
//! Offsets: the plan channels sit on the 25 kHz VDL2 raster at
//! −162.5/−87.5/−37.5/+12.5/+62.5/+112.5/+162.5 kHz from the capture center.
//! The channelizer's 100 kHz bins put +162.5, −37.5 and +62.5 kHz 37.5 kHz
//! off a bin center (in the prototype roll-off), the worst-case residual,
//! while +12.5 kHz sits 12.5 kHz off a bin center, the near-centered control.
//! Comparing the two classes shows how much of a front end's frame count
//! depends on bin-center residual rather than on the signal. The
//! `channelized-2x` row is the 2× oversampled bank, which serves every
//! channel flat wherever it falls in its bin.
//!
//! Only frames from the channel the fixture was placed on are counted; the
//! `other` column counts frames from any other plan channel (leakage — it
//! should be 0). The per-channel `Vdl2ChannelDecoder` (single DDC) is shown
//! as a reference row.

use num_complex::Complex;
use std::f64::consts::{PI, TAU};
use std::time::{Duration, Instant};
use xng_dsp::Oversample;
use xng_mode_vdl2::{Vdl2ChannelDecoder, Vdl2MultiChannelDecoder};

const IN_RATE: f64 = 105_000.0;
const FS: f64 = 2_400_000.0;
const PLAN_CENTER_HZ: f64 = 136_812_500.0;
const PLAN_OFFSETS: [f64; 7] =
    [-162_500.0, -87_500.0, -37_500.0, 12_500.0, 62_500.0, 112_500.0, 162_500.0];
/// Plan offsets the fixture is placed at (see the module doc).
const TEST_OFFSETS: [f64; 4] = [162_500.0, -37_500.0, 62_500.0, 12_500.0];
/// 2_400_000 / 105_000 = 160 / 7.
const L: usize = 160;
const M: usize = 7;
const TAPS_PER_PHASE: usize = 48;
/// Kaiser β for ≈90 dB stopband (β = 0.1102·(A − 8.7)).
const KAISER_BETA: f64 = 8.96;
const BLOCK: usize = 65_536;

/// Zeroth-order modified Bessel function of the first kind (power series).
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..200 {
        term *= q / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// Polyphase bank of the `upconvert` example's interpolation lowpass.
fn polyphase_bank() -> Vec<[f64; TAPS_PER_PHASE]> {
    let n = L * TAPS_PER_PHASE;
    let center = (n - 1) as f64 / 2.0;
    let fc = 0.5 / L as f64;
    let i0b = bessel_i0(KAISER_BETA);
    let h: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 - center;
            let x = 2.0 * fc * t;
            let sinc = if x.abs() < 1e-12 { 1.0 } else { (PI * x).sin() / (PI * x) };
            let r = t / center;
            let w = bessel_i0(KAISER_BETA * (1.0 - r * r).max(0.0).sqrt()) / i0b;
            2.0 * fc * sinc * w
        })
        .collect();
    let gain = L as f64 / h.iter().sum::<f64>();
    (0..L)
        .map(|p| {
            let mut ph = [0.0; TAPS_PER_PHASE];
            for (t, c) in ph.iter_mut().enumerate() {
                *c = h[p + t * L] * gain;
            }
            ph
        })
        .collect()
}

/// Streams the fixture up-converted to `FS` and mixed to `offset` Hz.
struct Upconverter<'a> {
    input: &'a [Complex<f64>],
    bank: &'a [[f64; TAPS_PER_PHASE]],
    offset: f64,
    next: usize,
    n_out: usize,
}

impl Upconverter<'_> {
    /// Fill `out` with the next block; returns false when exhausted.
    fn next_block(&mut self, out: &mut Vec<Complex<f32>>) -> bool {
        out.clear();
        let end = (self.next + BLOCK).min(self.n_out);
        // Upsampled-rate group delay, so output sample m lines up with input
        // time m·M/L.
        let delay = L * TAPS_PER_PHASE / 2;
        for mi in self.next..end {
            let k = mi * M + delay;
            let (i0, p) = (k / L, k % L);
            let mut acc = Complex::new(0.0f64, 0.0);
            for (t, &c) in self.bank[p].iter().enumerate() {
                if let Some(x) = i0.checked_sub(t).and_then(|j| self.input.get(j)) {
                    acc += x * c;
                }
            }
            let ph = TAU * (self.offset * mi as f64 / FS).fract();
            let y = acc * Complex::new(ph.cos(), ph.sin());
            out.push(Complex::new(y.re as f32, y.im as f32));
        }
        self.next = end;
        !out.is_empty()
    }
}

fn load(path: &str, format: &str) -> Vec<Complex<f64>> {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    match format {
        "cs16" => raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| {
                Complex::new(
                    i16::from_le_bytes([b[0], b[1]]) as f64 / 32768.0,
                    i16::from_le_bytes([b[2], b[3]]) as f64 / 32768.0,
                )
            })
            .collect(),
        "cf32" => raw
            .as_chunks::<8>()
            .0
            .iter()
            .map(|b| {
                Complex::new(
                    f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
                    f32::from_le_bytes([b[4], b[5], b[6], b[7]]) as f64,
                )
            })
            .collect(),
        other => panic!("unknown format {other:?} (expected cs16 or cf32)"),
    }
}

/// One decoder under test.
enum Under {
    Single(Box<Vdl2ChannelDecoder>),
    Multi(Box<Vdl2MultiChannelDecoder>),
}

struct Row {
    name: &'static str,
    dec: Under,
    frames: usize,
    other: usize,
    wall: Duration,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "bench/data/vdl2_105k_conj.s16".to_owned());
    let format = args.next().unwrap_or_else(|| "cs16".to_owned());
    let channel_hz: f64 = match args.next() {
        Some(s) => s.parse().expect("channel_hz must be a number"),
        None => 136_975_000.0,
    };

    let input = load(&path, &format);
    let n_out = input.len() * L / M;
    let bank = polyphase_bank();
    println!(
        "{path} ({format}, {:.3} MHz, {:.1} s at {IN_RATE} S/s) -> {FS} S/s plan @ {:.4} MHz",
        channel_hz / 1e6,
        input.len() as f64 / IN_RATE,
        PLAN_CENTER_HZ / 1e6
    );
    println!("{:>10}  {:<14}{:>8}{:>7}{:>10}", "offset", "front end", "frames", "other", "wall ms");

    let mut block = Vec::with_capacity(BLOCK);
    for &offset in &TEST_OFFSETS {
        let ch = PLAN_OFFSETS.iter().position(|&o| o == offset).expect("offset on the plan");
        let row = |name, dec| Row { name, dec, frames: 0, other: 0, wall: Duration::ZERO };
        let mut rows = vec![
            row(
                "per-channel",
                Under::Single(Box::new(
                    Vdl2ChannelDecoder::new(FS, offset).expect("per-channel decoder"),
                )),
            ),
            row(
                "default",
                Under::Multi(Box::new(
                    Vdl2MultiChannelDecoder::new(FS, &PLAN_OFFSETS).expect("default"),
                )),
            ),
            row(
                "shared",
                Under::Multi(Box::new(
                    Vdl2MultiChannelDecoder::new_shared(FS, &PLAN_OFFSETS).expect("shared"),
                )),
            ),
            row(
                "channelized",
                Under::Multi(Box::new(
                    Vdl2MultiChannelDecoder::new_channelized(FS, &PLAN_OFFSETS)
                        .expect("channelized"),
                )),
            ),
            row(
                "channelized-2x",
                Under::Multi(Box::new(
                    Vdl2MultiChannelDecoder::new_channelized_with_oversample(
                        FS,
                        &PLAN_OFFSETS,
                        Oversample::Twice,
                    )
                    .expect("channelized-2x"),
                )),
            ),
        ];
        let mut up = Upconverter { input: &input, bank: &bank, offset, next: 0, n_out };
        while up.next_block(&mut block) {
            for r in &mut rows {
                let t0 = Instant::now();
                match &mut r.dec {
                    Under::Single(d) => r.frames += d.process(&block).len(),
                    Under::Multi(d) => {
                        for (i, f) in d.process(&block) {
                            if i == ch {
                                r.frames += f.len();
                            } else {
                                r.other += f.len();
                            }
                        }
                    }
                }
                r.wall += t0.elapsed();
            }
        }
        for r in &rows {
            println!(
                "{:>+10.0}  {:<14}{:>8}{:>7}{:>10.0}",
                offset,
                r.name,
                r.frames,
                r.other,
                r.wall.as_secs_f64() * 1e3
            );
        }
    }
}
