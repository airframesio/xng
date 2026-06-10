//! HFDL burst modulator for loopback testing.

use crate::fec::{self, Setting};
use num_complex::Complex;
use std::f64::consts::TAU;
use xng_dsp::viterbi::Viterbi;

pub const SYMBOL_RATE: f64 = 1_800.0;
const PREKEY_SYMBOLS: usize = 448;

/// Inverse Gray: label g → phase position n.
fn gray_inv(g: u32) -> u32 {
    g ^ (g >> 1) ^ (g >> 2)
}

/// Build the burst symbol phase sequence (radians) for a payload.
pub fn burst_symbols(payload: &[u8], s: &Setting) -> Vec<f64> {
    // Payload bytes → bits LSB-first, zero-padded to payload_bits.
    let mut bits: Vec<u8> =
        payload.iter().flat_map(|&b| (0..8).map(move |i| (b >> i) & 1)).collect();
    assert!(bits.len() <= s.payload_bits());
    bits.resize(s.payload_bits(), 0);

    let mut chips = Viterbi::new(7, 0o133, 0o171).encode(&bits); // 133-first, as on air
    if s.rate_quarter {
        chips = chips.iter().flat_map(|&c| [c, c]).collect();
    }
    let air = fec::interleave(&chips, s);

    let a = fec::bits_of(fec::A_BITS);
    let m = fec::bits_of(fec::M_BITS);
    let t = fec::bits_of(fec::T_BITS);
    let bpsk = |b: u8| if b == 1 { std::f64::consts::PI } else { 0.0 };

    let mut sym: Vec<f64> = Vec::new();
    sym.extend(std::iter::repeat(0.0).take(PREKEY_SYMBOLS));
    for _ in 0..2 {
        sym.extend(a.iter().map(|&b| bpsk(b)));
    }
    for j in 0..127 + 15 {
        sym.push(bpsk(m[(s.m1_shift + j) % 127]));
    }
    for _ in 0..9 {
        sym.extend(t.iter().map(|&b| bpsk(b)));
    }

    let flips = fec::scramble_flips(s.data_segments() * 30);
    let bps = s.bps_per_sym as usize;
    let mut chip_idx = 0;
    let mut data_sym_idx = 0;
    for _ in 0..s.data_segments() {
        for _ in 0..30 {
            let mut label = 0u32;
            for _ in 0..bps {
                label = (label << 1) | air[chip_idx] as u32; // MSB first
                chip_idx += 1;
            }
            let pos = gray_inv(label);
            let mut phase = TAU * pos as f64 / (1 << bps) as f64;
            if flips[data_sym_idx] == 1 {
                phase += std::f64::consts::PI;
            }
            sym.push(phase);
            data_sym_idx += 1;
        }
        sym.extend(t.iter().map(|&b| bpsk(b)));
    }
    sym
}

/// Render symbols as IQ at `sample_rate` with a carrier offset.
pub fn modulate(symbols: &[f64], sample_rate: f64, freq_offset_hz: f64, amplitude: f32) -> Vec<Complex<f32>> {
    let sps = sample_rate / SYMBOL_RATE;
    let n = (symbols.len() as f64 * sps).ceil() as usize;
    (0..n)
        .map(|k| {
            let sym = ((k as f64 / sps) as usize).min(symbols.len() - 1);
            let ph = symbols[sym] + TAU * freq_offset_hz * k as f64 / sample_rate;
            Complex::new(ph.cos() as f32, ph.sin() as f32) * amplitude
        })
        .collect()
}
