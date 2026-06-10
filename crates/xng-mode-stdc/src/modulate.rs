//! STD-C modulator for loopback testing: frame symbols → pulse-shaped
//! BPSK IQ. Real transmitters shape with RC α=0.6; rectangular pulses
//! would push sidelobe energy through the coherent demod's narrow filter
//! as data-dependent ISI, so the baseband is lowpass-shaped here before
//! the carrier offset is applied (at low rates where it is affordable).

use num_complex::Complex;
use std::f64::consts::TAU;
use xng_dsp::{lowpass_taps, Fir};

pub fn modulate(
    symbols: &[u8],
    symbol_rate: f64,
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let spb = sample_rate / symbol_rate;
    // Rectangular baseband first.
    let mut base = Vec::with_capacity((symbols.len() as f64 * spb) as usize + 1);
    let mut emitted = 0usize;
    for (i, &s) in symbols.iter().enumerate() {
        let bipolar = if s == 1 { 1.0f32 } else { -1.0 };
        let end = (((i + 1) as f64) * spb).round() as usize;
        while emitted < end {
            base.push(Complex::new(bipolar, 0.0));
            emitted += 1;
        }
    }
    // Pulse shaping (skipped at wideband rates where the per-sample cost
    // explodes and the receive DDC bandlimits anyway).
    let shaped = if sample_rate <= 96_000.0 {
        // Windowed sinc at half the symbol rate = Nyquist pulse (zero
        // ISI at symbol centers), approximating the real RC shaping.
        let mut f = Fir::new(lowpass_taps(0.5 * symbol_rate / sample_rate, 161));
        let mut out = Vec::with_capacity(base.len());
        f.process(&base, &mut out);
        out
    } else {
        base
    };
    shaped
        .into_iter()
        .enumerate()
        .map(|(n, s)| {
            let ph = TAU * freq_offset_hz * n as f64 / sample_rate;
            Complex::new(ph.cos() as f32, ph.sin() as f32) * s * amplitude
        })
        .collect()
}
