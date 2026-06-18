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
use xng_dsp::Fir;

/// Inmarsat STD-C RRC roll-off factor (α). IEC 61097-4 specifies 0.6.
pub const RRC_BETA: f64 = 0.6;

/// Root-raised-cosine taps (unit energy), `sps` samples per symbol,
/// roll-off `beta`. Standard textbook RRC (Proakis §9.2). Kept local to
/// this crate because `xng_dsp` does not yet expose an RRC helper; see the
/// `shared_needs` note to promote a single `xng_dsp::rrc_taps`.
pub fn rrc_taps(sps: f64, num_taps: usize, beta: f64) -> Vec<f32> {
    let mid = (num_taps - 1) as f64 / 2.0;
    let mut taps: Vec<f64> = (0..num_taps)
        .map(|n| {
            let t = (n as f64 - mid) / sps; // in symbols
            if t.abs() < 1e-9 {
                1.0 - beta + 4.0 * beta / std::f64::consts::PI
            } else if (t.abs() - 1.0 / (4.0 * beta)).abs() < 1e-9 {
                (beta / std::f64::consts::SQRT_2)
                    * ((1.0 + 2.0 / std::f64::consts::PI)
                        * (std::f64::consts::PI / (4.0 * beta)).sin()
                        + (1.0 - 2.0 / std::f64::consts::PI)
                            * (std::f64::consts::PI / (4.0 * beta)).cos())
            } else {
                let pt = std::f64::consts::PI * t;
                ((pt * (1.0 - beta)).sin() + 4.0 * beta * t * (pt * (1.0 + beta)).cos())
                    / (pt * (1.0 - (4.0 * beta * t).powi(2)))
            }
        })
        .collect();
    let energy: f64 = taps.iter().map(|h| h * h).sum::<f64>().sqrt();
    taps.iter_mut().for_each(|h| *h /= energy);
    taps.into_iter().map(|h| h as f32).collect()
}

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
        let mut f = Fir::new(rrc_taps(spb, 161, RRC_BETA));
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
