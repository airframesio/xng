//! Coherent BPSK demodulator for STD-C (1200 sym/s): square-law FFT
//! coarse frequency acquisition, decision-directed Costas loop, Gardner
//! timing recovery. The 180° phase ambiguity is resolved downstream at
//! the frame layer (UW matched in both polarities).

use num_complex::Complex;
use rustfft::FftPlanner;
use std::sync::Arc;
use xng_dsp::{lowpass_taps, Fir};

const SYMBOL_RATE: f64 = 1200.0;
const COARSE_FFT: usize = 8192;
/// Costas loop gains (updates at symbol strobes).
const PHASE_GAIN: f32 = 0.05;
const FREQ_GAIN: f32 = 0.002;
const TIMING_GAIN: f64 = 0.02;
const AGC_ALPHA: f32 = 0.01;

pub struct BpskDemod {
    spb: f64,
    lpf: Fir,
    filtered: Vec<Complex<f32>>,
    fft: Arc<dyn rustfft::Fft<f32>>,
    /// Coarse acquisition buffer (squared signal).
    coarse_buf: Vec<Complex<f32>>,
    /// True once the frame layer reports UW lock; stops re-acquisition.
    pub locked: bool,
    nco_phase: f32,
    /// NCO frequency in radians/sample.
    nco_freq: f32,
    timing: f64,
    history: [Complex<f32>; 24],
    hist_pos: usize,
    sample_idx: u64,
    prev_sym: f32,
    agc: f32,
    level: f32,
    /// Carrier-lock quality: EMA of |Costas error| (small when locked).
    carr_err: f32,
}

impl BpskDemod {
    pub fn new(channel_rate: f64) -> Self {
        Self {
            spb: channel_rate / SYMBOL_RATE,
            lpf: Fir::new(lowpass_taps(1000.0 / channel_rate, 121)),
            filtered: Vec::new(),
            fft: FftPlanner::new().plan_fft_forward(COARSE_FFT),
            coarse_buf: Vec::with_capacity(COARSE_FFT),
            locked: false,
            nco_phase: 0.0,
            nco_freq: 0.0,
            timing: 0.0,
            history: [Complex::new(0.0, 0.0); 24],
            hist_pos: 0,
            sample_idx: 0,
            prev_sym: 0.0,
            agc: 1e-3,
            level: 0.0,
            carr_err: 1.0,
        }
    }

    /// Interpolated history sample `delay` samples in the past.
    fn past(&self, delay: f64) -> Complex<f32> {
        let n = self.history.len();
        let i = delay.floor() as usize;
        let frac = (delay - i as f64) as f32;
        let a = self.history[(self.hist_pos + n - 1 - i) % n];
        let b = self.history[(self.hist_pos + n - 2 - i) % n];
        a * (1.0 - frac) + b * frac
    }

    fn coarse_estimate(&mut self) -> Option<f32> {
        if self.coarse_buf.len() < COARSE_FFT {
            return None;
        }
        let mut buf = self.coarse_buf.clone();
        self.coarse_buf.clear();
        self.fft.process(&mut buf);
        let (best, _) = buf
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.norm_sqr().partial_cmp(&b.1.norm_sqr()).unwrap())?;
        // Squared BPSK puts a tone at 2× the carrier offset.
        let bin = if best <= COARSE_FFT / 2 { best as f32 } else { best as f32 - COARSE_FFT as f32 };
        Some(std::f32::consts::TAU * bin / COARSE_FFT as f32 / 2.0)
    }

    /// Feed channel IQ; append soft symbol decisions (+ = bit 1).
    pub fn process(&mut self, input: &[Complex<f32>], out: &mut Vec<f32>) {
        for &raw in input {
            self.level += 0.001 * (raw.norm_sqr() - self.level);
            let x = raw;

            // Coarse AFC while unlocked. Snap only when the estimate is
            // far from the current NCO — re-snapping every window would
            // discard the Costas loop's converged fine correction and
            // leave a re-convergence tail of symbol errors.
            if !self.locked {
                self.coarse_buf.push(x * x);
                if self.coarse_buf.len() == COARSE_FFT {
                    if let Some(w) = self.coarse_estimate() {
                        let bin = std::f32::consts::TAU / COARSE_FFT as f32 / 2.0;
                        if (-w - self.nco_freq).abs() > 4.0 * bin {
                            self.nco_freq = -w;
                        }
                    }
                }
            }

            // Mix by the NCO FIRST (so the narrow filter sees a centered
            // signal regardless of carrier offset), then lowpass.
            let mixed = x * Complex::from_polar(1.0, self.nco_phase);
            self.nco_phase += self.nco_freq;
            if self.nco_phase.abs() > std::f32::consts::TAU {
                self.nco_phase %= std::f32::consts::TAU;
            }
            self.filtered.clear();
            self.lpf.process(&[mixed], &mut self.filtered);
            let Some(&y) = self.filtered.first() else { continue };
            self.history[self.hist_pos] = y;
            self.hist_pos = (self.hist_pos + 1) % self.history.len();
            self.sample_idx += 1;
            if self.sample_idx < self.history.len() as u64 {
                continue;
            }

            self.timing += 1.0;
            if self.timing < self.spb {
                continue;
            }
            self.timing -= self.spb;
            // Symbol instant: interpolate now and at the midpoint.
            let now = self.past(self.timing);
            let mid = self.past(self.timing + self.spb / 2.0);

            self.agc += AGC_ALPHA * (now.re.abs() - self.agc);
            let sym = now.re / self.agc.max(1e-9);

            // Decision-directed Costas error; its magnitude doubles as a
            // carrier-lock quality measure.
            let perr = now.im / self.agc.max(1e-9) * sym.signum();
            self.nco_phase -= PHASE_GAIN * perr;
            self.nco_freq -= FREQ_GAIN * perr / self.spb as f32;
            self.carr_err += 0.05 * (perr.abs() - self.carr_err);

            // Gardner timing — gated on carrier lock: while the carrier
            // spins, Gardner errors are garbage and random-walk the clock
            // (symbol slips that corrupt whole frames).
            if self.carr_err < 0.4 {
                let terr = ((now.re - self.prev_sym * self.agc) * mid.re) as f64
                    / (self.agc * self.agc).max(1e-9) as f64;
                // Tight clamp: spikes must never accumulate into a symbol
                // slip within a frame (10368 symbols).
                self.timing += (TIMING_GAIN * terr).clamp(-0.08, 0.08);
            }

            self.prev_sym = sym.clamp(-2.0, 2.0);
            out.push(sym.clamp(-2.0, 2.0));
        }
    }

    /// Debug: (nco_freq rad/sample, carrier-error EMA, timing phase).
    pub fn debug_state(&self) -> (f32, f32, f64) {
        (self.nco_freq, self.carr_err, self.timing)
    }

    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }
}
