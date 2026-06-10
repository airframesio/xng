//! Window functions for filter design.

use std::f64::consts::PI;

/// Hamming window of length `n`.
pub fn hamming(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 0.54 - 0.46 * (2.0 * PI * i as f64 / (n as f64 - 1.0)).cos())
        .collect()
}

/// 4-term Blackman-Harris window of length `n` (high stopband attenuation,
/// the usual choice for channelizer prototype filters).
pub fn blackman_harris(n: usize) -> Vec<f64> {
    const A: [f64; 4] = [0.35875, 0.48829, 0.14128, 0.01168];
    (0..n)
        .map(|i| {
            let x = 2.0 * PI * i as f64 / (n as f64 - 1.0);
            A[0] - A[1] * x.cos() + A[2] * (2.0 * x).cos() - A[3] * (3.0 * x).cos()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_symmetric_and_positive_peak() {
        for w in [hamming(64), blackman_harris(64)] {
            let n = w.len();
            for i in 0..n / 2 {
                assert!((w[i] - w[n - 1 - i]).abs() < 1e-12);
            }
            let peak = w.iter().cloned().fold(f64::MIN, f64::max);
            assert!(peak > 0.9 && peak <= 1.0 + 1e-9);
        }
    }
}
