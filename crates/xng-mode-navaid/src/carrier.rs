//! Carrier acquisition and coherent AM detection.
//!
//! Every navaid is a carrier with low-frequency AM. Other open decoders take
//! the envelope `|z|`, which is simple but has a threshold: below about
//! 10 dB in the detector bandwidth the noise cross terms swamp the
//! modulation. We instead find the carrier with an FFT, track it with a
//! second-order PLL and use the in-phase component. Coherent detection is
//! linear at any SNR, removes the RTL-SDR's frequency error, and separates
//! the two carriers of a two-frequency localizer, which the envelope mixes
//! into beat notes.

use num_complex::Complex;
use rustfft::FftPlanner;
use std::f64::consts::TAU;

/// One carrier found by [`acquire`].
#[derive(Debug, Clone, Copy)]
pub struct Carrier {
    /// Offset from the channel center (Hz).
    pub freq_hz: f64,
    /// Peak power relative to the noise floor in one FFT bin (dB).
    pub peak_db: f64,
}

/// Equivalent noise bandwidth of the 48 kHz channel (the DDC's ±18 kHz
/// passband and its transition bands), measured with white noise.
pub const CHANNEL_NOISE_BW_HZ: f64 = 34_000.0;

/// Find up to `max` carriers in `x` (sample rate `fs`) within ±`span_hz` of
/// center. The strongest comes first. A second carrier must be at least
/// `min_sep_hz` away, a narrow line, and within 20 dB of the first; that keeps
/// VOR subcarrier and voice sidebands out.
pub fn acquire(
    x: &[Complex<f32>],
    fs: f64,
    span_hz: f64,
    max: usize,
    min_sep_hz: f64,
) -> Vec<Carrier> {
    let n = x.len().next_power_of_two() / 2;
    if n < 1024 {
        return Vec::new();
    }
    let mut buf: Vec<Complex<f64>> = (0..n)
        .map(|i| {
            let w = 0.5 - 0.5 * (TAU * i as f64 / n as f64).cos();
            let s = x[x.len() - n + i];
            Complex::new(s.re as f64 * w, s.im as f64 * w)
        })
        .collect();
    FftPlanner::new().plan_fft_forward(n).process(&mut buf);
    let bin_hz = fs / n as f64;
    let p: Vec<f64> = buf.iter().map(|c| c.norm_sqr()).collect();
    let freq_of = |k: usize| {
        if k < n / 2 {
            k as f64 * bin_hz
        } else {
            (k as f64 - n as f64) * bin_hz
        }
    };
    let mut cands: Vec<usize> = (0..n).filter(|&k| freq_of(k).abs() <= span_hz).collect();
    if cands.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<f64> = cands.iter().map(|&k| p[k]).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor = sorted[sorted.len() / 2].max(1e-300);
    cands.sort_by(|&a, &b| p[b].partial_cmp(&p[a]).unwrap());

    let mut out: Vec<Carrier> = Vec::new();
    for &k in &cands {
        if out.len() >= max {
            break;
        }
        let db = 10.0 * (p[k] / floor).log10();
        if db < 15.0 {
            break;
        }
        let f = freq_of(k);
        if out.iter().any(|c| (c.freq_hz - f).abs() < min_sep_hz) {
            continue;
        }
        // Local maximum only.
        let l = p[(k + n - 1) % n];
        let r = p[(k + 1) % n];
        if p[k] < l || p[k] < r {
            continue;
        }
        if let Some(first) = out.first() {
            if db < first.peak_db - 20.0 {
                break;
            }
            // A carrier is a narrow line: well above its ±300 Hz neighborhood.
            let w = (300.0 / bin_hz) as usize;
            let mut neigh: Vec<f64> = (1..=w)
                .flat_map(|d| [p[(k + d) % n], p[(k + n - d) % n]])
                .collect();
            neigh.sort_by(|a, b| a.partial_cmp(b).unwrap());
            if 10.0 * (p[k] / neigh[neigh.len() / 2].max(1e-300)).log10() < 18.0 {
                continue;
            }
        }
        // Parabolic interpolation on log power.
        let (a, b, c) = (
            l.max(1e-300).ln(),
            p[k].max(1e-300).ln(),
            r.max(1e-300).ln(),
        );
        let den = a - 2.0 * b + c;
        let delta = if den.abs() > 1e-12 {
            0.5 * (a - c) / den
        } else {
            0.0
        };
        out.push(Carrier {
            freq_hz: f + delta.clamp(-0.5, 0.5) * bin_hz,
            peak_db: db,
        });
    }
    out
}

/// Second-order carrier PLL producing coherent I/Q.
pub struct CarrierPll {
    fs: f64,
    phase: f64,
    /// Loop frequency (rad/sample).
    w: f64,
    kp: f64,
    ki: f64,
    /// Running carrier amplitude estimate (mean of I).
    pub amp: f64,
    /// Running variance of Q (noise power in Q).
    pub q_var: f64,
}

impl CarrierPll {
    /// `freq_hz`: initial carrier offset; `bn_hz`: loop noise bandwidth.
    pub fn new(fs: f64, freq_hz: f64, bn_hz: f64) -> Self {
        let zeta = 0.707;
        let wn = 2.0 * bn_hz / (zeta + 1.0 / (4.0 * zeta));
        let t = 1.0 / fs;
        Self {
            fs,
            phase: 0.0,
            w: TAU * freq_hz / fs,
            kp: 2.0 * zeta * wn * t,
            ki: (wn * t) * (wn * t),
            amp: 0.0,
            q_var: 0.0,
        }
    }

    pub fn freq_hz(&self) -> f64 {
        self.w * self.fs / TAU
    }

    /// Carrier-to-noise density from the tracked amplitude and Q noise (dB-Hz).
    pub fn cn0_dbhz(&self) -> f64 {
        // Complex AWGN of total variance σ² has σ²/2 in Q, and the noise
        // reaching the PLL fills only the channel filter's noise bandwidth:
        // N0 = σ²/B.
        let n0 = 2.0 * self.q_var / CHANNEL_NOISE_BW_HZ.min(self.fs);
        if n0 <= 0.0 || self.amp <= 0.0 {
            return f64::NAN;
        }
        10.0 * (self.amp * self.amp / n0).log10()
    }

    /// Track `x`, appending the in-phase component to `i_out`.
    pub fn process(&mut self, x: &[Complex<f32>], i_out: &mut Vec<f32>) {
        const AMP_ALPHA: f64 = 1.0 / 48_000.0;
        for &s in x {
            let (sn, cs) = self.phase.sin_cos();
            let z = Complex::new(s.re as f64, s.im as f64) * Complex::new(cs, -sn);
            let err = z.im.atan2(z.re);
            self.w += self.ki * err;
            self.phase = (self.phase + self.w + self.kp * err) % TAU;
            self.amp += AMP_ALPHA * (z.re - self.amp);
            self.q_var += AMP_ALPHA * (z.im * z.im - self.q_var);
            i_out.push(z.re as f32);
        }
    }
}
