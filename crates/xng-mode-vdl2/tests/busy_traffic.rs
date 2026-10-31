//! Busy-channel regression: back-to-back bursts (5 ms gaps) on one channel.
//!
//! A false UW lock on the decaying tail of a burst occasionally passes the
//! header FEC and an RS miscorrection. Trusting that fake burst's declared
//! length skipped the real bursts inside it; the demod must instead require
//! an FCS-valid AVLC frame before skipping (PERF-17). Without that guard these
//! fixed seeds lose bursts.

use num_complex::Complex;
use std::collections::BTreeSet;
use xng_mode_vdl2::avlc::{encode_address, AddressType};
use xng_mode_vdl2::modulate::burst_iq_shaped;
use xng_mode_vdl2::Vdl2ChannelDecoder;

const RATE: f64 = 100_000.0;
const BURSTS: usize = 300;
const GAP: usize = 500; // 5 ms
const AMPS: [f32; 2] = [0.4, 0.1];
/// Per-component noise σ: ≈ 31 dB in-channel SNR for the weak bursts.
const SIGMA: f32 = 0.005;

/// Downlink ACARS-over-AVLC UI frame, unique per sequence number (same shape
/// as examples/gen_vdl2_capture.rs).
fn aoa_frame(seq: usize) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend(encode_address(AddressType::GroundIcao, 0x10A234, false, false));
    f.extend(encode_address(AddressType::Aircraft, 0xA0_0000, false, true));
    f.push(0x03); // UI
    f.push(0xFF); // AOA IPI
    f.extend(xng_acars::block::build(
        '2',
        "N700XG",
        None,
        "H1",
        '3',
        Some(&format!("M{:02}A", seq % 100)),
        Some("XG0700"),
        &text(seq),
        false,
    ));
    f
}

fn text(seq: usize) -> String {
    format!("CH0 SEQ {seq:04} POSN 4737.2N 12218.1W AT 120000Z")
}

/// Fixed-seed Gaussian noise (xorshift64 + Box–Muller).
struct Awgn(u64);
impl Awgn {
    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn next(&mut self) -> f32 {
        let (a, b) = (self.uniform(), self.uniform());
        ((-2.0 * a.ln()).sqrt() * (std::f64::consts::TAU * b).cos()) as f32
    }
}

#[test]
fn busy_channel_decodes_every_burst() {
    let mut clean: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); 2_000];
    for seq in 0..BURSTS {
        clean.extend(burst_iq_shaped(&[aoa_frame(seq)], RATE, 0.0, AMPS[seq % AMPS.len()]));
        clean.extend(std::iter::repeat_n(Complex::new(0.0, 0.0), GAP));
    }
    clean.extend(std::iter::repeat_n(Complex::new(0.0, 0.0), (RATE * 0.5) as usize));

    for seed in [8u64, 9, 12] {
        let mut noise = Awgn(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let iq: Vec<Complex<f32>> =
            clean.iter().map(|s| s + Complex::new(noise.next() * SIGMA, noise.next() * SIGMA)).collect();
        let mut dec = Vdl2ChannelDecoder::new(RATE, 0.0).unwrap();
        let mut got = BTreeSet::new();
        for chunk in iq.chunks(8192) {
            for f in dec.process(chunk) {
                if let Some(a) = &f.acars {
                    got.insert(a.core.text.clone());
                }
            }
        }
        let missing: Vec<usize> = (0..BURSTS).filter(|&s| !got.contains(&text(s))).collect();
        assert!(missing.is_empty(), "seed {seed}: lost bursts {missing:?}");
    }
}
