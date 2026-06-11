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

/// Like `burst_iq`, but with the Annex 10 pulse shaping: each D8PSK
/// symbol point is carried on a full raised-cosine pulse (α = 0.6),
/// i.e. linear modulation s(t) = Σ e^{jφ_k}·h(t−kT). RC is a Nyquist
/// pulse, so symbol-center samples are ISI-free — but inter-symbol
/// samples follow the real off-air trajectory, unlike the rectangular
/// test modulator. Use this for any receive-filter experiment.
pub fn burst_iq_shaped(
    frames: &[Vec<u8>],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let rs = interleave::vdl2_rs();
    let phases = burst_phases(frames, &rs);
    let sps = sample_rate / SYMBOL_RATE;
    const ALPHA: f64 = 0.6;
    const SPAN: f64 = 6.0; // pulse support in symbols, each side

    // Raised-cosine pulse h(t/T): 1 at 0, zeros at nonzero integers.
    let rc = |t: f64| -> f64 {
        let denom = 1.0 - (2.0 * ALPHA * t) * (2.0 * ALPHA * t);
        let sinc = if t.abs() < 1e-12 { 1.0 } else { (PI * t).sin() / (PI * t) };
        if denom.abs() < 1e-9 {
            // t = ±T/(2α): limit α/2·sinc(1/(2α))·π/... use l'Hôpital form
            ALPHA / 2.0 * (PI / (2.0 * ALPHA)).sin()
        } else {
            sinc * (PI * ALPHA * t).cos() / denom
        }
    };

    let nsamples = ((phases.len() as f64 + 2.0 * SPAN) * sps).ceil() as usize;
    (0..nsamples)
        .map(|n| {
            let t_sym = n as f64 / sps - SPAN; // time in symbols
            let lo = ((t_sym - SPAN).ceil().max(0.0)) as usize;
            let hi = ((t_sym + SPAN).floor()).min(phases.len() as f64 - 1.0) as usize;
            let mut acc = Complex::new(0.0f64, 0.0);
            for (k, &phk) in phases.iter().enumerate().take(hi + 1).skip(lo) {
                let w = rc(t_sym - k as f64);
                acc += Complex::new(phk.cos(), phk.sin()) * w;
            }
            let rot = TAU * freq_offset_hz * n as f64 / sample_rate;
            let r = Complex::new(rot.cos(), rot.sin());
            let v = acc * r;
            Complex::new(v.re as f32, v.im as f32) * amplitude
        })
        .collect()
}

/// Cumulative D8PSK phase per symbol (ramp-up + UW + scrambled data).
fn burst_phases(frames: &[Vec<u8>], rs: &ReedSolomon) -> Vec<f64> {
    let avlc_bits = crate::avlc::build(frames);
    let tl = avlc_bits.len() as u32;
    let tx = interleave::interleave(&avlc_bits, rs);

    let mut bits: Vec<u8> = header::encode(tl).to_vec();
    bits.extend(tx);
    Scrambler::new().apply(&mut bits);
    while bits.len() % 3 != 0 {
        bits.push(0);
    }
    let mut deltas: Vec<u8> = vec![0; 5];
    deltas.extend(UW_DELTAS);
    for t in bits.chunks_exact(3) {
        let idx = (t[0] | (t[1] << 1) | (t[2] << 2)) as usize;
        deltas.push(GRAY_FWD[idx]);
    }
    let mut phases = Vec::with_capacity(deltas.len());
    let mut ph = 0.0f64;
    for &d in &deltas {
        ph += d as f64 * PI / 4.0;
        phases.push(ph);
    }
    phases
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
