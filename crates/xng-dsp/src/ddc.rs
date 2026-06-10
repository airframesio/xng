//! Digital downconverter: extract one narrowband channel from a wideband
//! capture (NCO mix to baseband + decimating anti-alias FIR, split into two
//! stages when the decimation factor is large).

use crate::fir::{lowpass_taps, Fir};
use crate::nco::Nco;
use crate::IqSample;

/// Required stopband attenuation drives taps ≈ 5.5 / normalized transition
/// width for the 4-term Blackman-Harris design.
const TAPS_PER_TRANSITION: f64 = 5.5;
const MAX_TAPS: usize = 8192;

pub struct Ddc {
    nco: Nco,
    stages: Vec<Fir>,
    /// Mixed copy of the input (the caller's buffer is left untouched so
    /// several channels can share one capture block).
    scratch: Vec<IqSample>,
    /// Intermediate buffer between stages (reused across calls).
    inter: Vec<IqSample>,
}

impl Ddc {
    /// * `input_rate` / `output_rate` — must divide to an integer factor.
    /// * `freq_offset_hz` — channel center relative to the capture center.
    /// * `passband_hz` — one-sided width of the signal to preserve.
    pub fn new(
        input_rate: f64,
        output_rate: f64,
        freq_offset_hz: f64,
        passband_hz: f64,
    ) -> Result<Self, String> {
        let ratio = input_rate / output_rate;
        let decim = ratio.round() as usize;
        if decim == 0 || (ratio - decim as f64).abs() > 1e-6 {
            return Err(format!(
                "input rate {input_rate} is not an integer multiple of output rate {output_rate}"
            ));
        }
        if output_rate < 2.0 * passband_hz {
            return Err(format!(
                "output rate {output_rate} cannot carry a ±{passband_hz} Hz passband"
            ));
        }

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
            scratch: Vec::new(),
            inter: Vec::new(),
        })
    }

    /// Mix + filter + decimate `input`, appending baseband output to `out`.
    pub fn process(&mut self, input: &[IqSample], out: &mut Vec<IqSample>) {
        self.scratch.clear();
        self.scratch.extend_from_slice(input);
        self.nco.mix(&mut self.scratch);
        match self.stages.len() {
            1 => self.stages[0].process(&self.scratch, out),
            _ => {
                // Two stages is the max produced by `new`.
                self.inter.clear();
                let (first, rest) = self.stages.split_at_mut(1);
                first[0].process(&self.scratch, &mut self.inter);
                rest[0].process(&self.inter, out);
            }
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
    fn rejects_non_integer_ratio() {
        assert!(Ddc::new(2_048_000.0, 24_000.0, 0.0, 5_000.0).is_err());
    }
}
