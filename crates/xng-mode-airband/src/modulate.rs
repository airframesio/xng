//! Signal synthesis for self-generated tests: AM and NFM voice-like
//! transmissions in complex baseband, plus white Gaussian noise.

use num_complex::Complex;
use std::f64::consts::TAU;

/// Deterministic xorshift noise source (no `rand` dependency).
pub struct Noise(u64);

impl Noise {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal (Box–Muller).
    pub fn gauss(&mut self) -> f64 {
        let u1 = self.uniform().max(1e-12);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (TAU * u2).cos()
    }

    /// Complex AWGN with total power `power` (linear, |x|²).
    pub fn complex(&mut self, power: f64) -> Complex<f32> {
        let s = (power / 2.0).sqrt();
        Complex::new((self.gauss() * s) as f32, (self.gauss() * s) as f32)
    }
}

/// A test transmission.
#[derive(Clone, Debug)]
pub struct Burst {
    /// Start time in seconds.
    pub start_s: f64,
    /// Length in seconds.
    pub len_s: f64,
    /// Carrier amplitude (|x|).
    pub amplitude: f64,
    /// Audio tone (Hz) modulating the carrier.
    pub tone_hz: f64,
    /// AM modulation index (0..1) or NFM deviation in Hz.
    pub depth: f64,
    /// Optional CTCSS tone (NFM) and its deviation.
    pub ctcss: Option<(f64, f64)>,
}

/// AM bursts at `offset_hz` from the capture center, in AWGN of `noise_power`.
pub fn am_capture(rate: f64, secs: f64, offset_hz: f64, bursts: &[Burst], noise_power: f64, seed: u64) -> Vec<Complex<f32>> {
    let n = (rate * secs) as usize;
    let mut noise = Noise::new(seed);
    (0..n)
        .map(|i| {
            let t = i as f64 / rate;
            let mut x = noise.complex(noise_power);
            for b in bursts {
                if t >= b.start_s && t < b.start_s + b.len_s {
                    let env = b.amplitude * (1.0 + b.depth * (TAU * b.tone_hz * t).sin());
                    let ph = TAU * offset_hz * t;
                    x += Complex::new((env * ph.cos()) as f32, (env * ph.sin()) as f32);
                }
            }
            x
        })
        .collect()
}

/// NFM bursts (deviation `depth` Hz) at `offset_hz`, in AWGN.
pub fn nfm_capture(rate: f64, secs: f64, offset_hz: f64, bursts: &[Burst], noise_power: f64, seed: u64) -> Vec<Complex<f32>> {
    let n = (rate * secs) as usize;
    let mut noise = Noise::new(seed);
    let mut phase = vec![0.0f64; bursts.len()];
    (0..n)
        .map(|i| {
            let t = i as f64 / rate;
            let mut x = noise.complex(noise_power);
            for (k, b) in bursts.iter().enumerate() {
                if t >= b.start_s && t < b.start_s + b.len_s {
                    let mut inst = offset_hz + b.depth * (TAU * b.tone_hz * t).sin();
                    if let Some((f, dev)) = b.ctcss {
                        inst += dev * (TAU * f * t).sin();
                    }
                    phase[k] += TAU * inst / rate;
                    x += Complex::new(
                        (b.amplitude * phase[k].cos()) as f32,
                        (b.amplitude * phase[k].sin()) as f32,
                    );
                }
            }
            x
        })
        .collect()
}
