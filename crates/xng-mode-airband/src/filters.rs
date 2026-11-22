//! Audio-rate filters for the voice path: second-order IIR sections from the
//! textbook bilinear-transform designs (high-pass, low-pass, notch), a
//! one-pole de-emphasis for NFM, and a Goertzel tone detector for CTCSS.

use std::f64::consts::PI;

/// Direct-form-I biquad.
#[derive(Clone, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    fn from_coeffs(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Self {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn w0_alpha(f0: f64, q: f64, rate: f64) -> (f64, f64, f64) {
        let w0 = 2.0 * PI * (f0 / rate).clamp(1e-6, 0.499);
        let (s, c) = w0.sin_cos();
        (c, s / (2.0 * q), w0)
    }

    /// Second-order low-pass at `f0` Hz.
    pub fn lowpass(f0: f64, q: f64, rate: f64) -> Self {
        let (c, alpha, _) = Self::w0_alpha(f0, q, rate);
        let b1 = 1.0 - c;
        Self::from_coeffs(b1 / 2.0, b1, b1 / 2.0, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    /// Second-order high-pass at `f0` Hz.
    pub fn highpass(f0: f64, q: f64, rate: f64) -> Self {
        let (c, alpha, _) = Self::w0_alpha(f0, q, rate);
        let b0 = (1.0 + c) / 2.0;
        Self::from_coeffs(b0, -(1.0 + c), b0, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    /// Band-reject (notch) at `f0` Hz; higher `q` = narrower notch.
    pub fn notch(f0: f64, q: f64, rate: f64) -> Self {
        let (c, alpha, _) = Self::w0_alpha(f0, q, rate);
        Self::from_coeffs(1.0, -2.0 * c, 1.0, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        // Flush denormals: a silent channel otherwise decays into subnormal
        // floats, which are very slow on some CPUs.
        self.y1 = if y.abs() < 1e-20 { 0.0 } else { y };
        self.y1
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

/// One-pole low-pass used as FM de-emphasis with time constant `tau_s`.
#[derive(Clone, Debug)]
pub struct Deemphasis {
    a: f32,
    y: f32,
}

impl Deemphasis {
    pub fn new(tau_s: f64, rate: f64) -> Self {
        let a = 1.0 - (-1.0 / (tau_s * rate)).exp();
        Self { a: a as f32, y: 0.0 }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.y += self.a * (x - self.y);
        self.y
    }
}

/// Goertzel single-bin power estimator.
#[derive(Clone, Debug)]
pub struct Goertzel {
    coeff: f32,
    s1: f32,
    s2: f32,
    n: usize,
}

impl Goertzel {
    pub fn new(freq_hz: f64, rate: f64) -> Self {
        let w = 2.0 * PI * freq_hz / rate;
        Self { coeff: (2.0 * w.cos()) as f32, s1: 0.0, s2: 0.0, n: 0 }
    }

    #[inline]
    pub fn push(&mut self, x: f32) {
        let s = x + self.coeff * self.s1 - self.s2;
        self.s2 = self.s1;
        self.s1 = s;
        self.n += 1;
    }

    /// Bin power normalised by the window length, so a unit-amplitude tone
    /// reads ~0.25 regardless of window size.
    pub fn power(&self) -> f32 {
        if self.n == 0 {
            return 0.0;
        }
        let p = self.s1 * self.s1 + self.s2 * self.s2 - self.coeff * self.s1 * self.s2;
        p / (self.n as f32 * self.n as f32)
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
        self.n = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f64, rate: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| (2.0 * PI * f * i as f64 / rate).sin() as f32).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn run(mut f: Biquad, x: &[f32]) -> Vec<f32> {
        x.iter().map(|&v| f.process(v)).collect()
    }

    #[test]
    fn highpass_blocks_low_passes_high() {
        let rate = 16_000.0;
        let lo = run(Biquad::highpass(300.0, 0.707, rate), &tone(50.0, rate, 16_000));
        let hi = run(Biquad::highpass(300.0, 0.707, rate), &tone(1_000.0, rate, 16_000));
        assert!(rms(&lo[8000..]) < 0.05, "50 Hz leaked: {}", rms(&lo[8000..]));
        assert!(rms(&hi[8000..]) > 0.65);
    }

    #[test]
    fn lowpass_blocks_high() {
        let rate = 16_000.0;
        let hi = run(Biquad::lowpass(3_000.0, 0.707, rate), &tone(7_000.0, rate, 16_000));
        let mid = run(Biquad::lowpass(3_000.0, 0.707, rate), &tone(1_000.0, rate, 16_000));
        assert!(rms(&hi[8000..]) < 0.15);
        assert!(rms(&mid[8000..]) > 0.65);
    }

    #[test]
    fn notch_removes_its_frequency_only() {
        let rate = 16_000.0;
        let at = run(Biquad::notch(1_000.0, 10.0, rate), &tone(1_000.0, rate, 32_000));
        let off = run(Biquad::notch(1_000.0, 10.0, rate), &tone(2_000.0, rate, 32_000));
        assert!(rms(&at[16000..]) < 0.02);
        assert!(rms(&off[16000..]) > 0.65);
    }

    #[test]
    fn goertzel_picks_its_tone() {
        let rate = 16_000.0;
        let x = tone(100.0, rate, 6_400);
        let mut on = Goertzel::new(100.0, rate);
        let mut off = Goertzel::new(150.0, rate);
        for &v in &x {
            on.push(v);
            off.push(v);
        }
        assert!(on.power() > 0.2);
        assert!(off.power() < 0.01);
    }
}
