//! Shared multi-channel digital downconverter.
//!
//! Extracts several narrowband channels that all share one capture, one
//! output rate, and one passband (e.g. every VHF ACARS channel is 24 kHz /
//! ±5 kHz) — but unlike running an independent [`Ddc`](crate::Ddc) per
//! channel, the expensive *full-rate* anti-alias decimation is run **once**
//! over the wideband stream and its low-rate output is shared by every
//! channel's cheap final extraction. With N channels the per-channel [`Ddc`]
//! does N full-rate convolutions per block; this does one, then N cheap
//! convolutions at the (much lower) intermediate rate.
//!
//! Chain:
//! ```text
//!   wideband @ fs ── coarse decimating FIR (shared, run once) ──► inter @ fi
//!   inter @ fi ──┬─ NCO(−off_0) ─ sharp FIR ─ resample ─► channel 0 @ fo
//!                ├─ NCO(−off_1) ─ sharp FIR ─ resample ─► channel 1 @ fo
//!                └─ …
//! ```
//!
//! The coarse stage must preserve every channel's band, so it keeps the full
//! span `max|offset| + passband` and decimates only far enough to stay safely
//! above twice that span (no channel may alias). The per-channel sharp stage
//! then realizes the narrow `passband` selectivity at the low rate.

use crate::fir::{lowpass_taps, Fir};
use crate::nco::Nco;
use crate::resample::Resampler;
use crate::IqSample;

/// Same windowed-sinc transition sizing as [`Ddc`](crate::Ddc).
const TAPS_PER_TRANSITION: f64 = 5.5;
const MAX_TAPS: usize = 8192;

/// One channel's cheap back end, operating at the shared intermediate rate.
struct Channel {
    nco: Nco,
    fine: Fir,
    resampler: Option<Resampler>,
    /// NCO-mixed copy of the (shared) intermediate stream.
    mixed: Vec<IqSample>,
    /// Sharp-FIR output, fed to the resampler when present.
    decimated: Vec<IqSample>,
}

pub struct SharedDdc {
    /// Shared coarse stage(s): one decimating FIR over the full-rate stream.
    coarse: Vec<Fir>,
    /// Reusable buffers for the shared coarse output (and the inter-stage
    /// buffer when the coarse stage is split in two).
    inter: Vec<IqSample>,
    coarse_inter: Vec<IqSample>,
    channels: Vec<Channel>,
}

impl SharedDdc {
    /// * `input_rate` — wideband capture rate.
    /// * `output_rate` — per-channel rate (same for all channels).
    /// * `offsets` — each channel's center relative to the capture center.
    /// * `passband_hz` — one-sided width of each channel's signal.
    pub fn new(
        input_rate: f64,
        output_rate: f64,
        offsets: &[f64],
        passband_hz: f64,
    ) -> Result<Self, String> {
        if offsets.is_empty() {
            return Err("SharedDdc needs at least one channel".to_string());
        }
        if output_rate < 2.0 * passband_hz {
            return Err(format!(
                "output rate {output_rate} cannot carry a ±{passband_hz} Hz passband"
            ));
        }
        // The coarse stage must pass every channel's band FLAT — the outermost
        // channel edge is `max_off + passband`. A windowed-sinc lowpass rolls
        // off before its cutoff, so the cutoff is placed beyond that edge with
        // margin; the per-channel sharp stage provides the real selectivity.
        let max_off = offsets.iter().fold(0.0f64, |m, &o| m.max(o.abs()));
        let edge = max_off + passband_hz;
        if input_rate < 2.0 * edge {
            return Err(format!(
                "capture {input_rate} S/s too narrow for channels spanning ±{edge} Hz"
            ));
        }

        // Pick the coarse decimation: keep the intermediate rate above 2·edge
        // so (a) no channel aliases and (b) a little room remains above the
        // flat passband for the coarse filter's transition band. A 1.2 margin
        // (inter ≥ 2.4·edge) is enough — the per-channel sharp stage provides
        // the real selectivity, so the coarse transition can be wide/cheap.
        // The bigger the decimation here, the cheaper every channel's finish.
        // Never decimate below the output rate (the sharp stage does the rest).
        let max_decim = (input_rate / (2.4 * edge)).floor().max(1.0);
        let mut coarse_decim = (input_rate / output_rate).floor().min(max_decim) as usize;
        coarse_decim = coarse_decim.max(1);
        let inter_rate = input_rate / coarse_decim as f64;

        // Coarse cutoff: halfway between the outermost channel edge and the
        // Nyquist fold of the intermediate rate, so `edge` sits inside the
        // flat passband and aliasing is still rejected by the fold.
        let coarse_cutoff = 0.5 * (edge + inter_rate / 2.0);

        // Build the coarse anti-alias stage(s) — split into two when the
        // decimation is large (cheap coarse + sharp final), mirroring `Ddc`.
        let coarse = build_decimation_stages(input_rate, coarse_decim, coarse_cutoff, edge)?;

        // Each channel: mix to baseband at the intermediate rate, sharp-filter
        // to the narrow passband, then resample any leftover fraction so the
        // output lands exactly on `output_rate`.
        let mut channels = Vec::with_capacity(offsets.len());
        for &off in offsets {
            let fine_decim = (inter_rate / output_rate).floor().max(1.0) as usize;
            let post_rate = inter_rate / fine_decim as f64;
            // Sharp lowpass realizing the narrow passband at the inter rate.
            let trans = post_rate - 2.0 * passband_hz;
            if trans <= 0.0 {
                return Err(format!(
                    "intermediate rate {inter_rate} too low for ±{passband_hz} Hz channel"
                ));
            }
            let aa = ((TAPS_PER_TRANSITION * inter_rate / trans).ceil() as usize | 1).max(9);
            let sharp =
                ((TAPS_PER_TRANSITION * inter_rate / passband_hz).ceil() as usize | 1).max(9);
            let ntaps = aa.max(sharp);
            if ntaps > MAX_TAPS {
                return Err(format!(
                    "channel finish needs {ntaps} taps from {inter_rate} S/s; \
                     choose a friendlier sample rate"
                ));
            }
            let fine = Fir::with_decimation(
                lowpass_taps(passband_hz / inter_rate, ntaps),
                fine_decim,
            );
            let resampler = if (post_rate - output_rate).abs() > 1e-6 {
                Some(Resampler::new(post_rate, output_rate))
            } else {
                None
            };
            channels.push(Channel {
                nco: Nco::new(off, inter_rate),
                fine,
                resampler,
                mixed: Vec::new(),
                decimated: Vec::new(),
            });
        }

        Ok(Self {
            coarse,
            inter: Vec::new(),
            coarse_inter: Vec::new(),
            channels,
        })
    }

    /// Number of channels.
    pub fn num_channels(&self) -> usize {
        self.channels.len()
    }

    /// Downconvert a wideband block into all channels. `out` must hold one
    /// vector per channel; each is cleared and filled with that channel's
    /// output samples.
    pub fn process(&mut self, input: &[IqSample], out: &mut [Vec<IqSample>]) {
        assert_eq!(out.len(), self.channels.len(), "out must have one vec per channel");

        // --- Shared coarse decimation, run ONCE over the full-rate stream. ---
        self.inter.clear();
        match self.coarse.len() {
            1 => self.coarse[0].process(input, &mut self.inter),
            _ => {
                self.coarse_inter.clear();
                let (first, rest) = self.coarse.split_at_mut(1);
                first[0].process(input, &mut self.coarse_inter);
                rest[0].process(&self.coarse_inter, &mut self.inter);
            }
        }

        // --- Cheap per-channel finish at the intermediate rate. ---
        for (ch, out_buf) in self.channels.iter_mut().zip(out.iter_mut()) {
            out_buf.clear();
            ch.mixed.clear();
            ch.mixed.extend_from_slice(&self.inter);
            ch.nco.mix(&mut ch.mixed);
            if ch.resampler.is_some() {
                ch.decimated.clear();
                ch.fine.process(&ch.mixed, &mut ch.decimated);
                if let Some(rs) = &mut ch.resampler {
                    rs.process(&ch.decimated, out_buf);
                }
            } else {
                ch.fine.process(&ch.mixed, out_buf);
            }
        }
    }
}

/// Build the shared coarse decimating lowpass, split into a cheap coarse stage
/// and a sharp final stage when the decimation is large (mirrors `Ddc::new`).
///
/// * `cutoff_hz` — the lowpass cutoff (placed beyond the outermost channel
///   edge so every channel is in the flat passband).
/// * `protect_hz` — the band that must NOT be aliased into (the channel edge);
///   the stop band starts where energy would fold back below it.
fn build_decimation_stages(
    input_rate: f64,
    decim: usize,
    cutoff_hz: f64,
    protect_hz: f64,
) -> Result<Vec<Fir>, String> {
    let factors: Vec<usize> = if decim <= 16 {
        vec![decim]
    } else {
        match (2..=16).rev().find(|d| decim % d == 0) {
            Some(d2) => vec![decim / d2, d2],
            None => vec![decim],
        }
    };

    let mut stages = Vec::new();
    let mut rate = input_rate;
    for &d in &factors {
        let out = rate / d as f64;
        // Aliases of the kept band fold around `out`; keep them out of the
        // protected band: transition spans cutoff → (out − protect).
        let transition = (out - protect_hz) - cutoff_hz;
        if transition <= 0.0 {
            return Err(format!(
                "stage output rate {out} too low for a {cutoff_hz} Hz cutoff \
                 protecting ±{protect_hz} Hz"
            ));
        }
        let ntaps = ((TAPS_PER_TRANSITION * rate / transition).ceil() as usize | 1).max(9);
        if ntaps > MAX_TAPS {
            return Err(format!(
                "decimation {decim} from {input_rate} S/s needs {ntaps} taps; \
                 choose a friendlier sample rate"
            ));
        }
        stages.push(Fir::with_decimation(lowpass_taps(cutoff_hz / rate, ntaps), d));
        rate = out;
    }
    Ok(stages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddc::Ddc;
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
    fn each_channel_recovers_its_own_tone() {
        let fs = 2_400_000.0;
        let out_rate = 24_000.0;
        let offsets = [50_000.0, -75_000.0, 150_000.0];
        let mut sd = SharedDdc::new(fs, out_rate, &offsets, 5_000.0).unwrap();

        // One in-channel tone per channel (offset + 1 kHz), summed.
        let n = 480_000;
        let mut input = vec![IqSample::new(0.0, 0.0); n];
        for &off in &offsets {
            for (i, s) in tone(off + 1_000.0, fs, n).into_iter().enumerate() {
                input[i] += s;
            }
        }

        let mut out: Vec<Vec<IqSample>> = vec![Vec::new(); offsets.len()];
        sd.process(&input, &mut out);

        for (k, ch) in out.iter().enumerate() {
            let settled = &ch[ch.len() / 2..];
            let amp = settled.iter().map(|s| s.norm()).sum::<f32>() / settled.len() as f32;
            // Each channel sees its own 1 kHz tone near unit gain; the other
            // channels' tones (≥50 kHz away) are rejected by the sharp stage.
            assert!((amp - 1.0).abs() < 0.1, "channel {k} gain {amp}, expected ~1");
        }
    }

    #[test]
    fn rejects_adjacent_channel() {
        let fs = 2_400_000.0;
        let out_rate = 24_000.0;
        let offsets = [0.0];
        let mut sd = SharedDdc::new(fs, out_rate, &offsets, 5_000.0).unwrap();
        // A tone 25 kHz away from the only channel must be strongly rejected.
        let input = tone(25_000.0, fs, 480_000);
        let mut out = vec![Vec::new()];
        sd.process(&input, &mut out);
        let settled = &out[0][out[0].len() / 2..];
        let amp = settled.iter().map(|s| s.norm()).sum::<f32>() / settled.len() as f32;
        assert!(amp < 0.05, "adjacent channel should be rejected, got {amp}");
    }

    #[test]
    fn matches_per_channel_ddc_closely() {
        // The shared front end must produce essentially the same channel
        // stream as an independent Ddc — same demod sees the same samples.
        let fs = 2_400_000.0;
        let out_rate = 24_000.0;
        let off = 50_000.0;
        let input = tone(off + 800.0, fs, 240_000);

        let mut sd = SharedDdc::new(fs, out_rate, &[off], 5_000.0).unwrap();
        let mut shared = vec![Vec::new()];
        sd.process(&input, &mut shared);

        let mut ddc = Ddc::new(fs, out_rate, off, 5_000.0).unwrap();
        let mut single = Vec::new();
        ddc.process(&input, &mut single);

        // Compare the settled tail amplitude/frequency (group delays differ,
        // so compare statistics, not sample-for-sample).
        let stat = |v: &[IqSample]| {
            let s = &v[v.len() / 2..];
            let amp = s.iter().map(|x| x.norm()).sum::<f32>() / s.len() as f32;
            let mid = s.len() / 2;
            let dphi = (s[mid + 1] * s[mid].conj()).arg();
            (amp, dphi)
        };
        let (a0, p0) = stat(&shared[0]);
        let (a1, p1) = stat(&single);
        assert!((a0 - a1).abs() < 0.05, "amplitude differs: {a0} vs {a1}");
        assert!((p0 - p1).abs() < 1e-2, "tone freq differs: {p0} vs {p1}");
    }

    #[test]
    fn streams_in_blocks_seamlessly() {
        // Feeding the capture in arbitrary block sizes must give the same
        // output count as one big block (state carries across calls).
        let fs = 2_400_000.0;
        let out_rate = 24_000.0;
        let input = tone(1_000.0, fs, 240_000);

        let mut whole = SharedDdc::new(fs, out_rate, &[0.0], 5_000.0).unwrap();
        let mut a = vec![Vec::new()];
        whole.process(&input, &mut a);

        let mut chunked = SharedDdc::new(fs, out_rate, &[0.0], 5_000.0).unwrap();
        let mut total = 0usize;
        let mut b = vec![Vec::new()];
        for blk in input.chunks(7919) {
            chunked.process(blk, &mut b);
            total += b[0].len();
        }
        // Within a couple of samples (decimation edge effects across blocks).
        assert!((a[0].len() as i64 - total as i64).abs() <= 4, "{} vs {total}", a[0].len());
    }
}
