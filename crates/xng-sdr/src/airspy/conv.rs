//! Real-to-IQ conversion for Airspy R2 / Mini raw samples.
//!
//! The R2 and Mini sample a real IF at twice the IQ rate (20 MS/s real for
//! 10 MS/s IQ) and leave the conversion to the host. libairspy's own
//! `FLOAT32_IQ` converter is mostly scalar in distribution builds and costs
//! several times what this one does. This is the textbook method:
//!
//! 1. remove DC (a slow running mean, updated once per block),
//! 2. shift the band down by fs/4: multiply by 1, −j, −1, j, …
//! 3. halfband low-pass and keep every second sample.
//!
//! After the fs/4 shift the even input samples are purely real and the odd
//! ones purely imaginary, and a halfband filter is zero at every even distance
//! from its centre. So I is a short real FIR over the even samples, and Q is
//! the odd samples delayed and scaled by the centre tap. There is no complex
//! arithmetic, and every loop is plain Rust the compiler vectorises. On
//! x86-64 a second copy is compiled for AVX2 with fused multiply-add and
//! picked at run time.
//!
//! With `x` the real input, `z[n] = x[n]·e^{−iπn/2}` is the shifted signal:
//! `z[2i] = (−1)^i·x[2i]` is real and `z[2i+1] = −i·(−1)^i·x[2i+1]` is
//! imaginary. A halfband `h` of length 4J+3 has its centre `c = 2J+1` at an
//! odd index and is zero at every other odd index, so of
//! `y[m] = Σ_k h[k]·z[2m − k]` only the even `k` reach the real samples and
//! only `k = c` reaches an imaginary one:
//!
//! ```text
//! I[m] = Σ_j h[2j]·e[m − j],     e[i] = (−1)^i·x[2i]
//! Q[m] = h[c]·o[m − (c + 1)/2],  o[i] = (−1)^(i+1)·x[2i+1]
//! ```
//!
//! Ported from the converter written for iridium-observatory by the same
//! author.

use num_complex::Complex;

/// Halfband length: 79 taps, Kaiser β 6.0, cutoff at a quarter of the real
/// rate. As fractions of the IQ rate: flat within 0.01 dB to 0.45, and at
/// least 62 dB down from 0.55, 69 dB from 0.58 and 72 dB from 0.6.
/// libairspy's 47-tap filter is −0.34 dB at 0.45, −28 dB from 0.55 and
/// −61.5 dB from 0.58. At 10 MS/s, anything at 5.5 MHz folds onto 4.5 MHz, so
/// the steeper edge keeps the top of the band clean.
const HB_NTAPS: usize = 79;
/// Kaiser window β for the halfband.
const HB_BETA: f64 = 6.0;
/// Index of the centre tap (39).
const HB_C: usize = HB_NTAPS / 2;
/// Nonzero taps that reach I: the even indices, 40 of them.
const HB_NI: usize = HB_NTAPS / 2 + 1;
/// Delay of the Q path in IQ samples (20).
const HB_QDLY: usize = HB_C.div_ceil(2);

/// Weight of each block's mean in the DC tracker. The ADC offset is static,
/// so a slow tracker is enough, and one constant per block keeps the inner
/// loop free of a per-sample recursion.
const DC_ALPHA: f64 = 0.05;

/// 12-bit samples to ±1.0 full scale, as libairspy's `FLOAT32_IQ` gives.
const SCALE: f32 = 1.0 / 2048.0;

/// Outputs per FIR block: 32 floats are four AVX or eight SSE/NEON registers.
const BLOCK: usize = 32;

/// Which build of the inner loops to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kernel {
    /// Plain multiply-add, vectorised for the build's baseline target (SSE2 on
    /// x86-64, NEON with fused multiply-add on aarch64).
    Generic,
    /// The same loop compiled for AVX2 with fused multiply-add.
    #[cfg(target_arch = "x86_64")]
    Avx2Fma,
}

impl Kernel {
    /// The fastest build this CPU supports.
    fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma")
        {
            return Kernel::Avx2Fma;
        }
        Kernel::Generic
    }
}

/// Streaming converter from raw Airspy samples (12-bit offset binary in a
/// `u16`, as libairspy's `UINT16_REAL` delivers them) to complex baseband at
/// half the input rate.
pub(crate) struct AirspyConverter {
    /// The halfband taps at even indices, which form the I filter.
    i_taps: [f32; HB_NI],
    /// Centre tap, the Q path's gain.
    centre: f32,
    /// DC estimate in raw units, `None` until the first block.
    dc: Option<f64>,
    /// Parity of the next pair index, for the fs/4 sign pattern.
    odd: bool,
    /// Even samples: `HB_NI − 1` of history, then the block.
    even: Vec<f32>,
    /// Odd samples: `HB_QDLY` of history, then the block.
    odd_samples: Vec<f32>,
    /// FIR output for the block.
    i_out: Vec<f32>,
    kernel: Kernel,
}

impl AirspyConverter {
    /// A converter using the fastest build of its loops this CPU supports.
    pub(crate) fn new() -> Self {
        Self::with_kernel(Kernel::detect())
    }

    /// A converter using a given build of its loops.
    fn with_kernel(kernel: Kernel) -> Self {
        let taps = halfband_taps();
        let mut i_taps = [0.0; HB_NI];
        for (j, t) in i_taps.iter_mut().enumerate() {
            *t = taps[2 * j];
        }
        Self {
            i_taps,
            centre: taps[HB_C],
            dc: None,
            odd: false,
            even: vec![0.0; HB_NI - 1],
            odd_samples: vec![0.0; HB_QDLY],
            i_out: Vec::new(),
            kernel,
        }
    }

    /// Convert `input` (an even number of raw samples; a trailing odd one is
    /// ignored) and append `input.len() / 2` IQ samples to `out`.
    pub(crate) fn process(&mut self, input: &[u16], out: &mut Vec<Complex<f32>>) {
        let m = input.len() / 2;
        if m == 0 {
            return;
        }
        let input = &input[..2 * m];

        // A u32 holds the sum of 2^16 samples of any u16 value, and summing
        // in u32 lanes is cheaper than widening each sample to u64.
        let sum: u64 = input
            .chunks(1 << 16)
            .map(|c| u64::from(c.iter().map(|&x| u32::from(x)).sum::<u32>()))
            .sum();
        let mean = sum as f64 / input.len() as f64;
        let dc = match self.dc {
            None => mean,
            Some(dc) => dc + DC_ALPHA * (mean - dc),
        };
        self.dc = Some(dc);
        let dc = dc as f32;

        let s0 = if self.odd { -SCALE } else { SCALE };
        if m % 2 == 1 {
            self.odd = !self.odd;
        }
        // The buffers keep their size between blocks of the same length (the
        // usual case), so only a change of length zero-fills.
        self.even.resize(HB_NI - 1 + m, 0.0);
        self.odd_samples.resize(HB_QDLY + m, 0.0);
        self.i_out.resize(m, 0.0);
        let start = out.len();
        out.resize(start + m, Complex::default());
        let block = Block {
            input,
            dc,
            s0,
            taps: &self.i_taps,
            centre: self.centre,
            even: &mut self.even,
            odd: &mut self.odd_samples,
            i_out: &mut self.i_out,
            out: &mut out[start..],
        };
        match self.kernel {
            // NEON always has fused multiply-add; SSE2 doesn't, and `mul_add`
            // without it is a slow library call.
            Kernel::Generic => block.run::<{ cfg!(target_arch = "aarch64") }>(),
            #[cfg(target_arch = "x86_64")]
            // SAFETY: `Kernel::detect` returns `Avx2Fma` only when the CPU has
            // both features, and tests construct it only after the same check.
            Kernel::Avx2Fma => unsafe { block.run_avx2_fma() },
        }

        // Keep the tails as the next block's history.
        self.even.copy_within(m.., 0);
        self.odd_samples.copy_within(m.., 0);
    }
}

/// The 79-tap Kaiser halfband, normalised to unit DC gain. Taps at an even
/// distance from the centre are exactly zero.
fn halfband_taps() -> [f32; HB_NTAPS] {
    let i0_beta = bessel_i0(HB_BETA);
    let mut h = [0.0f64; HB_NTAPS];
    for (k, v) in h.iter_mut().enumerate() {
        let d = k as i64 - HB_C as i64;
        let sinc = if d == 0 {
            0.5
        } else if d % 2 == 0 {
            0.0
        } else {
            (std::f64::consts::PI * d as f64 / 2.0).sin() / (std::f64::consts::PI * d as f64)
        };
        let r = d as f64 / HB_C as f64;
        *v = sinc * bessel_i0(HB_BETA * (1.0 - r * r).sqrt()) / i0_beta;
    }
    let sum: f64 = h.iter().sum();
    let mut taps = [0.0f32; HB_NTAPS];
    for (t, v) in taps.iter_mut().zip(h) {
        *t = (v / sum) as f32;
    }
    taps
}

/// Modified Bessel function of the first kind, order zero (for the Kaiser
/// window).
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    let q = x * x / 4.0;
    for k in 1..64 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-12 {
            break;
        }
    }
    sum
}

/// One block's worth of the converter's inner loops, borrowed from
/// [`AirspyConverter`] so one dispatch covers all of them.
struct Block<'a> {
    /// The block's raw samples, an even number.
    input: &'a [u16],
    /// DC estimate in raw units.
    dc: f32,
    /// Scale for the block's first pair, its sign the fs/4 pattern.
    s0: f32,
    taps: &'a [f32; HB_NI],
    centre: f32,
    /// `HB_NI − 1` of history, then room for the block's even samples.
    even: &'a mut [f32],
    /// `HB_QDLY` of history, then room for the block's odd samples.
    odd: &'a mut [f32],
    /// Room for the block's FIR output.
    i_out: &'a mut [f32],
    /// Room for the block's IQ output.
    out: &'a mut [Complex<f32>],
}

impl Block<'_> {
    /// [`Block::run`] compiled for AVX2 with fused multiply-add.
    ///
    /// # Safety
    ///
    /// The CPU must support AVX2 and FMA.
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,fma")]
    unsafe fn run_avx2_fma(self) {
        self.run::<true>();
    }

    /// Split, FIR, and merge into IQ. `FMA` picks fused multiply-add.
    #[inline(always)]
    fn run<const FMA: bool>(self) {
        // Split into even and odd samples, remove DC, scale, and apply the
        // fs/4 signs: e = +x, o = −x at an even pair index, and the other way
        // round at an odd one. With the scale folded in, each sample is one
        // multiply-add.
        let (s0, d0) = (self.s0, -self.s0 * self.dc);
        let e = &mut self.even[HB_NI - 1..];
        let o = &mut self.odd[HB_QDLY..];
        let (quads, rest) = self.input.as_chunks::<4>();
        let (e2, e_rest) = e.as_chunks_mut::<2>();
        let (o2, o_rest) = o.as_chunks_mut::<2>();
        for ((q, e), o) in quads.iter().zip(e2).zip(o2) {
            e[0] = f32::from(q[0]) * s0 + d0;
            o[0] = f32::from(q[1]) * -s0 - d0;
            e[1] = f32::from(q[2]) * -s0 - d0;
            o[1] = f32::from(q[3]) * s0 + d0;
        }
        if let ([x0, x1], [e], [o]) = (rest, e_rest, o_rest) {
            *e = f32::from(*x0) * s0 + d0;
            *o = f32::from(*x1) * -s0 - d0;
        }

        fir_loop::<FMA>(self.taps, self.even, self.i_out);

        // I is negated: −conj(I + iQ) = −I + iQ is the +fs/4 shift instead of
        // −fs/4, the orientation the Airspy's IF needs (with −fs/4 the band
        // comes out mirrored, as checked against libairspy's output).
        let centre = self.centre;
        for ((y, &i), &q) in self.out.iter_mut().zip(&*self.i_out).zip(&*self.odd) {
            *y = Complex::new(-i, centre * q);
        }
    }
}

/// `out[m] = Σ_j taps[j]·input[m + j]`, where `input` holds
/// `out.len() + HB_NI − 1` samples (the taps are symmetric, so this is the
/// convolution too), in blocks of [`BLOCK`] outputs whose accumulators stay in vector
/// registers across all the taps: one load and one multiply-add per tap and
/// output, which the compiler turns into whole-register operations.
#[inline(always)]
fn fir_loop<const FMA: bool>(taps: &[f32; HB_NI], input: &[f32], out: &mut [f32]) {
    let (blocks, tail) = out.as_chunks_mut::<BLOCK>();
    for (b, block) in blocks.iter_mut().enumerate() {
        let base = b * BLOCK;
        let mut acc = [0.0f32; BLOCK];
        for (j, &t) in taps.iter().enumerate() {
            let x: &[f32; BLOCK] = input[base + j..base + j + BLOCK].try_into().unwrap();
            for (a, &x) in acc.iter_mut().zip(x) {
                *a = if FMA { x.mul_add(t, *a) } else { *a + t * x };
            }
        }
        *block = acc;
    }
    let base = blocks.len() * BLOCK;
    for (m, y) in tail.iter_mut().enumerate() {
        let x = &input[base + m..base + m + HB_NI];
        *y = taps.iter().zip(x).fold(
            0.0,
            |a, (&t, &x)| if FMA { x.mul_add(t, a) } else { a + t * x },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;
    use std::os::raw::c_int;

    /// Gain in dB of the full halfband at `f`, a fraction of the real rate.
    fn response_db(taps: &[f32], f: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (k, &t) in taps.iter().enumerate() {
            let w = -2.0 * PI * f * k as f64;
            re += t as f64 * w.cos();
            im += t as f64 * w.sin();
        }
        10.0 * (re * re + im * im).log10()
    }

    /// Raw offset-binary samples of a real tone at `f` (a fraction of the real
    /// rate) with amplitude `amp` in ADC counts.
    fn tone(n: usize, f: f64, amp: f64) -> Vec<u16> {
        (0..n)
            .map(|k| (2048.0 + amp * (2.0 * PI * f * k as f64).cos()).round() as u16)
            .collect()
    }

    /// Raw samples whose every pair sums to 4096, so any block's mean is
    /// exactly 2048 and the DC tracker never moves: broadband, but
    /// independent of how it is split into blocks.
    fn pair_noise(pairs: usize, seed: u64) -> Vec<u16> {
        let mut s = seed;
        let mut v = Vec::with_capacity(2 * pairs);
        for _ in 0..pairs {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let a = ((s >> 33) % 1500) as u16;
            v.push(2048 + a);
            v.push(2048 - a);
        }
        v
    }

    /// The builds to test on this CPU.
    fn kernels() -> Vec<Kernel> {
        let mut k = vec![Kernel::Generic];
        #[cfg(target_arch = "x86_64")]
        if Kernel::detect() == Kernel::Avx2Fma {
            k.push(Kernel::Avx2Fma);
        }
        k
    }

    /// The halfband is symmetric, has exact zeros at even distances from the
    /// centre, and the I taps and the centre tap each carry half the gain.
    #[test]
    fn halfband_shape() {
        let h = halfband_taps();
        for k in 0..HB_NTAPS {
            assert_eq!(h[k], h[HB_NTAPS - 1 - k]);
            let d = k.abs_diff(HB_C);
            if d != 0 && d % 2 == 0 {
                assert_eq!(h[k], 0.0, "tap {k}");
            }
        }
        let i_sum: f32 = (0..HB_NI).map(|j| h[2 * j]).sum();
        assert!((i_sum - 0.5).abs() < 1e-4, "{i_sum}");
        assert!((h[HB_C] - 0.5).abs() < 1e-4, "{}", h[HB_C]);
    }

    /// The response the [`HB_NTAPS`] comment states, checked every 0.1% of
    /// the IQ rate.
    #[test]
    fn halfband_response() {
        let h = halfband_taps();
        for i in 0..=1000 {
            let f_iq = i as f64 / 1000.0;
            // A fraction of the IQ rate is half that fraction of the real rate.
            let g = response_db(&h, f_iq / 2.0);
            let limit = match f_iq {
                f if f <= 0.45 => {
                    assert!(g.abs() < 0.01, "{g:.3} dB at {f_iq}");
                    continue;
                }
                f if f < 0.55 => continue,
                f if f < 0.58 => -62.0,
                f if f < 0.6 => -69.0,
                _ => -72.0,
            };
            assert!(g < limit, "{g:.1} dB at {f_iq}");
        }
    }

    /// A real tone a little above a quarter of the real rate comes out as one
    /// IQ tone that far below the centre (the mapping is mirrored, as in
    /// libairspy's output), at half the ADC amplitude, with its image and DC
    /// far below it.
    #[test]
    fn tone_lands_mirrored() {
        for kernel in kernels() {
            let (n, amp) = (1 << 16, 1000.0);
            // 0.25 + 0.03125 of the real rate: −0.0625 of the IQ rate.
            let raw = tone(2 * n, 0.25 + 1.0 / 32.0, amp);
            let mut c = AirspyConverter::with_kernel(kernel);
            let mut out = Vec::new();
            c.process(&raw, &mut out);
            let y = &out[1024..];
            let at = |f: f64| -> f64 {
                let s: Complex<f64> = y
                    .iter()
                    .enumerate()
                    .map(|(k, v)| {
                        Complex::new(v.re as f64, v.im as f64)
                            * Complex::from_polar(1.0, -2.0 * PI * f * k as f64)
                    })
                    .sum();
                s.norm() / y.len() as f64
            };
            let (want, image, dc) = (at(-1.0 / 16.0), at(1.0 / 16.0), at(0.0));
            let expect = amp / 2.0 / 2048.0;
            assert!(
                (want / expect - 1.0).abs() < 1e-3,
                "{kernel:?}: {want} vs {expect}"
            );
            assert!(image < want * 1e-4, "{kernel:?}: image {image}");
            assert!(dc < want * 1e-4, "{kernel:?}: dc {dc}");
        }
    }

    /// Splitting the input into blocks of any size, odd IQ counts included,
    /// gives the same output as one block.
    #[test]
    fn block_size_does_not_matter() {
        let raw = pair_noise(10_000, 1);
        for kernel in kernels() {
            let mut whole = Vec::new();
            AirspyConverter::with_kernel(kernel).process(&raw, &mut whole);
            for size in [2, 6, 158, 1000, 4098] {
                let mut c = AirspyConverter::with_kernel(kernel);
                let mut split = Vec::new();
                for block in raw.chunks(size) {
                    c.process(block, &mut split);
                }
                assert_eq!(split, whole, "{kernel:?}, blocks of {size}");
            }
        }
    }

    /// The AVX2 build matches the generic one to rounding.
    #[test]
    fn kernels_agree() {
        let raw = pair_noise(50_000, 2);
        let outs: Vec<Vec<Complex<f32>>> = kernels()
            .into_iter()
            .map(|k| {
                let mut out = Vec::new();
                AirspyConverter::with_kernel(k).process(&raw, &mut out);
                out
            })
            .collect();
        for out in &outs[1..] {
            for (a, b) in out.iter().zip(&outs[0]) {
                assert!((a - b).norm() < 1e-6, "{a} vs {b}");
            }
        }
    }

    /// A trailing odd sample is ignored, and an empty block is a no-op.
    #[test]
    fn odd_and_empty_input() {
        let mut c = AirspyConverter::new();
        let mut out = Vec::new();
        c.process(&[], &mut out);
        c.process(&[2048], &mut out);
        assert!(out.is_empty());
        c.process(&[2048, 2048, 2048], &mut out);
        assert_eq!(out.len(), 1);
    }

    /// Measured complex amplitude of a tone at `f` (a fraction of the IQ
    /// rate) in `y`.
    fn tone_at(y: &[Complex<f32>], f: f64) -> Complex<f64> {
        let s: Complex<f64> = y
            .iter()
            .enumerate()
            .map(|(k, v)| {
                Complex::new(v.re as f64, v.im as f64)
                    * Complex::from_polar(1.0, -2.0 * PI * f * k as f64)
            })
            .sum();
        s / y.len() as f64
    }

    mod libairspy {
        use std::os::raw::{c_int, c_void};

        extern "C" {
            pub fn iqconverter_float_create(hb_kernel: *const f32, len: c_int) -> *mut c_void;
            pub fn iqconverter_float_free(cnv: *mut c_void);
            pub fn iqconverter_float_process(cnv: *mut c_void, samples: *mut f32, len: c_int);
        }
    }

    /// Against libairspy's own converter, given our halfband so only the DC
    /// handling differs: every tone across the band comes out at the same
    /// frequency, gain and phase. libairspy removes DC per sample at the real
    /// rate, which reads as about +0.04 dB of gain across the band and a
    /// phase lead that grows towards the real-rate DC (+0.03 rad at the +0.40
    /// tone, 0.001 rad mid-band); ours, a per-block constant, adds neither.
    #[test]
    fn matches_libairspy() {
        // Real-rate frequencies. The mapping is mirrored, so the IQ offsets
        // are 2·(0.25 − f) of the IQ rate: +0.40, +0.14, −0.04, −0.22, −0.36,
        // none of them another's image.
        let tones = [0.05, 0.18, 0.27, 0.36, 0.43];
        let n = 1 << 17;
        let raw: Vec<u16> = (0..n)
            .map(|k| {
                let v: f64 = tones
                    .iter()
                    .enumerate()
                    .map(|(i, f)| 300.0 * (2.0 * PI * f * k as f64 + i as f64).cos())
                    .sum();
                (2048.0 + v).round() as u16
            })
            .collect();

        let taps = halfband_taps();
        let mut theirs: Vec<f32> = raw
            .iter()
            .map(|&x| (f32::from(x) - 2048.0) * SCALE)
            .collect();
        unsafe {
            let cnv = libairspy::iqconverter_float_create(taps.as_ptr(), HB_NTAPS as c_int);
            assert!(!cnv.is_null());
            for block in theirs.chunks_mut(1 << 16) {
                libairspy::iqconverter_float_process(cnv, block.as_mut_ptr(), block.len() as c_int);
            }
            libairspy::iqconverter_float_free(cnv);
        }
        // In place: each block of real samples now holds half as many IQ.
        let theirs: Vec<Complex<f32>> = theirs
            .chunks(1 << 16)
            .flat_map(|b| b.as_chunks::<2>().0)
            .map(|p| Complex::new(p[0], p[1]))
            .collect();

        for kernel in kernels() {
            let mut ours = Vec::new();
            let mut c = AirspyConverter::with_kernel(kernel);
            for block in raw.chunks(1 << 16) {
                c.process(block, &mut ours);
            }
            assert_eq!(ours.len(), theirs.len());
            let (a, b) = (&ours[4096..], &theirs[4096..]);
            for &f in &tones {
                let off = 2.0 * (0.25 - f);
                let (ta, tb) = (tone_at(a, off), tone_at(b, off));
                let db = 20.0 * (ta.norm() / tb.norm()).log10();
                let rad = (ta / tb).arg();
                assert!(db.abs() < 0.05, "{kernel:?} at {off}: {db} dB");
                assert!(rad.abs() < 0.04, "{kernel:?} at {off}: {rad} rad");
                // Nothing at the mirror image.
                let im = tone_at(a, -off).norm();
                assert!(im < ta.norm() * 1e-3, "{kernel:?} image at {}: {im}", -off);
            }
        }
    }
}
