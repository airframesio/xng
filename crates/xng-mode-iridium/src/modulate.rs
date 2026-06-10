//! Iridium burst modulator (loopback/testing): tone preamble + UW +
//! payload symbols at 25 ksym/s, RRC-shaped DQPSK.

use num_complex::Complex;

/// Root-raised-cosine taps (unit energy).
pub fn rrc_taps(sps: f64, num_taps: usize, beta: f64) -> Vec<f32> {
    let mid = (num_taps - 1) as f64 / 2.0;
    let mut taps: Vec<f64> = (0..num_taps)
        .map(|n| {
            let t = (n as f64 - mid) / sps;
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

/// Inverse of the demod's differential decode: bits (starting at the
/// access code) → absolute QPSK symbols, old_sym starting at 0.
pub fn bits_to_symbols(bits: &[u8]) -> Vec<u8> {
    const INV_MAP: [u8; 4] = [0, 3, 1, 2]; // inverse of [0,2,3,1]
    let mut out = Vec::with_capacity(bits.len() / 2);
    let mut old = 0u8;
    for pair in bits.chunks_exact(2) {
        let m = (pair[0] << 1) | pair[1];
        let d = INV_MAP[m as usize];
        old = (old + d) % 4;
        out.push(old);
    }
    out
}

/// Modulate a burst: `pre_syms` of tone, then the symbol stream,
/// RRC-shaped at `sample_rate` with `freq_offset_hz`.
pub fn modulate(
    bits: &[u8],
    pre_syms: usize,
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let sps = sample_rate / super::demod::SYMBOL_RATE;
    let symbols = bits_to_symbols(bits);
    let total_syms = pre_syms + symbols.len() + 4;
    let total = (total_syms as f64 * sps) as usize;
    let mut i_imp = vec![Complex::new(0.0f32, 0.0); total];
    for k in 0..pre_syms {
        let at = (k as f64 * sps) as usize;
        i_imp[at] = Complex::new(1.0, 0.0);
    }
    for (k, &q) in symbols.iter().enumerate() {
        let at = ((pre_syms + k) as f64 * sps) as usize;
        i_imp[at] = Complex::from_polar(1.0, q as f32 * std::f32::consts::FRAC_PI_2);
    }
    let mut shaped = Vec::new();
    let mut fir = xng_dsp::Fir::new(rrc_taps(sps, 81, 0.4));
    fir.process(&i_imp, &mut shaped);
    shaped
        .iter()
        .enumerate()
        .map(|(n, s)| {
            let ph = std::f64::consts::TAU * freq_offset_hz * n as f64 / sample_rate;
            s * Complex::from_polar(amplitude, ph as f32)
        })
        .collect()
}
