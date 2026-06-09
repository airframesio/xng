//! Critically-sampled polyphase filter-bank (PFB) analysis channelizer.
//!
//! Splits a wideband capture at `fs` into `M` evenly spaced channels of
//! `fs / M` each — the front end shared by every xng decode core (e.g. one
//! 2 MS/s VHF capture → 25 kHz channels for ACARS + VDL2).
//!
//! Implementation: standard DFT filter bank (harris, "Multirate Signal
//! Processing for Communication Systems"). The prototype lowpass `h` of
//! length `M·L` is decomposed into `M` polyphase branches
//! `p_k[l] = h[k + l·M]`. For each input block of `M` samples:
//!
//! ```text
//! v[k] = Σ_l p_k[l] · x[n - k - l·M]      (newest sample at n)
//! y    = IDFT(v)                          (unnormalized)
//! ```
//!
//! `y[i]` is then one output sample of the channel centered at `+i · fs/M`
//! (indices wrap: `i > M/2` are negative frequencies, like FFT bins).

use crate::fir::lowpass_taps;
use crate::IqSample;
use rustfft::{Fft, FftDirection, FftPlanner};
use std::sync::Arc;

pub struct PfbChannelizer {
    nch: usize,
    taps_per_branch: usize,
    /// Polyphase branches: `branches[k][l] = h[k + l*nch]`.
    branches: Vec<Vec<f32>>,
    /// Delay line of the last `nch * taps_per_branch` input samples,
    /// oldest first.
    delay: Vec<IqSample>,
    ifft: Arc<dyn Fft<f32>>,
    /// Scratch for the IFFT (v[k] → channel outputs).
    scratch: Vec<IqSample>,
    /// Partial input block buffered until `nch` samples are available.
    pending: Vec<IqSample>,
}

impl PfbChannelizer {
    /// Create a channelizer with `nch` channels and `taps_per_branch` taps
    /// per polyphase branch (8–16 is typical; more = sharper channel edges,
    /// more CPU).
    pub fn new(nch: usize, taps_per_branch: usize) -> Self {
        assert!(nch >= 2 && taps_per_branch >= 2);
        let total = nch * taps_per_branch;
        // Prototype lowpass: passband half-width = half a channel.
        let proto = lowpass_taps(0.5 / nch as f64, total);
        let branches: Vec<Vec<f32>> = (0..nch)
            .map(|k| (0..taps_per_branch).map(|l| proto[k + l * nch]).collect())
            .collect();
        let ifft = FftPlanner::new().plan_fft(nch, FftDirection::Inverse);
        Self {
            nch,
            taps_per_branch,
            branches,
            delay: vec![IqSample::new(0.0, 0.0); total],
            ifft,
            scratch: vec![IqSample::new(0.0, 0.0); nch],
            pending: Vec::with_capacity(nch),
        }
    }

    pub fn num_channels(&self) -> usize {
        self.nch
    }

    /// Center frequency of channel `i` relative to the capture center, given
    /// the input sample rate. Channels above `nch/2` wrap negative.
    pub fn channel_offset_hz(&self, i: usize, sample_rate: f64) -> f64 {
        let i = i % self.nch;
        let idx = if i <= self.nch / 2 { i as f64 } else { i as f64 - self.nch as f64 };
        idx * sample_rate / self.nch as f64
    }

    /// Feed input samples; for every full block of `nch` inputs, one output
    /// sample per channel is appended to `out[ch]`.
    ///
    /// `out` must contain `nch` vectors.
    pub fn process(&mut self, input: &[IqSample], out: &mut [Vec<IqSample>]) {
        assert_eq!(out.len(), self.nch);
        for &x in input {
            self.pending.push(x);
            if self.pending.len() == self.nch {
                self.step(out);
                self.pending.clear();
            }
        }
    }

    fn step(&mut self, out: &mut [Vec<IqSample>]) {
        let total = self.delay.len();
        // Slide the delay line left by one block, append the new block
        // (delay is oldest-first; newest sample ends at delay[total-1]).
        self.delay.copy_within(self.nch.., 0);
        self.delay[total - self.nch..].copy_from_slice(&self.pending);

        // v[k] = Σ_l p_k[l] · x[n - k - l·M]; x[n - j] = delay[total-1-j]
        for k in 0..self.nch {
            let mut acc = IqSample::new(0.0, 0.0);
            for l in 0..self.taps_per_branch {
                let j = k + l * self.nch;
                acc += self.delay[total - 1 - j] * self.branches[k][l];
            }
            self.scratch[k] = acc;
        }

        self.ifft.process(&mut self.scratch);
        for (ch, sample) in self.scratch.iter().enumerate() {
            // Scale so a full-scale input tone yields ~unit channel output
            // (IDFT is unnormalized; prototype has unity DC gain split
            // across branches).
            out[ch].push(*sample);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    /// Inject a tone at channel i's center; energy must land in channel i.
    fn tone_lands_in_channel(nch: usize, ch: usize) {
        let fs = 1_000_000.0;
        let mut pfb = PfbChannelizer::new(nch, 12);
        let f = pfb.channel_offset_hz(ch, fs);
        let n = nch * 400;
        let input: Vec<IqSample> = (0..n)
            .map(|i| {
                let ph = TAU * f * i as f64 / fs;
                IqSample::new(ph.cos() as f32, ph.sin() as f32)
            })
            .collect();
        let mut out: Vec<Vec<IqSample>> = vec![Vec::new(); nch];
        pfb.process(&input, &mut out);

        // Skip filter settling, then compare mean power per channel.
        let settle = pfb.taps_per_branch * 2;
        let power: Vec<f32> = out
            .iter()
            .map(|c| c[settle..].iter().map(|s| s.norm_sqr()).sum::<f32>() / (c.len() - settle) as f32)
            .collect();
        let best = power
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(best, ch, "tone at {f} Hz should land in ch {ch}, powers: {power:?}");

        // Dominance: winning channel ≥ 20 dB above every other channel.
        for (i, p) in power.iter().enumerate() {
            if i != ch {
                assert!(
                    power[ch] / p.max(1e-12) > 100.0,
                    "ch {ch} not dominant over ch {i}: {power:?}"
                );
            }
        }
    }

    #[test]
    fn dc_tone_lands_in_channel_zero() {
        tone_lands_in_channel(8, 0);
    }

    #[test]
    fn positive_freq_channels() {
        tone_lands_in_channel(8, 1);
        tone_lands_in_channel(8, 3);
    }

    #[test]
    fn negative_freq_channels_wrap() {
        tone_lands_in_channel(8, 7); // -fs/8
        tone_lands_in_channel(8, 5); // -3fs/8
    }

    #[test]
    fn odd_channel_counts_work() {
        tone_lands_in_channel(5, 2);
        tone_lands_in_channel(5, 3); // negative side
    }

    #[test]
    fn output_rate_is_fs_over_m() {
        let nch = 16;
        let mut pfb = PfbChannelizer::new(nch, 8);
        let input = vec![IqSample::new(0.0, 0.0); nch * 100 + 7];
        let mut out = vec![Vec::new(); nch];
        pfb.process(&input, &mut out);
        for c in &out {
            assert_eq!(c.len(), 100); // 7 leftovers buffered as pending
        }
    }

    #[test]
    fn channel_offset_mapping() {
        let pfb = PfbChannelizer::new(8, 8);
        let fs = 800.0;
        assert_eq!(pfb.channel_offset_hz(0, fs), 0.0);
        assert_eq!(pfb.channel_offset_hz(1, fs), 100.0);
        assert_eq!(pfb.channel_offset_hz(4, fs), 400.0);
        assert_eq!(pfb.channel_offset_hz(5, fs), -300.0);
        assert_eq!(pfb.channel_offset_hz(7, fs), -100.0);
    }
}
