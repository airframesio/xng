//! Arbitrary-ratio resampler (4-point Catmull-Rom interpolation).
//!
//! The [`Ddc`](crate::Ddc) integer-decimates a wideband capture down to just
//! above the target channel rate, then this stage corrects the small leftover
//! fraction to land on the exact channel rate. That makes any SDR sample rate
//! usable for any mode — e.g. an Airspy R2 (10 / 2.5 MS/s only) can feed the
//! 24 kHz ACARS or 48 kHz AIS channelizers, whose rates divide neither.
//!
//! Because the input is already band-limited to the channel passband and the
//! resample ratio is near unity (the integer stage does the heavy lifting),
//! cubic interpolation is well below the demodulators' noise floor. Integer
//! ratios skip this stage entirely.

use crate::IqSample;

/// Pull-based fractional resampler. Holds the few input samples straddling a
/// block boundary plus the running fractional read position, so a stream fed
/// in arbitrary-sized blocks resamples seamlessly.
pub struct Resampler {
    /// Input samples consumed per output sample (`in_rate / out_rate`).
    step: f64,
    /// Read position of the next output sample, in samples from the start of
    /// `carry`. Kept in `[1, 2)` between calls so index `i-1` always exists.
    pos: f64,
    /// Tail of the previous block needed for continuity (the 4-point kernel
    /// reaches one sample back and two forward).
    carry: Vec<IqSample>,
}

impl Resampler {
    /// Resample from `in_rate` to `out_rate` (both Hz, `in_rate >= out_rate`
    /// in practice, though any positive ratio works).
    pub fn new(in_rate: f64, out_rate: f64) -> Self {
        Self { step: in_rate / out_rate, pos: 1.0, carry: Vec::new() }
    }

    /// Resample `input`, appending output samples to `out`.
    pub fn process(&mut self, input: &[IqSample], out: &mut Vec<IqSample>) {
        // Work on the carried tail followed by the new block.
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(input);
        let n = buf.len();
        // Need indices i-1 .. i+2, with i = floor(pos): require pos < n-2.
        if n < 4 {
            self.carry = buf;
            return;
        }
        let mut pos = self.pos;
        while pos < (n - 2) as f64 {
            let i = pos.floor() as usize;
            let mu = (pos - i as f64) as f32;
            out.push(cubic(buf[i - 1], buf[i], buf[i + 1], buf[i + 2], mu));
            pos += self.step;
        }
        // Retain from one sample before the next read position so the kernel's
        // `i-1` tap survives into the next call; rebase `pos` onto the carry.
        let keep_from = (pos.floor() as usize).saturating_sub(1).min(n);
        self.carry = buf.split_off(keep_from);
        self.pos = pos - keep_from as f64;
    }
}

/// 4-point Catmull-Rom interpolation at fractional offset `mu` in `[0, 1)`
/// between `y0` and `y1` (with `ym1`/`y2` as the outer control points).
#[inline]
fn cubic(ym1: IqSample, y0: IqSample, y1: IqSample, y2: IqSample, mu: f32) -> IqSample {
    let mu2 = mu * mu;
    let mu3 = mu2 * mu;
    let c_m1 = -0.5 * mu + mu2 - 0.5 * mu3;
    let c_0 = 1.0 - 2.5 * mu2 + 1.5 * mu3;
    let c_1 = 0.5 * mu + 2.0 * mu2 - 1.5 * mu3;
    let c_2 = -0.5 * mu2 + 0.5 * mu3;
    ym1.scale(c_m1) + y0.scale(c_0) + y1.scale(c_1) + y2.scale(c_2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn tone(freq: f64, fs: f64, n: usize) -> Vec<IqSample> {
        (0..n)
            .map(|i| {
                let ph = TAU * freq * i as f64 / fs;
                IqSample::new(ph.cos() as f32, ph.sin() as f32)
            })
            .collect()
    }

    #[test]
    fn kernel_hits_control_points() {
        let (a, b, c, d) = (
            IqSample::new(1.0, 0.0),
            IqSample::new(2.0, 0.0),
            IqSample::new(3.0, 0.0),
            IqSample::new(4.0, 0.0),
        );
        // mu = 0 → y0, mu → 1 → y1.
        assert!((cubic(a, b, c, d, 0.0) - b).norm() < 1e-6);
        assert!((cubic(a, b, c, d, 1.0) - c).norm() < 1e-6);
        // A straight line stays linear at the midpoint.
        assert!((cubic(a, b, c, d, 0.5) - IqSample::new(2.5, 0.0)).norm() < 1e-6);
    }

    #[test]
    fn output_rate_is_exact() {
        // 24038.46.. → 24000 (the ACARS leftover after decimating 2.5 MS/s).
        let mut rs = Resampler::new(2_500_000.0 / 104.0, 24_000.0);
        let mut out = Vec::new();
        // Feed ~1 s of input in uneven blocks; expect ~24000 output samples.
        let input = tone(1_000.0, 2_500_000.0 / 104.0, 24_038);
        for chunk in input.chunks(257) {
            rs.process(chunk, &mut out);
        }
        // Within a couple of samples of the ideal 24000 (edge effects).
        assert!((out.len() as i64 - 24_000).abs() <= 3, "got {}", out.len());
    }

    #[test]
    fn preserves_an_in_band_tone() {
        // A 1 kHz tone resampled near unity keeps its frequency and amplitude.
        let in_rate = 24_038.0;
        let out_rate = 24_000.0;
        let mut rs = Resampler::new(in_rate, out_rate);
        let input = tone(1_000.0, in_rate, 48_000);
        let mut out = Vec::new();
        for chunk in input.chunks(512) {
            rs.process(chunk, &mut out);
        }
        let settled = &out[out.len() / 4..out.len() * 3 / 4];
        let amp = settled.iter().map(|s| s.norm()).sum::<f32>() / settled.len() as f32;
        assert!((amp - 1.0).abs() < 0.02, "amplitude drifted: {amp}");
        // Frequency check: the phase should advance by 2π·1000/24000 per output.
        let expected_dphi = (TAU * 1_000.0 / out_rate) as f32;
        let mid = out.len() / 2;
        let dphi = (out[mid + 1] * out[mid].conj()).arg();
        assert!((dphi - expected_dphi).abs() < 1e-3, "freq drifted: {dphi} vs {expected_dphi}");
    }
}
