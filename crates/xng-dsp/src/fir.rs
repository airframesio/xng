//! FIR filtering and windowed-sinc filter design.

use crate::window::blackman_harris;
use crate::IqSample;
use std::f64::consts::PI;

/// Design a windowed-sinc lowpass filter.
///
/// * `cutoff` — normalized cutoff frequency in cycles/sample (0 < cutoff < 0.5),
///   i.e. `f_cutoff_hz / sample_rate`.
/// * `num_taps` — filter length.
///
/// Taps are normalized to unity DC gain.
pub fn lowpass_taps(cutoff: f64, num_taps: usize) -> Vec<f32> {
    assert!(cutoff > 0.0 && cutoff < 0.5, "cutoff must be in (0, 0.5)");
    assert!(num_taps >= 3);
    let win = blackman_harris(num_taps);
    let center = (num_taps as f64 - 1.0) / 2.0;
    let mut taps: Vec<f64> = (0..num_taps)
        .map(|i| {
            let t = i as f64 - center;
            let sinc = if t.abs() < 1e-12 {
                2.0 * cutoff
            } else {
                (2.0 * PI * cutoff * t).sin() / (PI * t)
            };
            sinc * win[i]
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    for t in &mut taps {
        *t /= sum;
    }
    taps.into_iter().map(|t| t as f32).collect()
}

/// Design root-raised-cosine (RRC) FIR taps — the canonical matched filter for
/// linearly-modulated bursts (BPSK/QPSK/QAM as used by STD-C, Aero, AIS, VDL2).
///
/// * `beta` — roll-off factor in `[0, 1]`.
/// * `sps` — samples per symbol (may be fractional).
/// * `num_taps` — filter length (an odd count keeps the peak on a single tap).
///
/// Taps are normalized to unit energy (L2 norm = 1) so the filter is a true
/// matched filter: convolving a TX-RRC pulse with these taps yields a
/// raised-cosine pulse with (near) zero inter-symbol interference at symbol
/// instants. Singularities at `t = 0` and `|t| = 1/(4·beta)` use their closed
/// forms.
pub fn rrc_taps(beta: f64, sps: f64, num_taps: usize) -> Vec<f32> {
    assert!((0.0..=1.0).contains(&beta), "beta must be in [0, 1]");
    assert!(sps > 0.0, "sps must be positive");
    assert!(num_taps >= 3);
    let center = (num_taps as f64 - 1.0) / 2.0;
    let mut taps: Vec<f64> = (0..num_taps)
        .map(|i| {
            // Time in symbols relative to the filter center.
            let t = (i as f64 - center) / sps;
            if t.abs() < 1e-12 {
                1.0 - beta + 4.0 * beta / PI
            } else if beta > 0.0 && (t.abs() - 1.0 / (4.0 * beta)).abs() < 1e-9 {
                let a = PI / (4.0 * beta);
                (beta / 2.0_f64.sqrt())
                    * ((1.0 + 2.0 / PI) * a.sin() + (1.0 - 2.0 / PI) * a.cos())
            } else {
                let num = (PI * t * (1.0 - beta)).sin()
                    + 4.0 * beta * t * (PI * t * (1.0 + beta)).cos();
                let den = PI * t * (1.0 - (4.0 * beta * t).powi(2));
                num / den
            }
        })
        .collect();
    let energy: f64 = taps.iter().map(|t| t * t).sum::<f64>().sqrt();
    for t in &mut taps {
        *t /= energy;
    }
    taps.into_iter().map(|t| t as f32).collect()
}

/// Streaming FIR filter over complex samples with optional decimation.
///
/// The delay line is stored **doubled and time-reversed-friendly**: the most
/// recent `n` samples always occupy one contiguous run, so the convolution
/// inner loop is a straight `zip` of two slices. A circular buffer with a
/// wrapping index (the obvious implementation) defeats vectorisation — the
/// compiler cannot prove the access pattern is affine, so it emits a
/// bounds-checked load and a branch per tap. Convolution is the single
/// largest cost in the decode pipeline, so this layout is deliberate.
pub struct Fir {
    taps: Vec<f32>,
    /// `2 * n` slots holding the live window contiguously. Samples are stored
    /// **newest-first** so the convolution walks the window and the taps in
    /// the same direction the circular-buffer formulation did — the summation
    /// order is preserved, so the result is bit-identical (asserted in tests).
    hist: Vec<IqSample>,
    /// Index of the newest sample within `hist` (always `< n`); the live
    /// window is `hist[head .. head + n]`, newest first.
    head: usize,
    decim: usize,
    phase: usize,
}

impl Fir {
    pub fn new(taps: Vec<f32>) -> Self {
        Self::with_decimation(taps, 1)
    }

    pub fn with_decimation(taps: Vec<f32>, decim: usize) -> Self {
        assert!(!taps.is_empty() && decim >= 1);
        let n = taps.len();
        Self {
            taps,
            hist: vec![IqSample::new(0.0, 0.0); 2 * n],
            head: 0,
            decim,
            phase: 0,
        }
    }

    /// Push input samples; append filtered (and decimated) output to `out`.
    pub fn process(&mut self, input: &[IqSample], out: &mut Vec<IqSample>) {
        let n = self.taps.len();
        for &x in input {
            // Prepend to the sliding window: `head` walks backwards so the
            // newest sample is at the LOW end and `hist[head..head + n]` reads
            // newest-first. Storing each sample at both `head` and `head + n`
            // keeps that window contiguous across the wrap.
            self.head = if self.head == 0 { n - 1 } else { self.head - 1 };
            self.hist[self.head] = x;
            self.hist[self.head + n] = x;

            self.phase += 1;
            if self.phase == self.decim {
                self.phase = 0;
                // Newest-first window against taps in natural order — the same
                // pairing and the same summation order as the circular-buffer
                // version, so the result is bit-identical. Both slices are
                // contiguous with statically-equal length, so this vectorises
                // (no wrapping index, no per-tap bounds check or branch).
                let win = &self.hist[self.head..self.head + n];
                let mut acc = IqSample::new(0.0, 0.0);
                for (w, &tap) in win.iter().zip(self.taps.iter()) {
                    acc += *w * tap;
                }
                out.push(acc);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowpass_passes_dc_blocks_high() {
        let taps = lowpass_taps(0.1, 101);
        let mut fir = Fir::new(taps);
        // DC input
        let dc: Vec<IqSample> = vec![IqSample::new(1.0, 0.0); 512];
        let mut out = Vec::new();
        fir.process(&dc, &mut out);
        let settled = &out[200..];
        let mean: f32 = settled.iter().map(|c| c.re).sum::<f32>() / settled.len() as f32;
        assert!((mean - 1.0).abs() < 1e-3, "DC gain should be 1, got {mean}");

        // High-frequency input (0.4 cycles/sample, well above 0.1 cutoff)
        let taps = lowpass_taps(0.1, 101);
        let mut fir = Fir::new(taps);
        let hf: Vec<IqSample> = (0..512)
            .map(|i| {
                let ph = 2.0 * std::f32::consts::PI * 0.4 * i as f32;
                IqSample::new(ph.cos(), ph.sin())
            })
            .collect();
        let mut out = Vec::new();
        fir.process(&hf, &mut out);
        let peak = out[200..].iter().map(|c| c.norm()).fold(0.0f32, f32::max);
        assert!(peak < 1e-3, "0.4 f/fs tone should be rejected, peak {peak}");
    }

    #[test]
    fn rrc_is_symmetric_and_peaked() {
        // beta=0.5, sps=4 places the |t|=1/(4beta)=0.5-symbol singularity exactly
        // on samples (center ± 2), exercising that closed-form branch.
        let taps = rrc_taps(0.5, 4.0, 65);
        let n = taps.len();
        for i in 0..n {
            assert!((taps[i] - taps[n - 1 - i]).abs() < 1e-6, "tap {i} not symmetric");
            assert!(taps[i].is_finite(), "tap {i} not finite");
        }
        let center = n / 2;
        let peak = taps[center];
        for (i, &t) in taps.iter().enumerate() {
            if i != center {
                assert!(t.abs() <= peak + 1e-6, "tap {i} exceeds center peak");
            }
        }
        let energy: f32 = taps.iter().map(|t| t * t).sum();
        assert!((energy - 1.0).abs() < 1e-4, "energy should be 1, got {energy}");
    }

    #[test]
    fn rrc_cascade_is_near_zero_isi() {
        // RRC ⊛ RRC ≈ raised cosine: zero crossings at non-zero symbol instants.
        let sps = 8usize;
        let taps = rrc_taps(0.35, sps as f64, 8 * sps + 1);
        // Full linear convolution of the tap set with itself.
        let m = taps.len();
        let mut rc = vec![0.0f32; 2 * m - 1];
        for i in 0..m {
            for j in 0..m {
                rc[i + j] += taps[i] * taps[j];
            }
        }
        let mid = m - 1; // center of the cascade
        let peak = rc[mid];
        // One symbol away should be a near-null relative to the peak.
        let one_sym = rc[mid + sps].abs() / peak.abs();
        assert!(one_sym < 0.05, "ISI at ±1 symbol too high: {one_sym}");
    }

    #[test]
    fn decimation_reduces_rate() {
        let taps = lowpass_taps(0.05, 64);
        let mut fir = Fir::with_decimation(taps, 4);
        let input = vec![IqSample::new(1.0, 0.0); 400];
        let mut out = Vec::new();
        fir.process(&input, &mut out);
        assert_eq!(out.len(), 100);
    }

    /// Reference implementation: the straightforward circular-delay-line FIR
    /// this filter replaced. Kept in the tests so the fast contiguous-window
    /// layout can be held to **bit-exact** equality with the obvious
    /// formulation — the layout is a performance change, not a maths change.
    struct RefFir {
        taps: Vec<f32>,
        delay: Vec<IqSample>,
        pos: usize,
        decim: usize,
        phase: usize,
    }

    impl RefFir {
        fn new(taps: Vec<f32>, decim: usize) -> Self {
            let n = taps.len();
            Self { taps, delay: vec![IqSample::new(0.0, 0.0); n], pos: 0, decim, phase: 0 }
        }
        fn process(&mut self, input: &[IqSample], out: &mut Vec<IqSample>) {
            let n = self.taps.len();
            for &x in input {
                self.pos = (self.pos + 1) % n;
                self.delay[self.pos] = x;
                self.phase += 1;
                if self.phase == self.decim {
                    self.phase = 0;
                    let mut acc = IqSample::new(0.0, 0.0);
                    let mut idx = self.pos;
                    for &tap in &self.taps {
                        acc += self.delay[idx] * tap;
                        idx = if idx == 0 { n - 1 } else { idx - 1 };
                    }
                    out.push(acc);
                }
            }
        }
    }

    #[test]
    fn matches_circular_reference_bit_exactly() {
        // Deterministic pseudo-random input; several tap counts and decimation
        // factors, fed in uneven blocks so window wrap-around is exercised at
        // arbitrary offsets.
        let mut s = 0x1234_5678_9abc_def0u64;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s as f32 / u64::MAX as f32) * 2.0 - 1.0
        };
        let input: Vec<IqSample> = (0..20_000).map(|_| IqSample::new(next(), next())).collect();

        for &(ntaps, decim) in &[(9usize, 1usize), (31, 1), (64, 4), (101, 1), (121, 10), (128, 3)]
        {
            let taps = lowpass_taps(0.05, ntaps);
            let mut fast = Fir::with_decimation(taps.clone(), decim);
            let mut refr = RefFir::new(taps, decim);
            let (mut a, mut b) = (Vec::new(), Vec::new());
            for blk in input.chunks(577) {
                fast.process(blk, &mut a);
                refr.process(blk, &mut b);
            }
            assert_eq!(a.len(), b.len(), "ntaps={ntaps} decim={decim}: length differs");
            for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                assert_eq!(
                    x.re.to_bits(),
                    y.re.to_bits(),
                    "ntaps={ntaps} decim={decim} sample {i}: re {} vs {}",
                    x.re,
                    y.re
                );
                assert_eq!(
                    x.im.to_bits(),
                    y.im.to_bits(),
                    "ntaps={ntaps} decim={decim} sample {i}: im {} vs {}",
                    x.im,
                    y.im
                );
            }
        }
    }
}
