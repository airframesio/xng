//! ACARS MSK demodulator.
//!
//! Input: complex channel IQ at 24 kHz (10 samples/bit at 2400 bd), AM
//! carrier with audio MSK tones at 1200/2400 Hz.
//!
//! Chain: AM envelope (|IQ|, immune to carrier frequency offset) → DC block
//! (EMA highpass removes the carrier level) → complex mix by −1800 Hz (the
//! tone midpoint) → 1300 Hz lowpass (wide enough for the MSK main lobe at
//! ±600 Hz ± 2400 bd transitions; rejects the −3000/−4200 Hz mixing images)
//! → per-sample frequency discriminator (1200 Hz → −600 Hz, 2400 Hz →
//! +600 Hz) → per-bit integrate-and-dump with zero-crossing timing recovery
//! → differential decode (ARINC 618 §4.4.2: 1200 Hz = bit change, 2400 Hz =
//! no change; all-ones pre-key radiates continuous 2400 Hz).
//!
//! The differential mapping makes the bit stream polarity-ambiguous at
//! start-up (the initial state is unknown when we tune in mid-burst), so
//! the deframer hunts for the sync pattern in both polarities.

use crate::CHANNEL_RATE;
use num_complex::Complex;
use xng_dsp::{lowpass_taps, Fir, Nco};

const BAUD: f64 = 2400.0;
const SAMPLES_PER_BIT: usize = 10;
const TONE_MID_HZ: f64 = 1800.0;
const AUDIO_LPF_CUTOFF: f64 = 1300.0;
const AUDIO_LPF_TAPS: usize = 121;
/// Envelope DC tracker: fc ≈ alpha·fs/2π ≈ 19 Hz, settles within the pre-key.
const DC_ALPHA: f32 = 0.005;
/// Timing loop gain (fraction of the phase error applied per zero crossing).
const TIMING_GAIN: f64 = 0.15;
/// Envelope power smoothing factor for the level estimate.
const LEVEL_ALPHA: f32 = 0.005;

pub struct MskDemod {
    mix: Nco,
    lpf: Fir,
    mixed: Vec<Complex<f32>>,
    filtered: Vec<Complex<f32>>,
    prev_sample: Complex<f32>,
    prev_disc: f32,
    /// Bit-timing phase in samples, advances by 1 per sample, wraps at
    /// SAMPLES_PER_BIT (the bit boundary).
    timing: f64,
    /// Discriminator integrator over the current bit window.
    acc: f32,
    /// Differential decode state (last emitted bit).
    prev_bit: u8,
    /// Smoothed envelope power for RSSI.
    level: f32,
    /// Envelope DC (carrier level) tracker.
    dc: f32,
}

impl MskDemod {
    pub fn new() -> Self {
        assert_eq!(CHANNEL_RATE as usize, (BAUD as usize) * SAMPLES_PER_BIT);
        Self {
            mix: Nco::new(TONE_MID_HZ, CHANNEL_RATE),
            lpf: Fir::new(lowpass_taps(AUDIO_LPF_CUTOFF / CHANNEL_RATE, AUDIO_LPF_TAPS)),
            mixed: Vec::new(),
            filtered: Vec::new(),
            prev_sample: Complex::new(0.0, 0.0),
            prev_disc: 0.0,
            timing: 0.0,
            acc: 0.0,
            prev_bit: 1, // pre-key state is all ones
            level: 0.0,
            dc: 0.0,
        }
    }

    /// Feed channel IQ; append hard bit decisions to `bits`.
    pub fn process(&mut self, input: &[Complex<f32>], bits: &mut Vec<u8>) {
        // AM envelope → DC block → complex, mixed down by the tone midpoint.
        self.mixed.clear();
        for x in input {
            let env = x.norm();
            self.level += LEVEL_ALPHA * (env * env - self.level);
            self.dc += DC_ALPHA * (env - self.dc);
            self.mixed.push(Complex::new(env - self.dc, 0.0));
        }
        self.mix.mix(&mut self.mixed);

        self.filtered.clear();
        self.lpf.process(&self.mixed, &mut self.filtered);

        for &y in &self.filtered {
            // Frequency discriminator: phase advance per sample.
            let disc = (y * self.prev_sample.conj()).arg();
            self.prev_sample = y;

            // Timing: tone transitions cross zero at bit boundaries
            // (timing == 0 mod SAMPLES_PER_BIT). Nudge the phase so
            // crossings align with the boundary.
            if disc != 0.0 && self.prev_disc != 0.0 && (disc < 0.0) != (self.prev_disc < 0.0) {
                let spb = SAMPLES_PER_BIT as f64;
                let err = self.timing - (self.timing / spb).round() * spb;
                self.timing -= TIMING_GAIN * err;
            }
            self.prev_disc = disc;

            self.acc += disc;
            self.timing += 1.0;
            if self.timing >= SAMPLES_PER_BIT as f64 {
                self.timing -= SAMPLES_PER_BIT as f64;
                // Mean frequency < 0 → 1200 Hz tone → bit change.
                let change = (self.acc < 0.0) as u8;
                self.acc = 0.0;
                self.prev_bit ^= change;
                bits.push(self.prev_bit);
            }
        }
    }

    /// Smoothed envelope level in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }
}

impl Default for MskDemod {
    fn default() -> Self {
        Self::new()
    }
}
