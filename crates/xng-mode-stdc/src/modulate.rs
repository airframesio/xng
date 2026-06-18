//! STD-C modulator for loopback testing: frame symbols → pulse-shaped
//! BPSK IQ. Real transmitters shape with RRC α=0.6; rectangular pulses
//! would push sidelobe energy through the coherent demod's narrow filter
//! as data-dependent ISI, so the baseband is RRC-shaped here before
//! the carrier offset is applied (at low rates where it is affordable).
//!
//! RRC (transmit half) is paired with the demod's RRC matched filter so
//! the combined response is a raised-cosine Nyquist pulse (zero ISI at
//! symbol centres) — the textbook matched-filter setup that maximises
//! symbol SNR for a given transmit power (Proakis §9, Inmarsat IEC 61097-4
//! BPSK 1200 sym/s with α=0.6 shaping).

use num_complex::Complex;
use std::f64::consts::TAU;
use xng_dsp::{rrc_taps, Fir};

/// Inmarsat STD-C RRC roll-off factor (α). IEC 61097-4 specifies 0.6.
pub const RRC_BETA: f64 = 0.6;

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
        // Transmit half of the matched-filter pair: RRC(α=0.6) at the
        // configured sps. Paired with the demod's RRC matched filter the
        // combined response is a raised-cosine Nyquist pulse (zero ISI at
        // symbol centres) — the real on-air shaping STD-C uses.
        let mut f = Fir::new(rrc_taps(RRC_BETA, spb, 161));
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
