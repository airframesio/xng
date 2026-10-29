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

use crate::channelizer::PfbChannelizer;
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
    /// All `M` bin outputs (only the selected bins are consumed downstream,
    /// but the polyphase step fills every bin — that is the shared work).
    bins: Vec<Vec<IqSample>>,
    channels: Vec<Channel>,
}

impl ChannelizedDdc {
    /// Same signature as [`SharedDdc::new`](crate::SharedDdc::new).
    pub fn new(
        input_rate: f64,
        output_rate: f64,
        offsets: &[f64],
        passband_hz: f64,
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
        let m = choose_num_bins(input_rate, output_rate, passband_hz, offsets)?;
        let bin_rate = input_rate / m as f64;
        // Rejection at the neighbouring bin's *center* (~109 dB at 8 taps) is
        // not the figure that matters: the bank is critically sampled, so
        // everything ½–1½ bins from a bin center folds into that bin. A
        // channel off its bin center (±12.5 kHz in a 50 kHz bin on the real US
        // ACARS plan) gets that fold on top of its own spectrum, where no
        // downstream filter can remove it. Prototype response at 0.75 bin:
        // 8 taps −30 dB, 12 taps −57 dB, 16 taps −111 dB.
        let pfb = PfbChannelizer::new(m, PFB_TAPS_PER_BRANCH);

        let mut channels = Vec::with_capacity(offsets.len());
        for &off in offsets {
            // Snap to the nearest bin: bin index k maps to k·fs/M, with
            // k > M/2 wrapping to negative frequencies (FFT bin convention).
            let k_round = (off / bin_rate).round();
            let bin = k_round.rem_euclid(m as f64) as usize;
            let bin_center = bin_offset_hz(bin, m, input_rate);
            let residual = off - bin_center;
            // The channel must fit inside the selected bin: not just its center
            // within ±½ bin, but its whole ±passband. A channel whose passband
            // reaches past the bin edge would be silently attenuated, so reject
            // it here and let the caller fall back to SharedDdc. `choose_num_bins`
            // sizes the bins (≥ 2.4·passband) so the real airband raster passes;
            // this guards pathological rate/offset sets.
            let max_safe_residual = bin_rate / 2.0 - passband_hz;
            if residual.abs() > max_safe_residual {
                return Err(format!(
                    "channel offset {off} Hz is too close to a {m}-bin channelizer edge"
                ));
            }
            let (aa, post_rate) = anti_alias(bin_rate, output_rate, passband_hz)?;
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

        Ok(Self {
            pfb,
            bins: vec![Vec::new(); m],
            channels,
        })
    }

    /// Number of channels.
    pub fn num_channels(&self) -> usize {
        self.channels.len()
    }

    /// Downconvert a wideband block into all channels (same contract as
    /// [`SharedDdc::process`](crate::SharedDdc::process)).
    pub fn process(&mut self, input: &[IqSample], out: &mut [Vec<IqSample>]) {
        assert_eq!(out.len(), self.channels.len(), "out must have one vec per channel");

        // --- Shared polyphase + FFT: fills every bin once. ---
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
    let ntaps = ((TAPS_PER_TRANSITION * bin_rate / transition).ceil() as usize | 1).max(9);
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
