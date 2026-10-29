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

/// Interleaved `f32` lanes (re, im) accumulated per register-resident pass of
/// the polyphase sum: 8 complex branches.
const LANES: usize = 16;

pub struct PfbChannelizer {
    nch: usize,
    taps_per_branch: usize,
    /// Polyphase taps laid out **block-major** and duplicated per re/im lane:
    /// `tap_by_block[2 * (l * nch + k) + {0, 1}]` is branch `k`'s tap applied
    /// to the block `l` steps back from the newest. This is the transpose of
    /// the natural `branches[k][l]` layout and is what makes the hot loop
    /// contiguous — see `step`.
    tap_by_block: Vec<f32>,
    /// Ring of the last `taps_per_branch` input blocks, each `nch` complex
    /// samples stored position-reversed (ring position k holds input position
    /// nch−1−k, the sample branch k reads) as interleaved (re, im) `f32`, and
    /// kept twice back to back (slot `b` and `b + taps_per_branch` hold the
    /// same block) so the taps' window is always contiguous. Blocks are
    /// overwritten in place; nothing is ever shifted.
    blocks: Vec<f32>,
    /// Index of the block that will be written next (i.e. one past newest).
    next_block: usize,
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
        // Branch k, tap l reads input position (nch-1-k) of the block l steps
        // back from the newest, and multiplies by proto[k + l*nch]. Blocks are
        // stored position-reversed (see `step`), so that sample sits at ring
        // position k; store the tap indexed by (block, k) so the inner loop
        // walks branches contiguously instead of gathering with stride nch.
        let mut tap_by_block = vec![0.0f32; 2 * total];
        for k in 0..nch {
            for l in 0..taps_per_branch {
                let i = 2 * (l * nch + k);
                tap_by_block[i] = proto[k + l * nch];
                tap_by_block[i + 1] = proto[k + l * nch];
            }
        }
        let ifft = FftPlanner::new().plan_fft(nch, FftDirection::Inverse);
        Self {
            nch,
            taps_per_branch,
            tap_by_block,
            blocks: vec![0.0; 4 * total],
            next_block: 0,
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
    pub fn process(&mut self, mut input: &[IqSample], out: &mut [Vec<IqSample>]) {
        assert_eq!(out.len(), self.nch);
        while !input.is_empty() {
            let take = (self.nch - self.pending.len()).min(input.len());
            self.pending.extend_from_slice(&input[..take]);
            input = &input[take..];
            if self.pending.len() == self.nch {
                self.step(out);
                self.pending.clear();
            }
        }
    }

    fn step(&mut self, out: &mut [Vec<IqSample>]) {
        let nch = self.nch;
        let tpb = self.taps_per_branch;
        let width = 2 * nch;
        // Overwrite the oldest block in the ring with the new one — no shift.
        // (`next_block` points at the oldest, which is what we replace.) The
        // ring is stored twice back to back, so the block written at slot b is
        // also written at slot b + tpb, and position-reversed so ring position
        // k is the sample branch k reads.
        let slot = self.next_block;
        for copy in [slot, slot + tpb] {
            let write = copy * width;
            let dst = self.blocks[write..write + width].as_chunks_mut::<2>().0;
            for (d, s) in dst.iter_mut().zip(self.pending.iter().rev()) {
                *d = [s.re, s.im];
            }
        }
        self.next_block += 1;
        if self.next_block == tpb {
            self.next_block = 0;
        }

        // v[k] = Σ_l p_k[l] · x[n − k − l·M]. Reindexed block-major: tap l
        // touches ring position k of the block l steps back from newest, so
        // accumulate over CONTIGUOUS branches. With the doubled ring, the
        // blocks for l = tpb−1 … 0 are the contiguous slots slot+1 … slot+tpb
        // (newest last), so walking that window backwards walks l forwards
        // with no wraparound.
        //
        // Branches are summed LANES f32 lanes (8 branches) at a time with the
        // accumulators held in registers across all taps, instead of
        // re-loading and re-storing every accumulator once per tap.
        // Bit-identical to the per-tap pass: each lane still starts at +0.0
        // and adds `s · t` for l = 0, 1, … in the same order, and
        // `Complex<f32> * f32` / `+=` are exactly the per-component `re·t`,
        // `im·t` and adds done here (no FMA contraction, no reassociation).
        let window = &self.blocks[(slot + 1) * width..(slot + 1 + tpb) * width];
        let full = width / LANES;
        for ci in 0..full {
            let mut acc = [0.0f32; LANES];
            for (s_blk, t_blk) in window
                .chunks_exact(width)
                .rev()
                .zip(self.tap_by_block.chunks_exact(width))
            {
                let s = &s_blk.as_chunks::<LANES>().0[ci];
                let t = &t_blk.as_chunks::<LANES>().0[ci];
                for ((a, &s), &t) in acc.iter_mut().zip(s.iter()).zip(t.iter()) {
                    *a += s * t;
                }
            }
            store(&mut self.scratch[ci * LANES / 2..], &acc);
        }
        let tail = full * LANES;
        if tail < width {
            let mut acc = [0.0f32; LANES];
            let n = width - tail;
            for (s_blk, t_blk) in window
                .chunks_exact(width)
                .rev()
                .zip(self.tap_by_block.chunks_exact(width))
            {
                for ((a, &s), &t) in acc[..n].iter_mut().zip(&s_blk[tail..]).zip(&t_blk[tail..]) {
                    *a += s * t;
                }
            }
            store(&mut self.scratch[tail / 2..], &acc[..n]);
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

/// Write interleaved (re, im) branch sums into consecutive `v[k]` slots.
fn store(dst: &mut [IqSample], acc: &[f32]) {
    for (d, &[re, im]) in dst.iter_mut().zip(acc.as_chunks::<2>().0) {
        *d = IqSample::new(re, im);
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

    /// Reference implementation: the direct shift-the-delay-line, strided
    /// gather formulation this channelizer replaced. Kept so the block-major
    /// layout can be held to **bit-exact** equality — the restructure is a
    /// performance change, not a maths change.
    struct RefPfb {
        nch: usize,
        taps_per_branch: usize,
        branches: Vec<Vec<f32>>,
        delay: Vec<IqSample>,
        ifft: Arc<dyn Fft<f32>>,
        scratch: Vec<IqSample>,
        pending: Vec<IqSample>,
    }

    impl RefPfb {
        fn new(nch: usize, taps_per_branch: usize) -> Self {
            let total = nch * taps_per_branch;
            let proto = lowpass_taps(0.5 / nch as f64, total);
            let branches: Vec<Vec<f32>> = (0..nch)
                .map(|k| (0..taps_per_branch).map(|l| proto[k + l * nch]).collect())
                .collect();
            Self {
                nch,
                taps_per_branch,
                branches,
                delay: vec![IqSample::new(0.0, 0.0); total],
                ifft: FftPlanner::new().plan_fft(nch, FftDirection::Inverse),
                scratch: vec![IqSample::new(0.0, 0.0); nch],
                pending: Vec::with_capacity(nch),
            }
        }
        fn process(&mut self, input: &[IqSample], out: &mut [Vec<IqSample>]) {
            for &x in input {
                self.pending.push(x);
                if self.pending.len() == self.nch {
                    let total = self.delay.len();
                    self.delay.copy_within(self.nch.., 0);
                    self.delay[total - self.nch..].copy_from_slice(&self.pending);
                    for k in 0..self.nch {
                        let mut acc = IqSample::new(0.0, 0.0);
                        for l in 0..self.taps_per_branch {
                            let j = k + l * self.nch;
                            acc += self.delay[total - 1 - j] * self.branches[k][l];
                        }
                        self.scratch[k] = acc;
                    }
                    self.ifft.process(&mut self.scratch);
                    for (ch, s) in self.scratch.iter().enumerate() {
                        out[ch].push(*s);
                    }
                    self.pending.clear();
                }
            }
        }
    }

    #[test]
    fn matches_direct_reference_bit_exactly() {
        let mut s = 0x0bad_c0de_dead_beefu64;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s as f32 / u64::MAX as f32) * 2.0 - 1.0
        };
        let input: Vec<IqSample> = (0..60_000).map(|_| IqSample::new(next(), next())).collect();

        for &(nch, tpb) in &[(4usize, 3usize), (8, 8), (16, 4), (48, 8), (5, 6)] {
            let mut fast = PfbChannelizer::new(nch, tpb);
            let mut refr = RefPfb::new(nch, tpb);
            let mut a: Vec<Vec<IqSample>> = vec![Vec::new(); nch];
            let mut b: Vec<Vec<IqSample>> = vec![Vec::new(); nch];
            // Uneven blocks so the pending buffer and ring wrap at odd offsets.
            for blk in input.chunks(1013) {
                fast.process(blk, &mut a);
                refr.process(blk, &mut b);
            }
            for ch in 0..nch {
                assert_eq!(a[ch].len(), b[ch].len(), "nch={nch} tpb={tpb} ch={ch} len");
                for (i, (x, y)) in a[ch].iter().zip(b[ch].iter()).enumerate() {
                    assert_eq!(
                        (x.re.to_bits(), x.im.to_bits()),
                        (y.re.to_bits(), y.im.to_bits()),
                        "nch={nch} tpb={tpb} ch={ch} sample {i}: {x:?} vs {y:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn polyphase_bit_exact_across_block_sizes() {
        let mut s = 0x1234_5678_9abc_def1u64;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
        };
        let input: Vec<IqSample> = (0..9_000).map(|_| IqSample::new(next(), next())).collect();
        // Block-size cycles: all 1-sample, small primes, sizes that are not a
        // multiple of any tested M, and a large irregular mix.
        let sequences: &[&[usize]] = &[&[1], &[3, 5, 7], &[13, 1, 97, 2], &[1000, 1, 47, 333]];

        // 12 and 13 exercise a full 8-branch pass plus a partial tail pass.
        for &nch in &[2usize, 3, 8, 12, 13, 24, 48] {
            for &tpb in &[2usize, 8, 16] {
                for seq in sequences {
                    let mut fast = PfbChannelizer::new(nch, tpb);
                    let mut refr = RefPfb::new(nch, tpb);
                    let mut a: Vec<Vec<IqSample>> = vec![Vec::new(); nch];
                    let mut b: Vec<Vec<IqSample>> = vec![Vec::new(); nch];
                    let mut pos = 0;
                    for &len in seq.iter().cycle() {
                        if pos >= input.len() {
                            break;
                        }
                        let end = (pos + len).min(input.len());
                        fast.process(&input[pos..end], &mut a);
                        refr.process(&input[pos..end], &mut b);
                        pos = end;
                    }
                    for ch in 0..nch {
                        assert_eq!(
                            a[ch].len(),
                            b[ch].len(),
                            "nch={nch} tpb={tpb} {seq:?} ch={ch}"
                        );
                        assert_eq!(a[ch].len(), input.len() / nch);
                        for (i, (x, y)) in a[ch].iter().zip(b[ch].iter()).enumerate() {
                            assert_eq!(
                                (x.re.to_bits(), x.im.to_bits()),
                                (y.re.to_bits(), y.im.to_bits()),
                                "nch={nch} tpb={tpb} {seq:?} ch={ch} sample {i}"
                            );
                        }
                    }
                }
            }
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
