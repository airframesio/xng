//! Polyphase filter-bank (PFB) analysis channelizer, critically sampled or
//! 2× oversampled.
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
//!
//! [`Oversample::Twice`] steps by `D = M/2` input samples instead: with the
//! ring holding `2L` blocks of `D` samples, `v[p + r·D]` sums
//! `h[q·D + p] · x[n − q·D − p]` over the blocks `q ≡ r (mod 2)`, and odd
//! bins are negated on odd output steps (the `(−1)^{i·m}` phase the half-block
//! advance leaves on bin `i`).

use crate::fir::lowpass_taps;
use crate::IqSample;
use rustfft::{Fft, FftDirection, FftPlanner};
use std::sync::Arc;

/// Interleaved `f32` lanes (re, im) accumulated per register-resident pass of
/// the polyphase sum: 8 complex branches.
const LANES: usize = 16;

/// How far the bank's output rate sits above its bin spacing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Oversample {
    /// Decimate by `M`: each bin is output at its own spacing `fs / M`, so
    /// everything ½–1½ bins from a bin center folds into that bin.
    Critical,
    /// Decimate by `M / 2`: each bin is output at `2 · fs / M`. The prototype
    /// cutoff sits at one bin spacing, so a channel may sit anywhere within
    /// ±½ spacing of its bin center flat, and only content past
    /// `2·fs/M − (residual + passband)` folds onto it. Twice the polyphase
    /// and FFT work per input sample of [`Critical`](Self::Critical).
    Twice,
}

impl Oversample {
    /// Bin output rate over bin spacing.
    fn factor(self) -> usize {
        match self {
            Self::Critical => 1,
            Self::Twice => 2,
        }
    }
}

pub struct PfbChannelizer {
    nch: usize,
    taps_per_branch: usize,
    /// Input samples per output step: `nch / oversample factor`.
    decim: usize,
    oversample: Oversample,
    /// Ring depth in `decim`-sample blocks: `oversample factor · taps_per_branch`.
    nblocks: usize,
    /// Prototype taps duplicated per re/im lane: `tap_by_block[2·(q·D + p) + {0, 1}]`
    /// is `h[q·D + p]`, applied to position `p` of the block `q` steps back
    /// from the newest (`D` = `decim`). Critically sampled (`D = nch`), this
    /// is branch `p`'s tap `q` — the block-major transpose of the natural
    /// `branches[k][l]` layout, which is what makes the hot loop contiguous
    /// (see `step`).
    tap_by_block: Vec<f32>,
    /// Ring of the last `nblocks` input blocks, each `decim` complex samples
    /// stored position-reversed (ring position p holds input position
    /// decim−1−p, the sample tap p reads) as interleaved (re, im) `f32`, and
    /// kept twice back to back (slot `b` and `b + nblocks` hold the same
    /// block) so the taps' window is always contiguous. Blocks are
    /// overwritten in place; nothing is ever shifted.
    blocks: Vec<f32>,
    /// Index of the block that will be written next (i.e. one past newest).
    next_block: usize,
    /// Whether the next output step is odd, for the oversampled bank's
    /// odd-bin sign flip.
    odd_step: bool,
    ifft: Arc<dyn Fft<f32>>,
    /// Scratch for the IFFT (v[k] → channel outputs).
    scratch: Vec<IqSample>,
    /// Partial input block buffered until `decim` samples are available.
    pending: Vec<IqSample>,
    /// Bins `process` appends output to (ascending, deduplicated); `None` =
    /// all bins.
    active: Option<Vec<usize>>,
}

impl PfbChannelizer {
    /// Create a critically sampled channelizer with `nch` channels and
    /// `taps_per_branch` taps per polyphase branch (8–16 is typical; more =
    /// sharper channel edges, more CPU).
    pub fn new(nch: usize, taps_per_branch: usize) -> Self {
        Self::with_oversample(nch, taps_per_branch, Oversample::Critical)
    }

    /// Create a channelizer with the given oversampling. [`Oversample::Twice`]
    /// needs an even `nch ≥ 4`; its prototype cutoff sits at one bin spacing
    /// instead of half of one.
    pub fn with_oversample(nch: usize, taps_per_branch: usize, oversample: Oversample) -> Self {
        assert!(nch >= 2 && taps_per_branch >= 2);
        let r = oversample.factor();
        assert!(
            nch.is_multiple_of(r) && nch >= 2 * r,
            "{nch} channels unsupported for {oversample:?} oversampling"
        );
        let decim = nch / r;
        let total = nch * taps_per_branch;
        // Prototype lowpass: passband half-width = half the bin output rate
        // (half a channel critically sampled, a full one at 2×).
        let proto = lowpass_taps(0.5 * r as f64 / nch as f64, total);
        // h[j], j = q·D + p, multiplies x[n − j]: input position D−1−p of the
        // block q steps back from the newest. Blocks are stored
        // position-reversed (see `step`), so that sample sits at ring
        // position p and the taps are just the prototype in order (lane
        // doubled): the inner loop walks positions contiguously instead of
        // gathering with stride D.
        let tap_by_block: Vec<f32> = proto.iter().flat_map(|&t| [t, t]).collect();
        let ifft = FftPlanner::new().plan_fft(nch, FftDirection::Inverse);
        Self {
            nch,
            taps_per_branch,
            decim,
            oversample,
            nblocks: r * taps_per_branch,
            tap_by_block,
            blocks: vec![0.0; 4 * total],
            next_block: 0,
            odd_step: false,
            ifft,
            scratch: vec![IqSample::new(0.0, 0.0); nch],
            pending: Vec::with_capacity(decim),
            active: None,
        }
    }

    pub fn taps_per_branch(&self) -> usize {
        self.taps_per_branch
    }

    /// Input samples consumed per output sample (per channel): `nch`
    /// critically sampled, `nch / 2` at 2×.
    pub fn decimation(&self) -> usize {
        self.decim
    }

    pub fn oversample(&self) -> Oversample {
        self.oversample
    }

    pub fn num_channels(&self) -> usize {
        self.nch
    }

    /// Restrict output to the listed bins: from now on `process` appends
    /// samples only to `out[i]` for each listed `i`. Inactive bins receive no
    /// samples — their `out` vectors are left untouched. The polyphase sum and
    /// IFFT still compute every bin, so active bins' outputs are identical to
    /// an unrestricted channelizer's; this only skips the per-bin pushes
    /// nobody reads. Duplicates are ignored, and out-of-range indices
    /// (`≥ num_channels()`) are dropped. Without a call, all bins are active.
    pub fn set_active_bins(&mut self, bins: &[usize]) {
        debug_assert!(
            bins.iter().all(|&b| b < self.nch),
            "active bins {bins:?} out of range for {} channels",
            self.nch
        );
        let mut active: Vec<usize> = bins.iter().copied().filter(|&b| b < self.nch).collect();
        active.sort_unstable();
        active.dedup();
        self.active = Some(active);
    }

    /// Center frequency of channel `i` relative to the capture center, given
    /// the input sample rate. Channels above `nch/2` wrap negative.
    pub fn channel_offset_hz(&self, i: usize, sample_rate: f64) -> f64 {
        let i = i % self.nch;
        let idx = if i <= self.nch / 2 { i as f64 } else { i as f64 - self.nch as f64 };
        idx * sample_rate / self.nch as f64
    }

    /// Feed input samples; for every full block of `decimation()` inputs, one output
    /// sample per channel is appended to `out[ch]` (only for active channels
    /// when [`set_active_bins`](Self::set_active_bins) has been called).
    ///
    /// `out` must contain `nch` vectors.
    pub fn process(&mut self, mut input: &[IqSample], out: &mut [Vec<IqSample>]) {
        assert_eq!(out.len(), self.nch);
        while !input.is_empty() {
            let take = (self.decim - self.pending.len()).min(input.len());
            self.pending.extend_from_slice(&input[..take]);
            input = &input[take..];
            if self.pending.len() == self.decim {
                self.step(out);
                self.pending.clear();
            }
        }
    }

    fn step(&mut self, out: &mut [Vec<IqSample>]) {
        let nb = self.nblocks;
        let width = 2 * self.decim;
        // Overwrite the oldest block in the ring with the new one — no shift.
        // (`next_block` points at the oldest, which is what we replace.) The
        // ring is stored twice back to back, so the block written at slot b is
        // also written at slot b + nb, and position-reversed so ring position
        // p is the sample tap p reads.
        let slot = self.next_block;
        for copy in [slot, slot + nb] {
            let write = copy * width;
            let dst = self.blocks[write..write + width].as_chunks_mut::<2>().0;
            for (d, s) in dst.iter_mut().zip(self.pending.iter().rev()) {
                *d = [s.re, s.im];
            }
        }
        self.next_block += 1;
        if self.next_block == nb {
            self.next_block = 0;
        }

        // With the doubled ring, the blocks q = nb−1 … 0 steps back are the
        // contiguous slots slot+1 … slot+nb (newest last), so walking that
        // window backwards walks q forwards with no wraparound.
        let window = &self.blocks[(slot + 1) * width..(slot + 1 + nb) * width];
        match self.oversample {
            Oversample::Critical => {
                polyphase_critical(window, &self.tap_by_block, width, &mut self.scratch);
            }
            Oversample::Twice => {
                polyphase_twice(window, &self.tap_by_block, width, &mut self.scratch);
            }
        }

        self.ifft.process(&mut self.scratch);
        // The IDFT leaves bin i modulated by e^{j2π·i·n/M} at the newest
        // sample n. Critically sampled, n advances by M per step and that is
        // a constant phase; at D = M/2 it flips odd bins' sign every other
        // step, which is undone here.
        if self.oversample == Oversample::Twice {
            if self.odd_step {
                for s in self.scratch.iter_mut().skip(1).step_by(2) {
                    *s = -*s;
                }
            }
            self.odd_step = !self.odd_step;
        }
        match &self.active {
            Some(bins) => {
                for &ch in bins {
                    out[ch].push(self.scratch[ch]);
                }
            }
            None => {
                for (ch, sample) in self.scratch.iter().enumerate() {
                    // Scale so a full-scale input tone yields ~unit channel output
                    // (IDFT is unnormalized; prototype has unity DC gain split
                    // across branches).
                    out[ch].push(*sample);
                }
            }
        }
    }
}

/// Critically sampled polyphase sum `v[k] = Σ_l p_k[l] · x[n − k − l·M]`,
/// with `window` the ring's `L` blocks of `width = 2·M` floats (oldest first,
/// so it is walked backwards) and `taps` the lane-doubled prototype: tap l
/// touches ring position k of the block l steps back from newest, so the sum
/// runs over CONTIGUOUS branches.
///
/// Branches are summed LANES f32 lanes (8 branches) at a time with the
/// accumulators held in registers across all taps, instead of re-loading and
/// re-storing every accumulator once per tap. Bit-identical to the per-tap
/// pass: each lane still starts at +0.0 and adds `s · t` for l = 0, 1, … in
/// the same order, and `Complex<f32> * f32` / `+=` are exactly the
/// per-component `re·t`, `im·t` and adds done here (no FMA contraction, no
/// reassociation). Kept separate from [`polyphase_twice`]: the extra
/// sub-slicing there costs this path ~5%.
#[inline(always)]
fn polyphase_critical(window: &[f32], taps: &[f32], width: usize, v: &mut [IqSample]) {
    let full = width / LANES;
    for ci in 0..full {
        let mut acc = [0.0f32; LANES];
        for (s_blk, t_blk) in window.chunks_exact(width).rev().zip(taps.chunks_exact(width)) {
            let s = &s_blk.as_chunks::<LANES>().0[ci];
            let t = &t_blk.as_chunks::<LANES>().0[ci];
            for ((a, &s), &t) in acc.iter_mut().zip(s.iter()).zip(t.iter()) {
                *a += s * t;
            }
        }
        store(&mut v[ci * LANES / 2..], &acc);
    }
    let tail = full * LANES;
    if tail < width {
        let mut acc = [0.0f32; LANES];
        let n = width - tail;
        for (s_blk, t_blk) in window.chunks_exact(width).rev().zip(taps.chunks_exact(width)) {
            for ((a, &s), &t) in acc[..n].iter_mut().zip(&s_blk[tail..]).zip(&t_blk[tail..]) {
                *a += s * t;
            }
        }
        store(&mut v[tail / 2..], &acc[..n]);
    }
}

/// 2× oversampled polyphase sum
/// `v[p + r·D] = Σ_{q ≡ r (mod 2)} h[q·D + p] · x[n − q·D − p]`, with
/// `window` the ring's `2L` blocks of `width = 2·D` floats (oldest first) and
/// `taps` the lane-doubled prototype. Same register-resident lane passes as
/// [`polyphase_critical`], once per parity r; blocks are walked in pairs so
/// each parity's sum stays a plain contiguous chunk walk (an iterator
/// `step_by` costs ~3×).
#[inline(always)]
fn polyphase_twice(window: &[f32], taps: &[f32], width: usize, v: &mut [IqSample]) {
    let d = width / 2;
    let full = width / LANES;
    let pair = 2 * width;
    for r in 0..2 {
        // In a pair of window blocks (oldest first) block q ≡ r sits at 1−r;
        // in a pair of tap blocks (q ascending) it sits at r.
        let s_off = (1 - r) * width;
        let t_off = r * width;
        let base = r * d;
        for ci in 0..full {
            let mut acc = [0.0f32; LANES];
            for (s_grp, t_grp) in window.chunks_exact(pair).rev().zip(taps.chunks_exact(pair)) {
                let s = &s_grp[s_off..s_off + width].as_chunks::<LANES>().0[ci];
                let t = &t_grp[t_off..t_off + width].as_chunks::<LANES>().0[ci];
                for ((a, &s), &t) in acc.iter_mut().zip(s.iter()).zip(t.iter()) {
                    *a += s * t;
                }
            }
            store(&mut v[base + ci * LANES / 2..], &acc);
        }
        let tail = full * LANES;
        if tail < width {
            let mut acc = [0.0f32; LANES];
            let n = width - tail;
            for (s_grp, t_grp) in window.chunks_exact(pair).rev().zip(taps.chunks_exact(pair)) {
                let s = &s_grp[s_off + tail..s_off + width];
                let t = &t_grp[t_off + tail..t_off + width];
                for ((a, &s), &t) in acc[..n].iter_mut().zip(s).zip(t) {
                    *a += s * t;
                }
            }
            store(&mut v[base + tail / 2..base + d], &acc[..n]);
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
    fn active_bins_bit_exact_and_inactive_untouched() {
        let mut s = 0xfeed_face_cafe_f00du64;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
        };
        let input: Vec<IqSample> = (0..20_000).map(|_| IqSample::new(next(), next())).collect();

        // Unsorted, with a duplicate; 13 has a partial tail pass.
        for &(nch, active) in &[(48usize, &[47usize, 0, 5, 5, 24, 30][..]), (13, &[12, 1][..])] {
            let mut all = PfbChannelizer::new(nch, 16);
            let mut some = PfbChannelizer::new(nch, 16);
            some.set_active_bins(active);
            let mut a: Vec<Vec<IqSample>> = vec![Vec::new(); nch];
            let mut b: Vec<Vec<IqSample>> = vec![Vec::new(); nch];
            let mut pos = 0;
            for &len in [1usize, 97, 1000, 3, 4801, 47].iter().cycle() {
                if pos >= input.len() {
                    break;
                }
                let end = (pos + len).min(input.len());
                all.process(&input[pos..end], &mut a);
                some.process(&input[pos..end], &mut b);
                pos = end;
            }
            for ch in 0..nch {
                if !active.contains(&ch) {
                    assert!(b[ch].is_empty(), "nch={nch} inactive ch={ch} got samples");
                    continue;
                }
                assert_eq!(b[ch].len(), input.len() / nch, "nch={nch} ch={ch} len");
                for (i, (x, y)) in a[ch].iter().zip(b[ch].iter()).enumerate() {
                    assert_eq!(
                        (x.re.to_bits(), x.im.to_bits()),
                        (y.re.to_bits(), y.im.to_bits()),
                        "nch={nch} ch={ch} sample {i}"
                    );
                }
            }
        }
    }

    fn xorshift_input(seed: u64, len: usize) -> Vec<IqSample> {
        let mut s = seed;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
        };
        (0..len).map(|_| IqSample::new(next(), next())).collect()
    }

    fn assert_bit_exact(a: &[IqSample], b: &[IqSample], what: &str) {
        assert_eq!(a.len(), b.len(), "{what} len");
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(
                (x.re.to_bits(), x.im.to_bits()),
                (y.re.to_bits(), y.im.to_bits()),
                "{what} sample {i}: {x:?} vs {y:?}"
            );
        }
    }

    /// Oversampled bank vs a direct f64 evaluation of each bin's baseband
    /// output: `y_i[m] = Σ_j h[j]·x[n−j]·e^{j2πij/M} · e^{−j2πi(n−D+1)/M}`,
    /// `n` = newest sample of step `m`.
    #[test]
    fn oversampled_matches_direct_modulated_filter() {
        for &(nch, tpb) in &[(4usize, 3usize), (8, 6), (12, 4), (24, 8), (48, 5), (34, 4)] {
            let input = xorshift_input(0x5eed_1234_abcd_0001, nch * 200 + 5);
            let mut pfb = PfbChannelizer::with_oversample(nch, tpb, Oversample::Twice);
            let d = pfb.decimation();
            assert_eq!(d, nch / 2);
            assert_eq!(pfb.taps_per_branch(), tpb);
            let mut out: Vec<Vec<IqSample>> = vec![Vec::new(); nch];
            for blk in input.chunks(37) {
                pfb.process(blk, &mut out);
            }
            let proto = lowpass_taps(1.0 / nch as f64, nch * tpb);
            let steps = input.len() / d;
            for (i, ch) in out.iter().enumerate() {
                assert_eq!(ch.len(), steps);
                for (m, got) in ch.iter().enumerate() {
                    let n = m * d + d - 1;
                    let mut acc = num_complex::Complex::<f64>::new(0.0, 0.0);
                    for (j, &h) in proto.iter().enumerate().take(n + 1) {
                        let x = input[n - j];
                        let ph = TAU * (i * j) as f64 / nch as f64;
                        acc += num_complex::Complex::new(f64::from(x.re), f64::from(x.im))
                            * f64::from(h)
                            * num_complex::Complex::from_polar(1.0, ph);
                    }
                    let ph = -TAU * (i * (n + 1 - d)) as f64 / nch as f64;
                    let want = acc * num_complex::Complex::from_polar(1.0, ph);
                    let err = num_complex::Complex::new(
                        f64::from(got.re) - want.re,
                        f64::from(got.im) - want.im,
                    )
                    .norm();
                    assert!(
                        err < 1e-4,
                        "nch={nch} tpb={tpb} bin {i} step {m}: {got:?} vs {want:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn oversampled_tone_lands_in_channel() {
        let nch = 16;
        let fs = 1_600_000.0;
        for ch in [0usize, 1, 3, 9, 15] {
            let mut pfb = PfbChannelizer::with_oversample(nch, 8, Oversample::Twice);
            let f = pfb.channel_offset_hz(ch, fs) + 0.3 * fs / nch as f64;
            let input: Vec<IqSample> = (0..nch * 400)
                .map(|i| {
                    let ph = TAU * f * i as f64 / fs;
                    IqSample::new(ph.cos() as f32, ph.sin() as f32)
                })
                .collect();
            let mut out: Vec<Vec<IqSample>> = vec![Vec::new(); nch];
            pfb.process(&input, &mut out);
            // A tone 0.3 bin off center sits flat in its bin, at +0.3 bin.
            let c = &out[ch][64..];
            let amp = c.iter().map(|s| s.norm()).sum::<f32>() / c.len() as f32;
            assert!((amp - 1.0).abs() < 1e-3, "ch {ch}: amp {amp}");
            let rot: num_complex::Complex<f32> = c.windows(2).map(|w| w[1] * w[0].conj()).sum();
            let got = f64::from(rot.arg()) / TAU * (2.0 * fs / nch as f64);
            assert!((got - 0.3 * fs / nch as f64).abs() < 10.0, "ch {ch}: freq {got}");
        }
    }

    #[test]
    fn oversampled_active_bins_bit_exact() {
        let input = xorshift_input(0x0dd5_7e95_0fb1_7500, 20_000);
        // Odd and even bins (odd ones take the sign flip), unsorted with a
        // duplicate; 26 = 2·13 has a partial tail pass per parity.
        for &(nch, active) in &[(48usize, &[47usize, 0, 5, 5, 24, 30][..]), (26, &[25, 1, 2][..])] {
            let mut all = PfbChannelizer::with_oversample(nch, 7, Oversample::Twice);
            let mut some = PfbChannelizer::with_oversample(nch, 7, Oversample::Twice);
            some.set_active_bins(active);
            let mut a: Vec<Vec<IqSample>> = vec![Vec::new(); nch];
            let mut b: Vec<Vec<IqSample>> = vec![Vec::new(); nch];
            let mut pos = 0;
            for &len in [1usize, 97, 1000, 3, 4801, 47].iter().cycle() {
                if pos >= input.len() {
                    break;
                }
                let end = (pos + len).min(input.len());
                all.process(&input[pos..end], &mut a);
                some.process(&input[pos..end], &mut b);
                pos = end;
            }
            for ch in 0..nch {
                if !active.contains(&ch) {
                    assert!(b[ch].is_empty(), "nch={nch} inactive ch={ch} got samples");
                    continue;
                }
                assert_eq!(b[ch].len(), input.len() / (nch / 2), "nch={nch} ch={ch} len");
                assert_bit_exact(&a[ch], &b[ch], &format!("nch={nch} ch={ch}"));
            }
        }
    }

    #[test]
    fn oversampled_bit_exact_across_block_sizes() {
        let input = xorshift_input(0x7a11_b0a7_5eed_0002, 9_000);
        let sequences: &[&[usize]] = &[&[1], &[3, 5, 7], &[13, 1, 97, 2], &[1000, 1, 47, 333]];
        for &nch in &[4usize, 8, 24, 26, 48] {
            for &tpb in &[2usize, 7, 16] {
                let mut whole = PfbChannelizer::with_oversample(nch, tpb, Oversample::Twice);
                let mut a: Vec<Vec<IqSample>> = vec![Vec::new(); nch];
                whole.process(&input, &mut a);
                for seq in sequences {
                    let mut chunked = PfbChannelizer::with_oversample(nch, tpb, Oversample::Twice);
                    let mut b: Vec<Vec<IqSample>> = vec![Vec::new(); nch];
                    let mut pos = 0;
                    for &len in seq.iter().cycle() {
                        if pos >= input.len() {
                            break;
                        }
                        let end = (pos + len).min(input.len());
                        chunked.process(&input[pos..end], &mut b);
                        pos = end;
                    }
                    for ch in 0..nch {
                        assert_eq!(a[ch].len(), input.len() / (nch / 2));
                        assert_bit_exact(
                            &a[ch],
                            &b[ch],
                            &format!("nch={nch} tpb={tpb} {seq:?} ch={ch}"),
                        );
                    }
                }
            }
        }
    }

    #[test]
    #[should_panic(expected = "unsupported")]
    fn oversampled_rejects_odd_channel_count() {
        let _ = PfbChannelizer::with_oversample(13, 8, Oversample::Twice);
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
