//! Mode S PPM demodulator (magnitude domain).
//!
//! 1090 MHz PPM at 1 Mbps: each 1 µs bit cell carries a 0.5 µs pulse in
//! the first half (bit 1) or second half (bit 0). Frames start with an
//! 8 µs preamble: pulses at 0, 1.0, 3.5, 4.5 µs, quiet elsewhere.
//!
//! Works at any sample rate giving an even integer number of samples per
//! µs (2 MS/s and up). Candidate preambles are screened on pulse/quiet
//! energy ratios, bits are decided by half-cell energy comparison, and
//! candidates are confirmed by the CRC layer ([`crate::frame`]) — only
//! parity-valid frames are emitted.

use crate::frame::{AdsbFrame, FrameValidator};
use num_complex::Complex;

/// Preamble length in µs.
const PREAMBLE_US: usize = 8;
/// Pulse starts within the preamble, in half-µs units.
const PREAMBLE_PULSES: [usize; 4] = [0, 2, 7, 9];
/// Quiet half-µs slots within the preamble (between/after pulses).
const PREAMBLE_QUIET: [usize; 6] = [1, 3, 5, 11, 13, 15];
/// Candidate gate: mean pulse energy must exceed this multiple of the
/// worst quiet slot.
const PULSE_QUIET_RATIO: f32 = 2.0;
/// Long frame length in bits (DF >= 16).
const LONG_BITS: usize = 112;
const SHORT_BITS: usize = 56;
/// Noise floor smoothing.
const NOISE_ALPHA: f32 = 1e-4;

pub struct PpmDemod {
    /// Samples per half µs.
    half: usize,
    /// Power (|x|²) carry buffer across process() calls.
    power: Vec<f32>,
    validator: FrameValidator,
    noise: f32,
}

impl PpmDemod {
    pub fn new(input_rate: f64) -> Result<Self, String> {
        let spu = input_rate / 1e6;
        if (spu - spu.round()).abs() > 1e-9 || (spu.round() as usize) % 2 != 0 || spu < 2.0 {
            return Err(format!(
                "Mode S needs an even integer number of samples per µs; \
                 {input_rate} S/s gives {spu} (use e.g. 2000000)"
            ));
        }
        Ok(Self {
            half: spu.round() as usize / 2,
            power: Vec::new(),
            validator: FrameValidator::new(),
            noise: 1e-6,
        })
    }

    /// Energy in half-µs slot `slot` starting at sample `base`.
    #[inline]
    fn slot(&self, base: usize, slot: usize) -> f32 {
        let s = base + slot * self.half;
        self.power[s..s + self.half].iter().sum()
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<AdsbFrame> {
        self.power.extend(input.iter().map(|x| x.norm_sqr()));

        let frame_samples = (PREAMBLE_US + LONG_BITS) * 2 * self.half;
        let mut out = Vec::new();
        if self.power.len() < frame_samples {
            return out;
        }

        let mut i = 0;
        let end = self.power.len() - frame_samples;
        while i < end {
            self.noise += NOISE_ALPHA * (self.power[i] - self.noise);

            let pulses: f32 = PREAMBLE_PULSES.iter().map(|&p| self.slot(i, p)).sum::<f32>() / 4.0;
            if pulses < self.noise * self.half as f32 * 3.0 {
                i += 1;
                continue;
            }
            let quiet_max = PREAMBLE_QUIET
                .iter()
                .map(|&q| self.slot(i, q))
                .fold(0.0f32, f32::max);
            let pulse_min = PREAMBLE_PULSES
                .iter()
                .map(|&p| self.slot(i, p))
                .fold(f32::MAX, f32::min);
            if pulse_min < quiet_max * PULSE_QUIET_RATIO {
                i += 1;
                continue;
            }

            // Decode bits: data starts after the 8 µs preamble.
            let data = i + PREAMBLE_US * 2 * self.half;
            let bit = |k: usize| -> u8 {
                let first = self.slot(data, 2 * k);
                let second = self.slot(data, 2 * k + 1);
                (first > second) as u8
            };
            let df = (0..5).fold(0u8, |v, k| (v << 1) | bit(k));
            let nbits = if df >= 16 { LONG_BITS } else { SHORT_BITS };
            let mut bytes = vec![0u8; nbits / 8];
            for k in 0..nbits {
                bytes[k / 8] = (bytes[k / 8] << 1) | bit(k);
            }

            let level = 10.0 * (pulses / self.half as f32).max(1e-12).log10();
            if let Some(frame) = self.validator.validate(&bytes, level) {
                out.push(frame);
                i += (PREAMBLE_US + nbits) * 2 * self.half;
            } else {
                i += 1;
            }
        }

        // Keep the unscanned tail for the next call.
        self.power.drain(..end.min(self.power.len()));
        out
    }

    pub fn noise_dbfs(&self) -> f32 {
        10.0 * self.noise.max(1e-12).log10()
    }
}
