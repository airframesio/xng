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
const PULSE_QUIET_RATIO: f32 = 0.5;
/// Long frame length in bits (DF >= 16).
const LONG_BITS: usize = 112;
const SHORT_BITS: usize = 56;
/// Noise floor smoothing.
const NOISE_ALPHA: f32 = 1e-4;

pub struct PpmDemod {
    /// Samples per half µs.
    half: usize,
    /// Fractional timing phases active (2 MS/s input): a pulse landing
    /// between samples splits its energy across two half-µs slots and
    /// weak frames decide wrong. The original grid is scanned exactly
    /// as before; quarter-sample-shifted interpolated grids are scanned
    /// independently and their *additional* frames merged in — never
    /// replacing an on-grid decode (interpolation blurs pulse/quiet
    /// contrast, measured −35 frames when used alone).
    two_phase: bool,
    last_sample: Complex<f32>,
    /// Power (|x|²) carry buffer across process() calls.
    power: Vec<f32>,
    /// Fractionally shifted power buffers (⅛-sample offset grid).
    power_frac: [Vec<f32>; 7],
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
        let two_phase = (spu.round() as usize) == 2;
        Ok(Self {
            half: spu.round() as usize / 2,
            two_phase,
            last_sample: Complex::new(0.0, 0.0),
            power: Vec::new(),
            power_frac: std::array::from_fn(|_| Vec::new()),
            validator: FrameValidator::new(),
            noise: 1e-6,
        })
    }

    /// Energy in half-µs slot `slot` starting at sample `base`.
    #[inline]
    fn slot(power: &[f32], half: usize, base: usize, slot: usize) -> f32 {
        let s = base + slot * half;
        power[s..s + half].iter().sum()
    }

    /// Scan one power grid; emits (position, frame).
    fn scan(
        power: &[f32],
        half: usize,
        end: usize,
        noise: &mut f32,
        validator: &mut FrameValidator,
        track_noise: bool,
    ) -> Vec<(usize, AdsbFrame)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < end {
            if track_noise {
                *noise += NOISE_ALPHA * (power[i] - *noise);
            }

            let pulses: f32 =
                PREAMBLE_PULSES.iter().map(|&p| Self::slot(power, half, i, p)).sum::<f32>() / 4.0;
            if pulses < *noise * half as f32 * 1.2 {
                i += 1;
                continue;
            }
            let quiet_max = PREAMBLE_QUIET
                .iter()
                .map(|&q| Self::slot(power, half, i, q))
                .fold(0.0f32, f32::max);
            let pulse_min = PREAMBLE_PULSES
                .iter()
                .map(|&p| Self::slot(power, half, i, p))
                .fold(f32::MAX, f32::min);
            if pulse_min < quiet_max * PULSE_QUIET_RATIO {
                i += 1;
                continue;
            }

            // Decode bits: data starts after the 8 µs preamble.
            let data = i + PREAMBLE_US * 2 * half;
            let bit = |k: usize| -> u8 {
                let first = Self::slot(power, half, data, 2 * k);
                let second = Self::slot(power, half, data, 2 * k + 1);
                (first > second) as u8
            };
            let df = (0..5).fold(0u8, |v, k| (v << 1) | bit(k));
            let nbits = if df >= 16 { LONG_BITS } else { SHORT_BITS };
            let mut bytes = vec![0u8; nbits / 8];
            for k in 0..nbits {
                bytes[k / 8] = (bytes[k / 8] << 1) | bit(k);
            }

            let level = 10.0 * (pulses / half as f32).max(1e-12).log10();
            if let Some(frame) = validator.validate(&bytes, level) {
                out.push((i, frame));
                i += (PREAMBLE_US + nbits) * 2 * half;
            } else {
                i += 1;
            }
        }
        out
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<AdsbFrame> {
        self.power.extend(input.iter().map(|x| x.norm_sqr()));
        if self.two_phase {
            let mut prev = self.last_sample;
            for v in &mut self.power_frac {
                v.reserve(input.len());
            }
            for &x in input {
                for (j, frac) in
                    [0.125f32, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875].iter().enumerate()
                {
                    let interp = prev * (1.0 - frac) + x * *frac;
                    self.power_frac[j].push(interp.norm_sqr());
                }
                prev = x;
            }
            if let Some(&last) = input.last() {
                self.last_sample = last;
            }
        }

        let frame_samples = (PREAMBLE_US + LONG_BITS) * 2 * self.half;
        let mut out = Vec::new();
        if self.power.len() < frame_samples {
            return out;
        }
        let end = self.power.len() - frame_samples;

        let raw_found = Self::scan(
            &self.power,
            self.half,
            end,
            &mut self.noise,
            &mut self.validator,
            true,
        );
        let mut found: Vec<(usize, AdsbFrame)> = Vec::new();
        for (pos, f) in raw_found {
            let dup = found.iter().any(|(p, g)| {
                g.bytes == f.bytes
                    && (*p as i64 - pos as i64).unsigned_abs() < frame_samples as u64
            });
            if !dup {
                found.push((pos, f));
            }
        }
        if self.two_phase {
            for grid in &self.power_frac {
                let frames = Self::scan(
                    grid,
                    self.half,
                    end.min(grid.len().saturating_sub(frame_samples)),
                    &mut self.noise,
                    &mut self.validator,
                    false,
                );
                // Merge: keep shifted decodes only when no equal-bytes
                // frame was already found nearby (legitimate repeats of
                // identical messages are many frame-lengths apart).
                for (pos, f) in frames {
                    let dup = found.iter().any(|(p, g)| {
                        g.bytes == f.bytes
                            && (*p as i64 - pos as i64).unsigned_abs() < frame_samples as u64
                    });
                    if !dup {
                        found.push((pos, f));
                    }
                }
            }
        }
        found.sort_by_key(|(p, _)| *p);
        out.extend(found.into_iter().map(|(_, f)| f));

        // Keep the unscanned tail for the next call.
        self.power.drain(..end.min(self.power.len()));
        if self.two_phase {
            for v in &mut self.power_frac {
                let n = end.min(v.len());
                v.drain(..n);
            }
        }
        out
    }

    pub fn noise_dbfs(&self) -> f32 {
        10.0 * self.noise.max(1e-12).log10()
    }
}
