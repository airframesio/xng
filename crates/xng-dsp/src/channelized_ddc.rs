//! Channelizer-based multi-channel downconverter.
//!
//! A drop-in alternative to [`SharedDdc`](crate::SharedDdc) with the same
//! interface, but built on the polyphase [`PfbChannelizer`](crate::PfbChannelizer):
//! one shared polyphase + FFT pass produces every channel at once, so the cost
//! is **independent of how many channels are requested and of how far apart
//! they are** — unlike a shared decimation, whose coarse stage can only
//! decimate as far as the *widest* channel allows.
//!
//! How a requested channel is served:
//!  1. The capture is split into `M` evenly spaced bins of `fs / M` each.
//!  2. Each requested channel snaps to its nearest bin (within ±½ bin).
//!  3. A small residual NCO removes the leftover offset (channel center − bin
//!     center) at the low bin rate.
//!  4. When the bin is wide enough that resampling it would fold out-of-channel
//!     content into the passband, an anti-alias lowpass (decimating by the
//!     integer part of `bin rate / channel rate`) runs first.
//!  5. A resampler corrects the remaining rate to the exact channel rate.
//!
//! `M` is chosen so each bin comfortably carries the channel passband and the
//! bin rate is a small integer-ish multiple of the output rate (cheap final
//! resample).
//!
//! With [`Oversample::Twice`] the bank outputs each bin at twice its spacing,
//! so a channel anywhere within ±½ bin of a bin center is served flat and
//! alias-free (no raster alignment needed), at roughly twice the shared
//! polyphase + FFT cost; `M` and the prototype length are then chosen by
//! [`choose_oversampled_bins`].

use crate::channelizer::{Oversample, PfbChannelizer};
use crate::fir::{lowpass_taps, Fir};
use crate::nco::Nco;
use crate::resample::Resampler;
use crate::IqSample;

/// Polyphase taps per branch. See the rejection/cost note at the
/// `PfbChannelizer::new` call site.
const PFB_TAPS_PER_BRANCH: usize = 16;

/// Same windowed-sinc transition sizing as [`SharedDdc`](crate::SharedDdc)
/// and [`Ddc`](crate::Ddc).
const TAPS_PER_TRANSITION: f64 = 5.5;

/// Largest admissible bin rate as a multiple of the output rate. Bounds the
/// per-channel decimation the anti-alias stage must do (and so its tap count
/// and per-channel cost); beyond it, [`SharedDdc`](crate::SharedDdc) is the
/// better front end.
const MAX_BIN_TO_OUTPUT_RATIO: f64 = 4.0;

/// Oversampled bank: prototype length per transition, in bin spacings. A
/// Blackman-Harris windowed sinc reaches its ~−110 dB floor one main-lobe
/// half-width (4/N) from the cutoff, so the full transition 2·(S − fp) needs
/// N·tw/fs = 8, i.e. `taps_per_branch = 8·S / tw`.
const OVERSAMPLED_TAPS_PER_TRANSITION: f64 = 8.0;
/// Fewest / most taps per branch for the oversampled prototype.
const OVERSAMPLED_MIN_TAPS: usize = 4;
const OVERSAMPLED_MAX_TAPS: usize = 16;
/// Anti-alias stage sizing behind an oversampled bank, whose bins pass far
/// more out-of-channel content than a critically sampled one (≈ −100 dB vs
/// ≈ −70 dB for [`TAPS_PER_TRANSITION`]).
const OVERSAMPLED_AA_TAPS_PER_TRANSITION: f64 = 8.0;
/// Largest rate (as a multiple of the output rate) an oversampled bin may
/// hand the cubic resampler after the optional anti-alias decimation. An
/// oversampled bin still carries content well past the passband (out to the
/// prototype's cutoff one bin spacing away), and the Catmull-Rom resampler
/// only keeps its images out of the passband close to unity ratio.
const MAX_RESAMPLE_RATIO: f64 = 1.25;

/// One requested channel's mapping onto the bin grid + its cheap back end.
struct Channel {
    /// Which channelizer bin carries this channel.
    bin: usize,
    /// Residual NCO removing (channel center − bin center) at the bin rate.
    nco: Nco,
    /// Anti-alias lowpass (with integer decimation) at the bin rate, present
    /// when resampling the raw bin would fold content into the passband.
    aa: Option<Fir>,
    resampler: Option<Resampler>,
    /// The selected bin's samples for this block (NCO-mixed in place).
    binbuf: Vec<IqSample>,
    /// Anti-alias output, fed to the resampler.
    aabuf: Vec<IqSample>,
}

pub struct ChannelizedDdc {
    pfb: PfbChannelizer,
    /// Output rate of every bin: `fs / M`, or `2·fs / M` oversampled.
    bin_rate: f64,
    /// One output vector per bin. The polyphase step computes every bin (that
    /// is the shared work), but only the bins channels are snapped to are
    /// active, so the other vectors stay empty.
    bins: Vec<Vec<IqSample>>,
    channels: Vec<Channel>,
}

impl ChannelizedDdc {
    /// Same signature as [`SharedDdc::new`](crate::SharedDdc::new);
    /// critically sampled bank.
    pub fn new(
        input_rate: f64,
        output_rate: f64,
        offsets: &[f64],
        passband_hz: f64,
    ) -> Result<Self, String> {
        Self::with_oversample(input_rate, output_rate, offsets, passband_hz, Oversample::Critical)
    }

    /// As [`Self::new`], with the bank's oversampling chosen explicitly.
    pub fn with_oversample(
        input_rate: f64,
        output_rate: f64,
        offsets: &[f64],
        passband_hz: f64,
        oversample: Oversample,
    ) -> Result<Self, String> {
        Self::build(input_rate, output_rate, offsets, passband_hz, oversample, MAX_RESAMPLE_RATIO)
    }

    /// [`Self::with_oversample`] with the oversampled bank's resample guard
    /// (see [`MAX_RESAMPLE_RATIO`]) as a parameter, so tests can disable it.
    fn build(
        input_rate: f64,
        output_rate: f64,
        offsets: &[f64],
        passband_hz: f64,
        oversample: Oversample,
        max_resample_ratio: f64,
    ) -> Result<Self, String> {
        if offsets.is_empty() {
            return Err("ChannelizedDdc needs at least one channel".to_string());
        }
        if output_rate < 2.0 * passband_hz {
            return Err(format!(
                "output rate {output_rate} cannot carry a ±{passband_hz} Hz passband"
            ));
        }

        // Choose M (number of bins). Each bin must carry the full channel
        // (its rate fs/M ≥ 2·passband with margin) and we want fs/M close to
        // a small multiple of output_rate so the final resample is gentle.
        // Target a bin rate of ~output_rate (critically sampled at the channel
        // rate is too tight for the channel filter, so allow up to ~2×).
        let (mut pfb, max_safe_residual, aa_taps_per_transition) = match oversample {
            Oversample::Critical => {
                let m = choose_num_bins(input_rate, output_rate, passband_hz, offsets)?;
                let spacing = input_rate / m as f64;
                // Rejection at the neighbouring bin's *center* (~109 dB at 8
                // taps) is not the figure that matters: the bank is critically
                // sampled, so everything ½–1½ bins from a bin center folds
                // into that bin. A channel off its bin center (±12.5 kHz in a
                // 50 kHz bin on the real US ACARS plan) gets that fold on top
                // of its own spectrum, where no downstream filter can remove
                // it. Prototype response at 0.75 bin: 8 taps −30 dB, 12 taps
                // −57 dB, 16 taps −111 dB.
                let pfb = PfbChannelizer::new(m, PFB_TAPS_PER_BRANCH);
                (pfb, spacing / 2.0 - passband_hz, TAPS_PER_TRANSITION)
            }
            Oversample::Twice => {
                let (m, tpb) = choose_oversampled_bins(
                    input_rate,
                    output_rate,
                    passband_hz,
                    offsets,
                    max_resample_ratio,
                )?;
                let spacing = input_rate / m as f64;
                // The prototype is sized for the worst residual, so any
                // nearest-bin snap (≤ ½ spacing, plus rounding) fits.
                let pfb = PfbChannelizer::with_oversample(m, tpb, oversample);
                (pfb, spacing / 2.0 + 1e-6, OVERSAMPLED_AA_TAPS_PER_TRANSITION)
            }
        };
        let m = pfb.num_channels();
        let spacing = input_rate / m as f64;
        let bin_rate = input_rate / pfb.decimation() as f64;

        let mut channels = Vec::with_capacity(offsets.len());
        for &off in offsets {
            // Snap to the nearest bin: bin index k maps to k·fs/M, with
            // k > M/2 wrapping to negative frequencies (FFT bin convention).
            let k_round = (off / spacing).round();
            let bin = k_round.rem_euclid(m as f64) as usize;
            let bin_center = bin_offset_hz(bin, m, input_rate);
            let residual = off - bin_center;
            // The channel must fit inside the selected bin: not just its center
            // within ±½ bin, but its whole ±passband. A channel whose passband
            // reaches past the bin edge would be silently attenuated, so reject
            // it here and let the caller fall back to SharedDdc. `choose_num_bins`
            // sizes the bins (≥ 2.4·passband) so the real airband raster passes;
            // this guards pathological rate/offset sets.
            if residual.abs() > max_safe_residual {
                return Err(format!(
                    "channel offset {off} Hz is too close to a {m}-bin channelizer edge"
                ));
            }
            let (aa, post_rate) =
                anti_alias(bin_rate, output_rate, passband_hz, aa_taps_per_transition)?;
            let resampler = if (post_rate - output_rate).abs() > 1e-6 {
                Some(Resampler::new(post_rate, output_rate))
            } else {
                None
            };
            channels.push(Channel {
                bin,
                nco: Nco::new(residual, bin_rate),
                aa,
                resampler,
                binbuf: Vec::new(),
                aabuf: Vec::new(),
            });
        }
        // Only the snapped bins are read downstream; skip pushing the rest.
        pfb.set_active_bins(&channels.iter().map(|c| c.bin).collect::<Vec<_>>());

        Ok(Self { pfb, bin_rate, bins: vec![Vec::new(); m], channels })
    }

    /// Number of channels.
    pub fn num_channels(&self) -> usize {
        self.channels.len()
    }

    /// Bank geometry, for benches and logs: bin count, prototype taps per
    /// branch, bin output rate, and whether channels need an anti-alias
    /// stage / resampler.
    pub fn describe(&self) -> String {
        let aa = self.channels.iter().any(|c| c.aa.is_some());
        let rs = self.channels.iter().any(|c| c.resampler.is_some());
        format!(
            "M={} {:?} tpb={} bin_rate={:.0} aa={aa} resample={rs}",
            self.pfb.num_channels(),
            self.pfb.oversample(),
            self.pfb.taps_per_branch(),
            self.bin_rate
        )
    }

    /// Downconvert a wideband block into all channels (same contract as
    /// [`SharedDdc::process`](crate::SharedDdc::process)).
    pub fn process(&mut self, input: &[IqSample], out: &mut [Vec<IqSample>]) {
        assert_eq!(out.len(), self.channels.len(), "out must have one vec per channel");

        // --- Shared polyphase + FFT: computes every bin, fills the active ones. ---
        for b in &mut self.bins {
            b.clear();
        }
        self.pfb.process(input, &mut self.bins);

        // --- Per channel: pull its bin, residual-mix, anti-alias, resample. ---
        // A copy out of the shared bin is unavoidable (the NCO mixes in place
        // and several channels may read the same bin), but when there is no
        // back-end stage the copy can land straight in the caller's buffer and
        // be mixed there, saving a whole buffer pass at the channel rate.
        for (ch, out_buf) in self.channels.iter_mut().zip(out.iter_mut()) {
            out_buf.clear();
            let bin = &self.bins[ch.bin];
            if ch.aa.is_none() && ch.resampler.is_none() {
                out_buf.extend_from_slice(bin);
                ch.nco.mix(out_buf);
                continue;
            }
            ch.binbuf.clear();
            ch.binbuf.extend_from_slice(bin);
            ch.nco.mix(&mut ch.binbuf);
            let staged: &[IqSample] = match &mut ch.aa {
                Some(aa) if ch.resampler.is_none() => {
                    aa.process(&ch.binbuf, out_buf);
                    continue;
                }
                Some(aa) => {
                    ch.aabuf.clear();
                    aa.process(&ch.binbuf, &mut ch.aabuf);
                    &ch.aabuf
                }
                None => &ch.binbuf,
            };
            if let Some(rs) = &mut ch.resampler {
                rs.process(staged, out_buf);
            }
        }
    }
}

/// Center frequency of bin `i` relative to the capture center (FFT convention:
/// bins above `M/2` are negative frequencies).
fn bin_offset_hz(i: usize, m: usize, sample_rate: f64) -> f64 {
    let idx = if i <= m / 2 { i as f64 } else { i as f64 - m as f64 };
    idx * sample_rate / m as f64
}

/// Build a channel's anti-alias stage, if its bin needs one. Returns the stage
/// and the rate it hands to the resampler (`bin_rate` when there is none).
fn anti_alias(
    bin_rate: f64,
    output_rate: f64,
    passband_hz: f64,
    taps_per_transition: f64,
) -> Result<(Option<Fir>, f64), String> {
    // The bin carries content up to ±bin_rate/2, and resampling to
    // output_rate folds f onto f − output_rate; that lands inside ±passband
    // only for f > output_rate − passband, which exists only if this holds.
    if bin_rate <= 2.0 * (output_rate - passband_hz) {
        return Ok((None, bin_rate));
    }
    let decim = (bin_rate / output_rate).floor().max(1.0) as usize;
    let post_rate = bin_rate / decim as f64;
    // Anything below the stopband edge folds (about post_rate ≥ output_rate in
    // the decimation, then about output_rate in the resample) to at least
    // ±passband from DC, so only content beyond it must be suppressed.
    let stop_hz = output_rate - passband_hz;
    let transition = stop_hz - passband_hz;
    if transition <= 0.0 {
        return Err(format!(
            "output rate {output_rate} leaves no anti-alias transition for ±{passband_hz} Hz"
        ));
    }
    let ntaps = ((taps_per_transition * bin_rate / transition).ceil() as usize | 1).max(9);
    let cutoff = 0.5 * (passband_hz + stop_hz);
    Ok((Some(Fir::with_decimation(lowpass_taps(cutoff / bin_rate, ntaps), decim)), post_rate))
}

/// Pick the bin count `M` (= channelizer bin grid `fs/M`).
///
/// A critically-sampled PFB bin only passes flat near its center; a channel
/// sitting at the bin edge is scalloped down ~6 dB. Real VHF airband channels
/// are all on a 25 kHz raster, so the right `M` puts every requested channel
/// on (or very near) a bin center — zero residual, no scalloping. So we search
/// for the `M` whose grid `fs/M` best lands every offset on a bin center,
/// subject to the bin rate carrying the passband, staying ≥ the output rate
/// (the resampler only trims down), and staying ≤ `MAX_BIN_TO_OUTPUT_RATIO` ×
/// the output rate (bounded per-channel anti-alias decimation).
fn choose_num_bins(
    input_rate: f64,
    output_rate: f64,
    passband_hz: f64,
    offsets: &[f64],
) -> Result<usize, String> {
    // Bin rate must clear the channel (≥ ~2.4·passband so ±passband sits in
    // the flat center region) and not require upsampling.
    let min_bin_rate = (2.4 * passband_hz).max(output_rate);
    let max_m = (input_rate / min_bin_rate).floor() as usize;
    if max_m < 2 {
        return Err(format!(
            "capture {input_rate} S/s too narrow to channelize ±{passband_hz} Hz at {output_rate} S/s"
        ));
    }
    // Smallest M whose bin rate stays within MAX_BIN_TO_OUTPUT_RATIO.
    let min_m = ((input_rate / (MAX_BIN_TO_OUTPUT_RATIO * output_rate)).ceil() as usize).max(2);
    // Score each candidate M by the worst-case channel-to-bin-center distance
    // as a fraction of the bin (0 = every channel dead-center, best). Prefer
    // the largest M among the best-scoring (smallest FFT blocks per output
    // sample, gentlest final resample). Cap the search for a small FFT.
    let cap = max_m.min(256);
    if min_m > cap {
        return Err(format!(
            "no channelizer bin count keeps {input_rate} S/s bins within \
             {MAX_BIN_TO_OUTPUT_RATIO}× the {output_rate} S/s output rate"
        ));
    }
    let mut best_m = min_m;
    let mut best_score = f64::INFINITY;
    for m in min_m..=cap {
        let bin_rate = input_rate / m as f64;
        let worst = offsets
            .iter()
            .map(|&off| {
                let k = (off / bin_rate).round();
                ((off - k * bin_rate) / bin_rate).abs()
            })
            .fold(0.0f64, f64::max);
        // Tie-break toward larger M by subtracting a tiny m-proportional term.
        let score = worst - (m as f64) * 1e-9;
        if score < best_score {
            best_score = score;
            best_m = m;
        }
    }
    Ok(best_m)
}

/// Pick `(M, taps_per_branch)` for a 2× oversampled bank.
///
/// Bins are output at `2·S` (`S = fs/M`), so a channel at residual `r` with
/// ±`passband` only needs the prototype flat to `fp = max|r| + passband` and
/// down by `2·S − fp`, where content first folds onto the passband. The
/// prototype cutoff sits at `S`, the transition is `2·(S − fp)`, and its
/// length follows from that. Candidates are the even `M` whose bin rate is
/// within `[output, MAX_BIN_TO_OUTPUT_RATIO·output]` and whose rate after
/// the anti-alias decimation stays ≤ `max_resample_ratio`·output (see
/// [`MAX_RESAMPLE_RATIO`]); among them, take the shortest prototype, then the
/// largest `M` (lowest bin rate: cheapest per-channel finish).
fn choose_oversampled_bins(
    input_rate: f64,
    output_rate: f64,
    passband_hz: f64,
    offsets: &[f64],
    max_resample_ratio: f64,
) -> Result<(usize, usize), String> {
    let min_m =
        ((2.0 * input_rate / (MAX_BIN_TO_OUTPUT_RATIO * output_rate)).ceil() as usize).max(4);
    let max_m = ((2.0 * input_rate / output_rate).floor() as usize).min(256);
    let mut best: Option<(usize, usize)> = None;
    for m in (min_m..=max_m).filter(|m| m.is_multiple_of(2)) {
        let spacing = input_rate / m as f64;
        let worst = offsets
            .iter()
            .map(|&off| (off - (off / spacing).round() * spacing).abs())
            .fold(0.0f64, f64::max);
        let tw = 2.0 * (spacing - (worst + passband_hz));
        if tw <= 0.0 {
            continue;
        }
        let tpb = ((OVERSAMPLED_TAPS_PER_TRANSITION * spacing / tw).ceil() as usize)
            .max(OVERSAMPLED_MIN_TAPS);
        if tpb > OVERSAMPLED_MAX_TAPS {
            continue;
        }
        // Same decimation `anti_alias` would pick (none when it is skipped:
        // then the bin rate is below 2·output and floor() gives 1).
        let bin_rate = 2.0 * spacing;
        let post_rate = bin_rate / (bin_rate / output_rate).floor().max(1.0);
        if post_rate > max_resample_ratio * output_rate {
            continue;
        }
        if best.is_none_or(|(_, t)| tpb <= t) {
            best = Some((m, tpb));
        }
    }
    best.ok_or_else(|| {
        format!(
            "no oversampled bin count carries ±{passband_hz} Hz at {output_rate} S/s from \
             {input_rate} S/s"
        )
    })
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
    fn recovers_raster_channels() {
        // Real VHF airband channels are all on a 25 kHz raster; the chosen M
        // puts each on a bin center (zero scalloping). Offsets here are all
        // 25 kHz multiples (e.g. 131.550/131.725/131.825 around a 131.500
        // center → +50/+225/+325 kHz; and a negative one).
        let fs = 2_400_000.0;
        let out_rate = 24_000.0;
        let offsets = [50_000.0, 225_000.0, 325_000.0, -75_000.0];

        let n = 720_000;
        // One tone per channel, each 1 kHz inside its channel.
        let mut out: Vec<Vec<IqSample>> = vec![Vec::new(); offsets.len()];
        for (k, &off) in offsets.iter().enumerate() {
            let input = tone(off + 1_000.0, fs, n);
            // Decode each channel's tone in isolation to assert per-channel
            // gain without inter-tone leakage masking it.
            let mut cd = ChannelizedDdc::new(fs, out_rate, &offsets, 5_000.0).unwrap();
            let mut o: Vec<Vec<IqSample>> = vec![Vec::new(); offsets.len()];
            cd.process(&input, &mut o);
            out[k] = std::mem::take(&mut o[k]);
        }

        for (k, ch) in out.iter().enumerate() {
            let settled = &ch[ch.len() / 2..];
            let amp = settled.iter().map(|s| s.norm()).sum::<f32>() / settled.len() as f32;
            assert!((amp - 1.0).abs() < 0.15, "channel {k} gain {amp}, expected ~1");
        }
    }

    #[test]
    fn rejects_neighbor_channel() {
        let fs = 2_400_000.0;
        let out_rate = 24_000.0;
        let offsets = [0.0];
        let mut cd = ChannelizedDdc::new(fs, out_rate, &offsets, 5_000.0).unwrap();
        // A tone one full bin away should be rejected by the channelizer.
        let bin_rate = fs / cd.bins.len() as f64;
        let input = tone(bin_rate, fs, 480_000);
        let mut out = vec![Vec::new()];
        cd.process(&input, &mut out);
        let settled = &out[0][out[0].len() / 2..];
        let amp = settled.iter().map(|s| s.norm()).sum::<f32>() / settled.len() as f32;
        assert!(amp < 0.2, "neighbor bin should be rejected, got {amp}");
    }

    #[test]
    fn streams_in_blocks_seamlessly() {
        // The PFB buffers a partial block of `M` samples across calls
        // (`pending`), so feeding the capture in arbitrary block sizes must
        // yield the same output as one big block — and the same samples.
        let fs = 2_400_000.0;
        let out_rate = 24_000.0;
        let offsets = [50_000.0, -75_000.0];
        let input = tone(50_000.0 + 1_000.0, fs, 480_000);

        let mut whole = ChannelizedDdc::new(fs, out_rate, &offsets, 5_000.0).unwrap();
        let mut a: Vec<Vec<IqSample>> = vec![Vec::new(); offsets.len()];
        whole.process(&input, &mut a);

        // Odd, non-multiple-of-M chunk sizes exercise the pending buffer.
        let mut chunked = ChannelizedDdc::new(fs, out_rate, &offsets, 5_000.0).unwrap();
        let mut totals = vec![0usize; offsets.len()];
        let mut joined: Vec<Vec<IqSample>> = vec![Vec::new(); offsets.len()];
        let mut b: Vec<Vec<IqSample>> = vec![Vec::new(); offsets.len()];
        for blk in input.chunks(7919) {
            chunked.process(blk, &mut b);
            for (k, v) in b.iter().enumerate() {
                totals[k] += v.len();
                joined[k].extend_from_slice(v);
            }
        }

        for k in 0..offsets.len() {
            assert!(
                (a[k].len() as i64 - totals[k] as i64).abs() <= 4,
                "channel {k}: {} whole vs {} chunked",
                a[k].len(),
                totals[k]
            );
            // The streamed samples must match the one-shot ones (same filter
            // state, just fed in pieces).
            let n = a[k].len().min(joined[k].len());
            let worst = (0..n).map(|i| (a[k][i] - joined[k][i]).norm()).fold(0.0f32, f32::max);
            assert!(worst < 1e-4, "channel {k}: streamed output diverged by {worst}");
        }
    }

    /// DFT length for the in-band power measurement.
    const INBAND_N: usize = 4096;
    /// Output samples skipped before measuring (filter settling).
    const INBAND_SKIP: usize = 512;

    /// In-band power (dB) of `x[skip..skip + INBAND_N]`: Blackman-Harris-windowed
    /// DFT power summed over every bin within ±`passband` of DC. Normalized by
    /// the window energy (Parseval), so a unit-amplitude 0 Hz tone reads 0 dB.
    fn inband_db(x: &[IqSample], fs: f64, passband: f64, skip: usize) -> f64 {
        use num_complex::Complex;
        use rustfft::FftPlanner;
        assert!(x.len() >= skip + INBAND_N, "need {} samples, got {}", skip + INBAND_N, x.len());
        let win = crate::window::blackman_harris(INBAND_N);
        let mut buf: Vec<Complex<f64>> = x[skip..skip + INBAND_N]
            .iter()
            .zip(win.iter())
            .map(|(s, &w)| Complex::new(f64::from(s.re) * w, f64::from(s.im) * w))
            .collect();
        FftPlanner::<f64>::new().plan_fft_forward(INBAND_N).process(&mut buf);
        let inband: f64 = buf
            .iter()
            .enumerate()
            .filter(|(k, _)| {
                let idx = if *k <= INBAND_N / 2 { *k as f64 } else { *k as f64 - INBAND_N as f64 };
                (idx * fs / INBAND_N as f64).abs() <= passband
            })
            .map(|(_, c)| c.norm_sqr())
            .sum();
        let energy: f64 = INBAND_N as f64 * win.iter().map(|w| w * w).sum::<f64>();
        10.0 * (inband / energy).max(1e-30).log10()
    }

    /// Feed one tone at `offsets[ch] + rel_hz` through a fresh front end and
    /// return channel `ch`'s in-band power (dB).
    fn channel_inband_db(
        fs: f64,
        out_rate: f64,
        offsets: &[f64],
        passband: f64,
        ch: usize,
        rel_hz: f64,
    ) -> f64 {
        let mut cd = ChannelizedDdc::new(fs, out_rate, offsets, passband).unwrap();
        let n_in = ((INBAND_SKIP + INBAND_N + 64) as f64 * fs / out_rate).ceil() as usize;
        let input = tone(offsets[ch] + rel_hz, fs, n_in);
        let mut out: Vec<Vec<IqSample>> = vec![Vec::new(); offsets.len()];
        cd.process(&input, &mut out);
        inband_db(&out[ch], out_rate, passband, INBAND_SKIP)
    }

    /// Every tone in `rel_tones` (channel-relative Hz) must land ≤ −60 dB
    /// in-band relative to a 0 Hz tone through the same channel. Returns the
    /// failures as readable lines (empty = pass).
    fn alias_failures(
        fs: f64,
        out_rate: f64,
        offsets: &[f64],
        passband: f64,
        chans: &[usize],
        rel_tones: &[f64],
    ) -> Vec<String> {
        let mut bad = Vec::new();
        for &ch in chans {
            let reference = channel_inband_db(fs, out_rate, offsets, passband, ch, 0.0);
            for &rel in rel_tones {
                let rej = channel_inband_db(fs, out_rate, offsets, passband, ch, rel) - reference;
                eprintln!("ch {ch} (off {:+.0}) tone {rel:+.0} Hz: {rej:+.1} dB", offsets[ch]);
                if rej > -60.0 {
                    bad.push(format!("ch {ch} tone {rel:+.0} Hz in-band at {rej:+.1} dB"));
                }
            }
        }
        bad
    }

    const ADJ_TONES: [f64; 8] =
        [20_000.0, -20_000.0, 23_000.0, -23_000.0, 30_000.0, -30_000.0, 50_000.0, -50_000.0];

    #[test]
    fn us_plan_rejects_adjacent_channel_alias() {
        // The real 16-channel US ACARS plan around 131.1375 MHz (see the
        // acars `cpu_budget` example): 50 kHz bins, residuals ±12.5 kHz, so a
        // bin is twice the 24 kHz channel rate and an unfiltered resample
        // folds the neighbouring raster channels into the passband.
        const PLAN_MHZ: [f64; 16] = [
            130.425, 130.45, 130.55, 130.825, 130.85, 131.125, 131.25, 131.425, 131.45, 131.475,
            131.525, 131.55, 131.65, 131.725, 131.825, 131.85,
        ];
        let fs = 2_400_000.0;
        let offsets: Vec<f64> = PLAN_MHZ.iter().map(|f| f * 1e6 - 131_137_500.0).collect();
        let cd = ChannelizedDdc::new(fs, 24_000.0, &offsets, 5_000.0).unwrap();
        // Channel 0 sits below its bin center and channel 1 above, so between
        // them the bin's wide side is exercised in both directions.
        let m = cd.bins.len();
        let res = |ch: usize| offsets[ch] - bin_offset_hz(cd.channels[ch].bin, m, fs);
        assert!(res(0) < 0.0 && res(1) > 0.0, "residuals {} / {}", res(0), res(1));
        let bad = alias_failures(fs, 24_000.0, &offsets, 5_000.0, &[0, 1], &ADJ_TONES);
        assert!(bad.is_empty(), "adjacent-channel aliases in-band:\n{}", bad.join("\n"));
    }

    #[test]
    fn off_raster_center_rejects_alias() {
        // The 8-channel bench plan with the capture centered 12.5 kHz off the
        // raster: every channel lands ±12.5 kHz from a 50 kHz bin center, the
        // same wide-bin geometry as the US plan.
        const PLAN_MHZ: [f64; 8] =
            [131.425, 131.450, 131.475, 131.525, 131.550, 131.575, 131.725, 131.825];
        let fs = 2_400_000.0;
        let offsets: Vec<f64> = PLAN_MHZ.iter().map(|f| f * 1e6 - 131_512_500.0).collect();
        let all: Vec<usize> = (0..offsets.len()).collect();
        let bad = alias_failures(fs, 24_000.0, &offsets, 5_000.0, &all, &ADJ_TONES);
        assert!(bad.is_empty(), "off-raster aliases in-band:\n{}", bad.join("\n"));
    }

    #[test]
    fn bin_edge_skirt_25khz_bins() {
        // The 8-channel bench plan centered on-raster at 131.500 MHz: every
        // offset is a 25 kHz multiple, so M = 96 (25 kHz bins) puts each
        // channel dead on a bin center. ACARS rates (24 kHz out, ±5 kHz).
        // A bin is critically sampled, so a tone at Δ lands at Δ mod 25 kHz
        // inside it: ±20 kHz folds to ∓5 kHz, the passband edge.
        const PLAN_MHZ: [f64; 8] =
            [131.425, 131.450, 131.475, 131.525, 131.550, 131.575, 131.725, 131.825];
        const RELS: [f64; 15] = [
            0.0, 10e3, -10e3, 12.5e3, -12.5e3, 15e3, -15e3, 20e3, -20e3, 25e3, -25e3, 30e3, -30e3,
            37.5e3, -37.5e3,
        ];
        let (fs, out_rate, pb, ch) = (2_400_000.0, 24_000.0, 5_000.0, 3);
        let offsets: Vec<f64> = PLAN_MHZ.iter().map(|f| f * 1e6 - 131_500_000.0).collect();
        let n_in = ((INBAND_SKIP + INBAND_N + 64) as f64 * fs / out_rate).ceil() as usize;
        // (in-band dB, total dB) of channel `ch` for a tone at center + rel,
        // with the channel moved `res` Hz off its bin center (the residual NCO
        // the constructor would build; the constructor itself would re-snap M).
        let measure = |res: f64, rel: f64| {
            let mut cd = ChannelizedDdc::new(fs, out_rate, &offsets, pb).unwrap();
            let bin_rate = fs / cd.bins.len() as f64;
            assert_eq!(bin_rate, 25_000.0);
            assert_eq!(bin_offset_hz(cd.channels[ch].bin, cd.bins.len(), fs), offsets[ch]);
            cd.channels[ch].nco = Nco::new(res, bin_rate);
            let mut out: Vec<Vec<IqSample>> = vec![Vec::new(); offsets.len()];
            cd.process(&tone(offsets[ch] + res + rel, fs, n_in), &mut out);
            let s = &out[ch][INBAND_SKIP..INBAND_SKIP + INBAND_N];
            let total = s.iter().map(|x| f64::from(x.norm_sqr())).sum::<f64>() / s.len() as f64;
            (inband_db(&out[ch], out_rate, pb, INBAND_SKIP), 10.0 * total.max(1e-30).log10())
        };
        // (residual, [(tone Δ, in-band regression floor dB)]): measured − ~3 dB.
        // Off center, −20 kHz sits 0.6 bin from the bin center (15 kHz) and
        // folds onto the channel's passband edge — the prototype's own skirt.
        let on = [(20e3, -130.0), (-20e3, -130.0), (25e3, -109.0), (-25e3, -109.0)];
        let off = [(20e3, -112.0), (-20e3, -22.0), (25e3, -117.0), (-25e3, -128.0)];
        for (res, floors) in [(0.0, on), (5e3, off)] {
            let (ref_in, ref_tot) = measure(res, 0.0);
            eprintln!("channel {:+.0} Hz from bin center:", res);
            eprintln!("  tone Δ     in-band(±5k)   total");
            for &rel in &RELS {
                let (inb, tot) = measure(res, rel);
                let (inb, tot) = (inb - ref_in, tot - ref_tot);
                eprintln!("  {:+8.1}k  {inb:+9.1} dB  {tot:+7.1} dB", rel / 1e3);
                for &(at, floor) in &floors {
                    if rel == at {
                        assert!(inb <= floor, "res {res} tone {rel}: {inb:+.1} dB > {floor} dB");
                    }
                }
            }
        }
    }

    /// (in-band dB, total dB) of channel `ch` of `cd` for a unit tone at
    /// `offsets[ch] + rel`.
    fn tone_power_db(mut cd: ChannelizedDdc, offsets: &[f64], ch: usize, rel: f64) -> (f64, f64) {
        let (fs, out_rate, pb) = (2_400_000.0, 24_000.0, 5_000.0);
        let n_in = ((INBAND_SKIP + INBAND_N + 64) as f64 * fs / out_rate).ceil() as usize;
        let mut out: Vec<Vec<IqSample>> = vec![Vec::new(); offsets.len()];
        cd.process(&tone(offsets[ch] + rel, fs, n_in), &mut out);
        let s = &out[ch][INBAND_SKIP..INBAND_SKIP + INBAND_N];
        let total = s.iter().map(|x| f64::from(x.norm_sqr())).sum::<f64>() / s.len() as f64;
        (inband_db(&out[ch], out_rate, pb, INBAND_SKIP), 10.0 * total.max(1e-30).log10())
    }

    #[test]
    fn oversampled_bin_edge_skirt() {
        // The `bin_edge_skirt_25khz_bins` geometry (8-channel plan, ACARS
        // rates) with the capture centered 0/5/10/12.5 kHz off the raster,
        // through the 2× bank: wherever the channels land relative to the
        // bin grid, tones ≥ 20 kHz away must stay out of the passband.
        const PLAN_MHZ: [f64; 8] =
            [131.425, 131.450, 131.475, 131.525, 131.550, 131.575, 131.725, 131.825];
        const RELS: [f64; 15] = [
            0.0, 10e3, -10e3, 12.5e3, -12.5e3, 15e3, -15e3, 20e3, -20e3, 25e3, -25e3, 30e3, -30e3,
            37.5e3, -37.5e3,
        ];
        let (fs, out_rate, pb, ch) = (2_400_000.0, 24_000.0, 5_000.0, 3);
        let mut worst_adj = f64::NEG_INFINITY;
        for shift in [0.0, 5e3, 10e3, 12.5e3] {
            let offsets: Vec<f64> =
                PLAN_MHZ.iter().map(|f| f * 1e6 - (131_500_000.0 + shift)).collect();
            let build = || {
                ChannelizedDdc::with_oversample(fs, out_rate, &offsets, pb, Oversample::Twice)
                    .unwrap()
            };
            let probe = build();
            let res = offsets[ch] - bin_offset_hz(probe.channels[ch].bin, probe.bins.len(), fs);
            eprintln!("shift {shift:+.0}: {} residual {res:+.0}", probe.describe());
            let (ref_in, ref_tot) = tone_power_db(build(), &offsets, ch, 0.0);
            assert!(ref_in.abs() < 0.1, "shift {shift}: in-channel tone at {ref_in:+.2} dB");
            eprintln!("  tone Δ     in-band(±5k)   total");
            for &rel in &RELS {
                let (inb, tot) = tone_power_db(build(), &offsets, ch, rel);
                let (inb, tot) = (inb - ref_in, tot - ref_tot);
                eprintln!("  {:+8.1}k  {inb:+9.1} dB  {tot:+7.1} dB", rel / 1e3);
                if rel.abs() >= 20e3 {
                    worst_adj = worst_adj.max(inb);
                }
            }
        }
        // Regression floor ~3 dB under the measured worst case.
        eprintln!("worst ≥20 kHz tone in-band: {worst_adj:+.1} dB");
        assert!(worst_adj < -104.0, "worst ≥20 kHz tone in-band at {worst_adj:+.1} dB");
    }

    #[test]
    fn oversampled_resample_guard() {
        // On this plan, without the guard, the chooser's shortest prototype
        // (6 taps) comes with M = 74: 64.9 kS/s bins → anti-alias decimation
        // by 2 → 32.4 kS/s, 1.35× the output rate, into the cubic resampler.
        let (fs, out_rate, pb) = (2_400_000.0, 24_000.0, 5_000.0);
        let offsets = [-100_000.0, 37_500.0, 162_500.0];
        let ratio = |m: usize| {
            let bin_rate = 2.0 * fs / m as f64;
            bin_rate / (bin_rate / out_rate).floor().max(1.0) / out_rate
        };
        let (m_free, _) =
            choose_oversampled_bins(fs, out_rate, pb, &offsets, f64::INFINITY).unwrap();
        assert!(ratio(m_free) > MAX_RESAMPLE_RATIO, "unguarded M={m_free} ratio {}", ratio(m_free));
        let guarded =
            ChannelizedDdc::with_oversample(fs, out_rate, &offsets, pb, Oversample::Twice).unwrap();
        let m = guarded.bins.len();
        assert!(ratio(m) <= MAX_RESAMPLE_RATIO, "guarded M={m} ratio {}", ratio(m));
        eprintln!("unguarded M={m_free} ({:.3}×), guarded {}", ratio(m_free), guarded.describe());

        let tones = [12.5e3, -12.5e3, 25e3, -25e3];
        let worst = |guard: f64| {
            let build = || {
                ChannelizedDdc::build(fs, out_rate, &offsets, pb, Oversample::Twice, guard).unwrap()
            };
            let mut worst = f64::NEG_INFINITY;
            for ch in 0..offsets.len() {
                let (ref_in, _) = tone_power_db(build(), &offsets, ch, 0.0);
                for &rel in &tones {
                    let rej = tone_power_db(build(), &offsets, ch, rel).0 - ref_in;
                    eprintln!("  guard {guard}: ch {ch} tone {rel:+.0} Hz: {rej:+.1} dB");
                    worst = worst.max(rej);
                }
            }
            worst
        };
        let (free, safe) = (worst(f64::INFINITY), worst(MAX_RESAMPLE_RATIO));
        eprintln!("worst in-band: unguarded {free:+.1} dB, guarded {safe:+.1} dB");
        assert!(safe < -60.0, "guarded: tone in-band at {safe:+.1} dB");
        // The fold the guard exists for (measured ≈ −20 dB).
        assert!(free > -60.0, "unguarded config no longer folds ({free:+.1} dB): pick a new plan");
    }

    #[test]
    fn oversampled_recovers_off_raster_channels() {
        // Offsets nowhere near a common raster: the 2× bank serves each flat
        // wherever it falls in its bin.
        let (fs, out_rate) = (2_400_000.0, 24_000.0);
        let offsets = [41_300.0, -118_900.0, 227_000.0, 3_100.0];
        for (k, &off) in offsets.iter().enumerate() {
            let mut cd =
                ChannelizedDdc::with_oversample(fs, out_rate, &offsets, 5_000.0, Oversample::Twice)
                    .unwrap();
            let mut o: Vec<Vec<IqSample>> = vec![Vec::new(); offsets.len()];
            cd.process(&tone(off + 1_000.0, fs, 480_000), &mut o);
            let settled = &o[k][o[k].len() / 2..];
            let amp = settled.iter().map(|s| s.norm()).sum::<f32>() / settled.len() as f32;
            assert!((amp - 1.0).abs() < 0.01, "channel {k} gain {amp}, expected ~1");
        }
    }

    #[test]
    fn oversampled_streams_in_blocks_seamlessly() {
        // The bank itself is chunk-invariant bit for bit (see the channelizer
        // tests); the residual NCO's per-block phase handling is not, so this
        // holds the output to the same 1e-4 as `streams_in_blocks_seamlessly`.
        let fs = 2_400_000.0;
        let offsets = [41_300.0, -75_000.0, 162_500.0];
        let input = tone(41_300.0 + 1_000.0, fs, 240_000);
        let build = || {
            ChannelizedDdc::with_oversample(fs, 24_000.0, &offsets, 5_000.0, Oversample::Twice)
                .unwrap()
        };
        let mut whole = build();
        let mut a: Vec<Vec<IqSample>> = vec![Vec::new(); offsets.len()];
        whole.process(&input, &mut a);
        let mut chunked = build();
        let mut joined: Vec<Vec<IqSample>> = vec![Vec::new(); offsets.len()];
        let mut b: Vec<Vec<IqSample>> = vec![Vec::new(); offsets.len()];
        for blk in input.chunks(7919) {
            chunked.process(blk, &mut b);
            for (j, v) in joined.iter_mut().zip(&b) {
                j.extend_from_slice(v);
            }
        }
        for k in 0..offsets.len() {
            assert_eq!(a[k].len(), joined[k].len(), "channel {k} len");
            let worst =
                a[k].iter().zip(&joined[k]).map(|(x, y)| (x - y).norm()).fold(0.0, f32::max);
            assert!(worst < 1e-4, "channel {k}: streamed output diverged by {worst}");
        }
    }

    #[test]
    fn bin_rate_ratio_is_bounded() {
        // AIS-like: two channels ±25 kHz, 48 kHz out, ±8 kHz passband. With
        // no bound on bin rate vs output rate the bin search picks 1.2 MHz
        // bins and everything within them folds into the channel. Either the
        // channelizer refuses (caller falls back to SharedDdc) or it must
        // reject far tones like any front end.
        let fs = 2_400_000.0;
        let offsets = [25_000.0, -25_000.0];
        if ChannelizedDdc::new(fs, 48_000.0, &offsets, 8_000.0).is_err() {
            return;
        }
        let tones = [100_000.0, -100_000.0, 200_000.0, -200_000.0];
        let bad = alias_failures(fs, 48_000.0, &offsets, 8_000.0, &[0, 1], &tones);
        assert!(bad.is_empty(), "far tones in-band:\n{}", bad.join("\n"));
    }

    #[test]
    fn output_rate_is_correct() {
        let fs = 2_400_000.0;
        let out_rate = 24_000.0;
        let mut cd = ChannelizedDdc::new(fs, out_rate, &[0.0], 5_000.0).unwrap();
        let input = vec![IqSample::new(0.0, 0.0); 2_400_000];
        let mut out = vec![Vec::new()];
        cd.process(&input, &mut out);
        // ~1 s of capture → ~24000 output samples (within edge effects).
        assert!((out[0].len() as i64 - 24_000).abs() <= 50, "got {}", out[0].len());
    }
}
