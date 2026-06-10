//! Aero A-BPSK (MSK-class) demodulator, discriminator-based.
//!
//! A-BPSK is BPSK with sinusoidal transitions — an MSK-class signal with
//! ±fb/4 deviation. The data maps directly onto the deviation sign
//! (bit 1 = +90° phase advance over the bit), so a frequency
//! discriminator with per-bit integration yields the data with no
//! differential step — validated against JAERO's off-air 600 bps
//! recording (UW appears in true polarity at 1200-bit frame spacing).
//! This is simpler than JAERO's coherent OQPSK-decomposition demod at a
//! sensitivity cost of a couple of dB (see PROVENANCE.md); the soft
//! outputs feed the Viterbi.

use num_complex::Complex;
use xng_dsp::{lowpass_taps, Fir};

const FREQ_ALPHA: f32 = 0.0004;
const LEVEL_ALPHA: f32 = 0.005;
const TIMING_GAIN: f64 = 0.1;
const MAG_ALPHA: f32 = 0.01;

pub struct MskDemod {
    spb: f64,
    /// Rate-matched lowpass ahead of the discriminator: the channel is
    /// wide (shared with the other rate chain) but a 600 bps MSK signal
    /// only occupies ~±0.6·fb — out-of-band noise would otherwise swamp
    /// the discriminator and its timing loop.
    lpf: Fir,
    filtered: Vec<Complex<f32>>,
    prev_sample: Complex<f32>,
    prev_disc: f32,
    freq_offset: f32,
    timing: f64,
    acc: f32,
    acc_n: u32,
    /// Running mean |integral| for normalization.
    mag: f32,
    level: f32,
}

impl MskDemod {
    pub fn new(channel_rate: f64, bit_rate: f64) -> Self {
        let cutoff = 0.6 * bit_rate / channel_rate;
        Self {
            spb: channel_rate / bit_rate,
            lpf: Fir::new(lowpass_taps(cutoff, 101)),
            filtered: Vec::new(),
            prev_sample: Complex::new(0.0, 0.0),
            prev_disc: 0.0,
            freq_offset: 0.0,
            timing: 0.0,
            acc: 0.0,
            acc_n: 0,
            mag: 1e-3,
            level: 0.0,
        }
    }

    /// Feed channel IQ; append (soft in -1..1, hard 0/1) bits.
    pub fn process(&mut self, input: &[Complex<f32>], out: &mut Vec<(f32, u8)>) {
        self.filtered.clear();
        self.lpf.process(input, &mut self.filtered);
        for &x in &self.filtered {
            self.level += LEVEL_ALPHA * (x.norm_sqr() - self.level);
            let raw = (x * self.prev_sample.conj()).arg();
            self.prev_sample = x;
            self.freq_offset += FREQ_ALPHA * (raw - self.freq_offset);
            let disc = raw - self.freq_offset;

            if disc != 0.0 && self.prev_disc != 0.0 && (disc < 0.0) != (self.prev_disc < 0.0) {
                let err = self.timing - (self.timing / self.spb).round() * self.spb;
                self.timing -= TIMING_GAIN * err;
            }
            self.prev_disc = disc;

            self.acc += disc;
            self.acc_n += 1;
            self.timing += 1.0;
            if self.timing >= self.spb {
                self.timing -= self.spb;
                let l = self.acc / self.acc_n.max(1) as f32;
                self.acc = 0.0;
                self.acc_n = 0;
                self.mag += MAG_ALPHA * (l.abs() - self.mag);
                // Direct mapping: deviation sign is the bit.
                let soft = (l / self.mag.max(1e-9)).clamp(-1.0, 1.0);
                out.push((soft, (soft > 0.0) as u8));
            }
        }
    }

    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }
}
