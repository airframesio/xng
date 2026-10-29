//! Place a 105 kS/s VDL2 capture (cs16) into a wide 2.4 MS/s capture (cf32)
//! at a chosen channel offset, so the live (DDC) path can be measured against
//! real RF:
//!
//! ```bash
//! cargo run --release -p xng-mode-vdl2 --example upconvert -- \
//!   bench/data/vdl2_105k_conj.s16 /tmp/vdl2_wide.cf32 162500 [noise_amp]
//! xng decode /tmp/vdl2_wide.cf32 -f cf32 -m vdl2 -r 2400000 -c 136812500 --channels 136.975
//! ```
//!
//! Resampling is an exact 160/7 rational polyphase interpolator (f64,
//! Kaiser-windowed sinc, cutoff at the input Nyquist), followed by a mix to
//! `offset_hz` and optional uniform noise of ±`noise_amp` on I and Q.

use num_complex::Complex;
use std::f64::consts::{PI, TAU};
use std::io::{Read, Write};

const IN_RATE: f64 = 105_000.0;
const FS: f64 = 2_400_000.0;
/// 2_400_000 / 105_000 = 160 / 7.
const L: usize = 160;
const M: usize = 7;
const TAPS_PER_PHASE: usize = 48;
/// Kaiser β for ≈90 dB stopband (β = 0.1102·(A − 8.7)).
const KAISER_BETA: f64 = 8.96;
const BLOCK: usize = 1 << 20;

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

/// Polyphase bank `phases[p][t] = L·h[p + t·L]` of a Kaiser-windowed sinc
/// lowpass at the upsampled rate, cutoff 0.5/L cycles/sample (the input
/// Nyquist). Normalized so each phase has unit DC gain on average, i.e. the
/// interpolator preserves amplitude.
fn polyphase_bank() -> Vec<[f64; TAPS_PER_PHASE]> {
    let n = L * TAPS_PER_PHASE;
    let center = (n - 1) as f64 / 2.0;
    let fc = 0.5 / L as f64;
    let i0b = bessel_i0(KAISER_BETA);
    let h: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 - center;
            let x = 2.0 * fc * t;
            let sinc = if x.abs() < 1e-12 {
                1.0
            } else {
                (PI * x).sin() / (PI * x)
            };
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

fn main() -> std::io::Result<()> {
    let usage = "usage: upconvert <in.cs16> <out.cf32> <offset_hz> [noise_amp]";
    let mut args = std::env::args().skip(1);
    let in_path = args.next().expect(usage);
    let out_path = args.next().expect(usage);
    let offset: f64 = args
        .next()
        .expect(usage)
        .parse()
        .expect("offset_hz must be a number");
    let noise_amp: f64 = match args.next() {
        Some(s) => s.parse().expect("noise_amp must be a number"),
        None => 0.0,
    };

    let mut raw = Vec::new();
    std::fs::File::open(&in_path)?.read_to_end(&mut raw)?;
    let input: Vec<Complex<f64>> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| {
            Complex::new(
                i16::from_le_bytes([b[0], b[1]]) as f64 / 32768.0,
                i16::from_le_bytes([b[2], b[3]]) as f64 / 32768.0,
            )
        })
        .collect();
    drop(raw);
    let n_in = input.len();
    let n_out = n_in * L / M;

    let bank = polyphase_bank();
    // Upsampled-rate group delay of the prototype (rounded), so output sample
    // m lines up with input time m·M/L instead of lagging by half the filter.
    let delay = L * TAPS_PER_PHASE / 2;
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut nx = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state as f64 / u64::MAX as f64) * 2.0 - 1.0
    };

    let mut out = std::io::BufWriter::new(std::fs::File::create(&out_path)?);
    let mut bytes = Vec::with_capacity(BLOCK * 8);
    let mut m = 0usize;
    while m < n_out {
        let end = (m + BLOCK).min(n_out);
        bytes.clear();
        for mi in m..end {
            let k = mi * M + delay;
            let (i0, p) = (k / L, k % L);
            let mut acc = Complex::new(0.0f64, 0.0);
            for (t, &c) in bank[p].iter().enumerate() {
                if let Some(x) = i0.checked_sub(t).and_then(|j| input.get(j)) {
                    acc += x * c;
                }
            }
            let ph = TAU * (offset * mi as f64 / FS).fract();
            let mut y = acc * Complex::new(ph.cos(), ph.sin());
            if noise_amp > 0.0 {
                y += Complex::new(nx() * noise_amp, nx() * noise_amp);
            }
            bytes.extend_from_slice(&(y.re as f32).to_le_bytes());
            bytes.extend_from_slice(&(y.im as f32).to_le_bytes());
        }
        out.write_all(&bytes)?;
        m = end;
    }
    out.flush()?;
    eprintln!(
        "in:  {n_in} samples ({:.3} s at {IN_RATE} S/s) from {in_path}",
        n_in as f64 / IN_RATE
    );
    eprintln!(
        "out: {n_out} samples ({:.3} s at {FS} S/s) to {out_path}",
        n_out as f64 / FS
    );
    eprintln!("offset {offset:+} Hz, noise ±{noise_amp}");
    Ok(())
}
