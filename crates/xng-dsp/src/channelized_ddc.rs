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
//!  4. A resampler corrects the bin rate (`fs / M`) to the exact channel rate.
//!
//! `M` is chosen so each bin comfortably carries the channel passband and the
//! bin rate is a small integer-ish multiple of the output rate (cheap final
//! resample).

use crate::channelizer::PfbChannelizer;
use crate::nco::Nco;
use crate::resample::Resampler;
use crate::IqSample;

/// One requested channel's mapping onto the bin grid + its cheap back end.
struct Channel {
    /// Which channelizer bin carries this channel.
    bin: usize,
    /// Residual NCO removing (channel center − bin center) at the bin rate.
    nco: Nco,
    resampler: Option<Resampler>,
    /// The selected bin's samples for this block (NCO-mixed in place).
    binbuf: Vec<IqSample>,
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
        let pfb = PfbChannelizer::new(m, 12);

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
            let resampler = if (bin_rate - output_rate).abs() > 1e-6 {
                Some(Resampler::new(bin_rate, output_rate))
            } else {
                None
            };
            channels.push(Channel {
                bin,
                nco: Nco::new(residual, bin_rate),
                resampler,
                binbuf: Vec::new(),
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

        // --- Per channel: pull its bin, residual-mix, resample. ---
        for (ch, out_buf) in self.channels.iter_mut().zip(out.iter_mut()) {
            out_buf.clear();
            ch.binbuf.clear();
            ch.binbuf.extend_from_slice(&self.bins[ch.bin]);
            ch.nco.mix(&mut ch.binbuf);
            match &mut ch.resampler {
                Some(rs) => rs.process(&ch.binbuf, out_buf),
                None => out_buf.extend_from_slice(&ch.binbuf),
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

/// Pick the bin count `M` (= channelizer bin grid `fs/M`).
///
/// A critically-sampled PFB bin only passes flat near its center; a channel
/// sitting at the bin edge is scalloped down ~6 dB. Real VHF airband channels
/// are all on a 25 kHz raster, so the right `M` puts every requested channel
/// on (or very near) a bin center — zero residual, no scalloping. So we search
/// for the `M` whose grid `fs/M` best lands every offset on a bin center,
/// subject to the bin rate carrying the passband and staying ≥ the output
/// rate (the resampler only trims down).
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
    // Score each candidate M by the worst-case channel-to-bin-center distance
    // as a fraction of the bin (0 = every channel dead-center, best). Prefer
    // the largest M among the best-scoring (smallest FFT blocks per output
    // sample, gentlest final resample). Cap the search for a small FFT.
    let cap = max_m.min(256);
    let mut best_m = 2usize;
    let mut best_score = f64::INFINITY;
    for m in 2..=cap {
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
