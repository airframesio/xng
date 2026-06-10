//! VDL2 modulator for loopback testing: AVLC frames → stuffed bit stream →
//! RS encode + interleave → header → scramble → D8PSK (rectangular symbol
//! shaping; real signals are RC α=0.6, which symbol-center sampling
//! tolerates).

use crate::demod::{GRAY_FWD, SYMBOL_RATE};
use crate::header;
use crate::interleave;
use crate::scramble::Scrambler;
use num_complex::Complex;
use std::f64::consts::{PI, TAU};
use xng_dsp::rs::ReedSolomon;

const UW_DELTAS: [u8; 16] = [0, 3, 2, 4, 0, 1, 6, 4, 1, 7, 2, 5, 6, 5, 7, 3];

/// Full burst IQ at `sample_rate` for raw AVLC frame octet vectors
/// (addresses + control + info; FCS/flags/stuffing added here).
pub fn burst_iq(
    frames: &[Vec<u8>],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let rs = interleave::vdl2_rs();
    burst_iq_with(frames, &rs, sample_rate, freq_offset_hz, amplitude)
}

pub fn burst_iq_with(
    frames: &[Vec<u8>],
    rs: &ReedSolomon,
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let avlc_bits = crate::avlc::build(frames);
    let tl = avlc_bits.len() as u32;
    let tx = interleave::interleave(&avlc_bits, rs);

    let mut bits: Vec<u8> = header::encode(tl).to_vec();
    bits.extend(tx);
    Scrambler::new().apply(&mut bits);

    // Triplets → Δφ indices (pad with zeros to a whole symbol).
    while bits.len() % 3 != 0 {
        bits.push(0);
    }
    let mut deltas: Vec<u8> = vec![0; 5]; // ramp-up symbols
    deltas.extend(UW_DELTAS);
    for t in bits.chunks_exact(3) {
        let idx = (t[0] | (t[1] << 1) | (t[2] << 2)) as usize;
        deltas.push(GRAY_FWD[idx]);
    }

    // Cumulative phase per symbol.
    let mut phases = Vec::with_capacity(deltas.len());
    let mut ph = 0.0f64;
    for &d in &deltas {
        ph += d as f64 * PI / 4.0;
        phases.push(ph);
    }

    let sps = sample_rate / SYMBOL_RATE;
    let nsamples = (deltas.len() as f64 * sps).ceil() as usize;
    (0..nsamples)
        .map(|n| {
            let sym = ((n as f64 / sps) as usize).min(phases.len() - 1);
            let p = phases[sym] + TAU * freq_offset_hz * n as f64 / sample_rate;
            Complex::new(p.cos() as f32, p.sin() as f32) * amplitude
        })
        .collect()
}
