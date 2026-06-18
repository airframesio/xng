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
    /// Samples per half µs (integer path).
    half: usize,
    /// Samples per half µs when fractional (prefix-sum slot path).
    half_f: Option<f64>,
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
    /// Fractional offsets scanned besides the on-grid stream.
    fracs: Vec<f32>,
    /// Fractionally shifted power buffers (one per offset).
    power_frac: Vec<Vec<f32>>,
    validator: FrameValidator,
    noise: f32,
    /// Input sample rate (for the 12 MHz MLAT tick conversion).
    input_rate: f64,
    /// Absolute count of samples already drained from `power` — the stream
    /// index of `power[0]`. A frame at buffer position `pos` is at absolute
    /// sample `base_samples + pos`, giving a monotonic per-frame receive time.
    base_samples: u64,
}

impl PpmDemod {
    pub fn new(input_rate: f64) -> Result<Self, String> {
        Self::with_phases(input_rate, &[0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875])
    }

    /// `fracs` selects the interpolated timing grids scanned besides
    /// the on-grid one (2 MS/s input only). The full ⅛-sample set buys
    /// ~+16 unique frames on the modes1 benchmark at ~8× the scan
    /// cost; `&[0.5]` is the live/embedded compromise.
    pub fn with_phases(input_rate: f64, fracs: &[f32]) -> Result<Self, String> {
        let spu = input_rate / 1e6;
        if spu < 2.0 {
            return Err(format!(
                "Mode S needs at least 2 samples per µs; {input_rate} S/s gives {spu}"
            ));
        }
        // Non-integer (or odd) samples/µs runs the fractional-slot path
        // (prefix-sum integrals): 2.4 MS/s — the RTL-SDR's best rate —
        // works natively.
        let fractional = (spu - spu.round()).abs() > 1e-9 || (spu.round() as usize) % 2 != 0;
        let two_phase = !fractional && (spu.round() as usize) == 2 && !fracs.is_empty();
        Ok(Self {
            half: if fractional { 1 } else { spu.round() as usize / 2 },
            half_f: if fractional { Some(spu / 2.0) } else { None },
            two_phase,
            fracs: fracs.to_vec(),
            last_sample: Complex::new(0.0, 0.0),
            power: Vec::new(),
            power_frac: fracs.iter().map(|_| Vec::new()).collect(),
            validator: FrameValidator::new(),
            noise: 1e-6,
            input_rate,
            base_samples: 0,
        })
    }

    /// Energy in half-µs slot `slot` starting at sample `base`.
    #[inline]
    fn slot(power: &[f32], half: usize, base: usize, slot: usize) -> f32 {
        let s = base + slot * half;
        power[s..s + half].iter().sum()
    }

    /// Fractional-rate variant: energy in half-µs slot `slot` from a
    /// prefix-sum array, with linearly weighted fractional edges.
    /// `half_f` is samples per half-µs (e.g. 1.2 at 2.4 MS/s).
    #[inline]
    fn slot_f(prefix: &[f64], half_f: f64, frac: f64, base: usize, slot: usize) -> f32 {
        let start = base as f64 + frac + slot as f64 * half_f;
        let end = start + half_f;
        let (s0, s1) = (start.floor() as usize, end.floor() as usize);
        let (f0, f1) = (start - s0 as f64, end - s1 as f64);
        if s1 + 1 >= prefix.len() {
            return 0.0;
        }
        // ∫ start..end of the sample-held power.
        let whole = prefix[s1] - prefix[s0 + 1];
        let head = (prefix[s0 + 1] - prefix[s0]) * (1.0 - f0);
        let tail = (prefix[s1 + 1] - prefix[s1]) * f1;
        (whole + head + tail) as f32
    }

    /// Scan a fractional-rate power stream (prefix sums) — the path
    /// for rates that are not an even integer number of samples/µs
    /// (2.4 MS/s natively, the RTL-SDR's best rate). Same candidate
    /// gates and CRC arbitration as the integer path; the timing grid
    /// is every input sample (finer than 0.5 µs above 2 MS/s).
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn scan_f(
        power: &[f32],
        prefix: &[f64],
        half_f: f64,
        frac: f64,
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

            let pulses: f32 = PREAMBLE_PULSES
                .iter()
                .map(|&p| Self::slot_f(prefix, half_f, frac, i, p))
                .sum::<f32>()
                / 4.0;
            if pulses < *noise * half_f as f32 * 1.2 {
                i += 1;
                continue;
            }
            let quiet_max = PREAMBLE_QUIET
                .iter()
                .map(|&q| Self::slot_f(prefix, half_f, frac, i, q))
                .fold(0.0f32, f32::max);
            let pulse_min = PREAMBLE_PULSES
                .iter()
                .map(|&p| Self::slot_f(prefix, half_f, frac, i, p))
                .fold(f32::MAX, f32::min);
            if pulse_min < quiet_max * PULSE_QUIET_RATIO {
                i += 1;
                continue;
            }

            let data_off = PREAMBLE_US * 2;
            // Bit decisions sample interpolated power at each half-bit
            // CENTER: the slot-integral splits a boundary-straddling
            // sample's energy across both halves and flips bits at
            // adverse sampling phases (measured at 2.4 MS/s); the
            // center is always inside its pulse.
            // Bit decisions at interpolated half-bit centers of the
            // pass phase. Falsified alternatives (measured on the
            // readsb benchmark file): trimmed-slot integrals 152,
            // preamble-contrast phase refinement 154, vs centers 157.
            // (Per-candidate phase refinement by preamble contrast was
            // also tried: 154 — overfits preamble noise. Plain pass-
            // grid centers win.)
            let center = |slot: usize| -> f32 {
                let pos = i as f64 + frac + (slot as f64 + 0.5) * half_f;
                let s0 = pos.floor() as usize;
                let f = (pos - s0 as f64) as f32;
                if s0 + 1 >= power.len() {
                    return 0.0;
                }
                power[s0] * (1.0 - f) + power[s0 + 1] * f
            };
            let bit = |k: usize| -> u8 {
                let first = center(data_off + 2 * k);
                let second = center(data_off + 2 * k + 1);
                (first > second) as u8
            };
            let df = (0..5).fold(0u8, |v, k| (v << 1) | bit(k));
            let nbits = if df >= 16 { LONG_BITS } else { SHORT_BITS };
            let mut bytes = vec![0u8; nbits / 8];
            for k in 0..nbits {
                bytes[k / 8] = (bytes[k / 8] << 1) | bit(k);
            }

            let level = 10.0 * (pulses / half_f as f32).max(1e-12).log10();
            if let Some(frame) = validator.validate(&bytes, level, i) {
                out.append(&mut validator.released);
                out.push((i, frame));
                i += (((PREAMBLE_US + nbits) * 2) as f64 * half_f) as usize;
            } else {
                i += 1;
            }
        }
        out
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
            if let Some(frame) = validator.validate(&bytes, level, i) {
                out.append(&mut validator.released);
                out.push((i, frame));
                i += (PREAMBLE_US + nbits) * 2 * half;
            } else {
                i += 1;
            }
        }
        out
    }

    /// Monotonic 12 MHz sample-clock tick for a frame at buffer position `pos`
    /// (absolute sample `base_samples + pos`). Beast MLAT counter convention.
    fn tick(&self, pos: usize) -> u64 {
        (((self.base_samples + pos as u64) as f64 / self.input_rate) * 12_000_000.0) as u64
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<AdsbFrame> {
        self.power.extend(input.iter().map(|x| x.norm_sqr()));
        if self.two_phase {
            let mut prev = self.last_sample;
            for v in &mut self.power_frac {
                v.reserve(input.len());
            }
            for &x in input {
                for (j, frac) in self.fracs.iter().enumerate() {
                    let interp = prev * (1.0 - frac) + x * *frac;
                    self.power_frac[j].push(interp.norm_sqr());
                }
                prev = x;
            }
            if let Some(&last) = input.last() {
                self.last_sample = last;
            }
        }

        let frame_samples = match self.half_f {
            Some(hf) => (((PREAMBLE_US + LONG_BITS) * 2) as f64 * hf).ceil() as usize + 2,
            None => (PREAMBLE_US + LONG_BITS) * 2 * self.half,
        };
        let mut out = Vec::new();
        if self.power.len() < frame_samples {
            return out;
        }
        let end = self.power.len() - frame_samples;

        if let Some(hf) = self.half_f {
            let mut prefix = Vec::with_capacity(self.power.len() + 1);
            let mut acc = 0.0f64;
            prefix.push(0.0);
            for &p in &self.power {
                acc += p as f64;
                prefix.push(acc);
            }
            // Sub-sample phase passes (the integer path's grid sweep,
            // fractionally): candidates at each offset, merged by
            // bytes + position.
            let mut found: Vec<(usize, AdsbFrame)> = Vec::new();
            // Effort follows the integer path's grid choice: live (one
            // extra phase configured) runs 4 passes; max runs 16
            // (measured asymptote: 157 → 163 → 164 unique at 4/8/16).
            // Live was 2 until real-RF testing showed it left frames
            // on the table (modes1@2.4M: 281 → 296 of max's 313 going
            // 2 → 4 passes).
            let npass: usize = if self.fracs.len() <= 1 { 4 } else { 16 };
            for (pass, frac) in
                (0..npass).map(|k| k as f64 / npass as f64).enumerate()
            {
                let pass_found = Self::scan_f(
                    &self.power,
                    &prefix,
                    hf,
                    frac,
                    end,
                    &mut self.noise,
                    &mut self.validator,
                    pass == 0,
                );
                for (pos, f) in pass_found {
                    let dup = found.iter().any(|(p, g)| {
                        g.bytes == f.bytes
                            && (*p as i64 - pos as i64).unsigned_abs() < frame_samples as u64
                    });
                    if !dup {
                        found.push((pos, f));
                    }
                }
            }
            found.sort_by_key(|(p, _)| *p);
            out.extend(found.into_iter().map(|(p, mut f)| {
                f.rx_ticks_12mhz = self.tick(p);
                f
            }));
            self.power.drain(..end.min(self.power.len()));
            self.base_samples += end as u64;
            return out;
        }

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
        out.extend(found.into_iter().map(|(p, mut f)| {
            f.rx_ticks_12mhz = self.tick(p);
            f
        }));

        // Keep the unscanned tail for the next call.
        self.power.drain(..end.min(self.power.len()));
        if self.two_phase {
            for v in &mut self.power_frac {
                let n = end.min(v.len());
                v.drain(..n);
            }
        }
        self.base_samples += end as u64;
        out
    }

    pub fn noise_dbfs(&self) -> f32 {
        10.0 * self.noise.max(1e-12).log10()
    }
}
