//! Digital downconverter: extract one narrowband channel from a wideband
//! capture (NCO mix to baseband + decimating anti-alias FIR, split into two
//! stages when the decimation factor is large).

use crate::fir::{lowpass_taps, Fir};
use crate::nco::Nco;
use crate::resample::Resampler;
use crate::IqSample;

/// Required stopband attenuation drives taps ≈ 5.5 / normalized transition
/// width for the 4-term Blackman-Harris design.
const TAPS_PER_TRANSITION: f64 = 5.5;
const MAX_TAPS: usize = 8192;

pub struct Ddc {
    nco: Nco,
    stages: Vec<Fir>,
    /// Present when the capture rate is not an integer multiple of the output
    /// rate: the FIR stages decimate to just above the output rate and this
    /// corrects the leftover fraction to land exactly on it.
    resampler: Option<Resampler>,
    /// Mixed copy of the input (the caller's buffer is left untouched so
    /// several channels can share one capture block).
    scratch: Vec<IqSample>,
    /// Intermediate buffer between stages (reused across calls).
    inter: Vec<IqSample>,
    /// FIR-stage output, fed to the resampler (reused across calls).
    decimated: Vec<IqSample>,
}

impl Ddc {
    /// * `input_rate` / `output_rate` — any ratio ≥ 1. A non-integer ratio is
    ///   handled by decimating to just above `output_rate` and resampling the
    ///   leftover fraction, so any SDR sample rate feeds any channel rate.
    /// * `freq_offset_hz` — channel center relative to the capture center.
    /// * `passband_hz` — one-sided width of the signal to preserve.
    pub fn new(
        input_rate: f64,
        output_rate: f64,
        freq_offset_hz: f64,
        passband_hz: f64,
    ) -> Result<Self, String> {
        let ratio = input_rate / output_rate;
        // Integer-decimate by the floor of the ratio (never below the output
        // rate), then resample the remainder. An exact integer ratio leaves no
        // remainder and skips the resampler entirely (unchanged fast path).
        let decim = ratio.floor() as usize;
        if decim == 0 {
            return Err(format!(
                "input rate {input_rate} is below output rate {output_rate}"
            ));
        }
        if output_rate < 2.0 * passband_hz {
            return Err(format!(
                "output rate {output_rate} cannot carry a ±{passband_hz} Hz passband"
            ));
        }
        // Rate after integer decimation; equals output_rate for integer ratios.
        let inter_rate = input_rate / decim as f64;
        let resampler = if (inter_rate - output_rate).abs() > 1e-6 {
            Some(Resampler::new(inter_rate, output_rate))
        } else {
            None
        };

        // Split a large decimation into two stages: a cheap coarse stage and
        // a sharp final stage at the low rate.
        let factors: Vec<usize> = if decim <= 16 {
            vec![decim]
        } else {
            match (2..=16).rev().find(|d| decim % d == 0) {
                Some(d2) => vec![decim / d2, d2],
                None => vec![decim], // prime factor; single sharp stage
            }
        };

        let mut stages = Vec::new();
        let mut rate = input_rate;
        for &d in &factors {
            let out = rate / d as f64;
            // Anti-alias: pass `passband_hz`, stop where aliases would fold
            // back into the passband (out - passband).
            let transition = out - 2.0 * passband_hz;
            if transition <= 0.0 {
                return Err(format!(
                    "stage output rate {out} too low for ±{passband_hz} Hz passband"
                ));
            }
            let ntaps = ((TAPS_PER_TRANSITION * rate / transition).ceil() as usize | 1).max(9);
            if ntaps > MAX_TAPS {
                return Err(format!(
                    "decimation {decim} from {input_rate} S/s needs {ntaps} taps; \
                     choose a friendlier sample rate"
                ));
            }
            stages.push(Fir::with_decimation(lowpass_taps(passband_hz / rate, ntaps), d));
            rate = out;
        }

        Ok(Self {
            nco: Nco::new(freq_offset_hz, input_rate),
            stages,
            resampler,
            scratch: Vec::new(),
            inter: Vec::new(),
            decimated: Vec::new(),
        })
    }

    /// Mix + filter + decimate `input` (resampling to the exact output rate
    /// when needed), appending baseband output to `out`.
    pub fn process(&mut self, input: &[IqSample], out: &mut Vec<IqSample>) {
        self.scratch.clear();
        self.scratch.extend_from_slice(input);
        self.nco.mix(&mut self.scratch);

        // Integer-decimate through the FIR stage(s). With no resampler the
        // output rate is already exact, so write straight to `out`.
        let sink = if self.resampler.is_some() {
            self.decimated.clear();
            &mut self.decimated
        } else {
            &mut *out
        };
        match self.stages.len() {
            1 => self.stages[0].process(&self.scratch, sink),
            _ => {
                // Two stages is the max produced by `new`.
                self.inter.clear();
                let (first, rest) = self.stages.split_at_mut(1);
                first[0].process(&self.scratch, &mut self.inter);
                rest[0].process(&self.inter, sink);
            }
        }

        if let Some(rs) = &mut self.resampler {
            rs.process(&self.decimated, out);
        }
    }
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
    fn passes_channel_rejects_neighbor() {
        let fs = 2_400_000.0;
        let out_rate = 24_000.0;
        let offset = 25_000.0;

        // In-channel tone: offset + 1 kHz → should appear at 1 kHz, near unit gain
        let mut ddc = Ddc::new(fs, out_rate, offset, 5_000.0).unwrap();
        let input = tone(offset + 1_000.0, fs, 240_000);
        let mut out = Vec::new();
        ddc.process(&input, &mut out);
        let settled = &out[out.len() / 2..];
        let amp = settled.iter().map(|s| s.norm()).sum::<f32>() / settled.len() as f32;
        assert!((amp - 1.0).abs() < 0.05, "in-channel gain should be ~1, got {amp}");

        // Adjacent channel (+25 kHz away): must be strongly rejected
        let mut ddc = Ddc::new(fs, out_rate, offset, 5_000.0).unwrap();
        let input = tone(offset + 25_000.0, fs, 240_000);
        let mut out = Vec::new();
        ddc.process(&input, &mut out);
        let settled = &out[out.len() / 2..];
        let amp = settled.iter().map(|s| s.norm()).sum::<f32>() / settled.len() as f32;
        assert!(amp < 0.01, "adjacent channel should be rejected, got {amp}");
    }

    #[test]
    fn output_rate_correct() {
        let mut ddc = Ddc::new(2_400_000.0, 24_000.0, 0.0, 5_000.0).unwrap();
        let input = vec![IqSample::new(0.0, 0.0); 240_000];
        let mut out = Vec::new();
        ddc.process(&input, &mut out);
        assert_eq!(out.len(), 2_400);
    }

    #[test]
    fn resamples_non_integer_ratio() {
        // 2.048 MS/s is not an integer multiple of 24 kHz (ratio 85.33): the
        // DDC now decimates by 85 and resamples the remainder to land exactly
        // on 24 kHz, instead of rejecting the rate.
        let fs = 2_048_000.0;
        let out_rate = 24_000.0;
        let mut ddc = Ddc::new(fs, out_rate, 0.0, 5_000.0).unwrap();
        let input = vec![IqSample::new(0.0, 0.0); 2_048_000];
        let mut out = Vec::new();
        ddc.process(&input, &mut out);
        // ~1 s of capture → ~24000 output samples (within edge effects).
        assert!((out.len() as i64 - 24_000).abs() <= 4, "got {}", out.len());
    }

    #[test]
    fn resamples_in_channel_tone_at_non_integer_ratio() {
        // Airspy R2 case: extract a 24 kHz ACARS channel from 2.5 MS/s, which
        // 24 kHz does not divide. The in-channel tone must survive at unit
        // gain; an adjacent channel must still be rejected.
        let fs = 2_500_000.0;
        let out_rate = 24_000.0;
        let offset = 50_000.0;

        let mut ddc = Ddc::new(fs, out_rate, offset, 5_000.0).unwrap();
        let input = tone(offset + 1_000.0, fs, 2_500_000);
        let mut out = Vec::new();
        ddc.process(&input, &mut out);
        let settled = &out[out.len() / 2..];
        let amp = settled.iter().map(|s| s.norm()).sum::<f32>() / settled.len() as f32;
        assert!((amp - 1.0).abs() < 0.05, "in-channel gain should be ~1, got {amp}");

        let mut ddc = Ddc::new(fs, out_rate, offset, 5_000.0).unwrap();
        let input = tone(offset + 50_000.0, fs, 2_500_000);
        let mut out = Vec::new();
        ddc.process(&input, &mut out);
        let settled = &out[out.len() / 2..];
        let amp = settled.iter().map(|s| s.norm()).sum::<f32>() / settled.len() as f32;
        assert!(amp < 0.02, "adjacent channel should be rejected, got {amp}");
    }
}
