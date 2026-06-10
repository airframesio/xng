//! Mode S PPM modulator for loopback testing.

use num_complex::Complex;

/// Render a frame as PPM IQ (constant phase, amplitude keying) at
/// `samples_per_us` (even). Includes the 8 µs preamble.
pub fn frame_iq(bytes: &[u8], samples_per_us: usize, amplitude: f32) -> Vec<Complex<f32>> {
    assert!(samples_per_us >= 2 && samples_per_us % 2 == 0);
    let half = samples_per_us / 2;
    let nbits = bytes.len() * 8;
    let mut env = vec![0.0f32; (8 + nbits) * samples_per_us];

    // Preamble pulses at 0, 1.0, 3.5, 4.5 µs.
    for &p in &[0usize, 2, 7, 9] {
        for s in 0..half {
            env[p * half + s] = 1.0;
        }
    }
    // Data: pulse in first half of the cell = 1, second half = 0.
    for k in 0..nbits {
        let bit = (bytes[k / 8] >> (7 - k % 8)) & 1;
        let cell = (8 + k) * samples_per_us;
        let start = if bit == 1 { cell } else { cell + half };
        for s in 0..half {
            env[start + s] = 1.0;
        }
    }

    env.into_iter().map(|e| Complex::new(e * amplitude, 0.0)).collect()
}
