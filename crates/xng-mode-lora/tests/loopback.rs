use num_complex::Complex;
use xng_mode_lora::coding::CodingParams;
use xng_mode_lora::demod::{Demodulator, RxParams};
use xng_mode_lora::modulate::{modulate, TxParams};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f64 {
        let u = self.next().max(1e-12);
        let v = self.next();
        (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    sf: u8,
    cr: u8,
    bw: f64,
    sync: u8,
    cfo_hz: f64,
    snr_db: f64,
    lead: usize,
    payload: &[u8],
) -> Vec<xng_mode_lora::demod::LoraFrame> {
    let ldro = (1u64 << sf) as f64 / bw > 0.016;
    let tx = TxParams {
        coding: CodingParams { sf, cr, ldro },
        bw_hz: bw,
        preamble: 16,
        sync_word: sync,
    };
    let sig = modulate(payload, &tx, 2, cfo_hz, cfo_hz / 906.875e6 * 1e6);
    let mut rng = Rng(0x1234_5678_9abc_def1 ^ sf as u64);
    let sigma = (10f64.powf(-snr_db / 10.0) / 2.0).sqrt();
    let mut x: Vec<Complex<f32>> = Vec::new();
    for _ in 0..lead {
        x.push(Complex::new(
            (sigma * rng.gauss()) as f32,
            (sigma * rng.gauss()) as f32,
        ));
    }
    x.extend(
        sig.iter()
            .map(|s| s + Complex::new((sigma * rng.gauss()) as f32, (sigma * rng.gauss()) as f32)),
    );
    for _ in 0..(8usize << sf) * 2 {
        x.push(Complex::new(
            (sigma * rng.gauss()) as f32,
            (sigma * rng.gauss()) as f32,
        ));
    }
    let mut d = Demodulator::new(RxParams {
        sf,
        bw_hz: bw,
        carrier_hz: 906.875e6,
        ldro: None,
    });
    let mut out = Vec::new();
    for chunk in x.chunks(3001) {
        out.extend(d.process(chunk));
    }
    out
}

#[test]
fn clean_frames_all_sf() {
    let payload: Vec<u8> = (0..40u8)
        .map(|i| i.wrapping_mul(29).wrapping_add(3))
        .collect();
    for sf in 7..=12 {
        for &lead in &[0usize, 777, 5003] {
            let f = run(sf, 1, 250e3, 0x2B, 0.0, 30.0, lead, &payload);
            assert_eq!(f.len(), 1, "sf{sf} lead{lead}");
            assert_eq!(f[0].payload, payload, "sf{sf} lead{lead}");
            assert_eq!(f[0].crc_ok, Some(true));
            assert_eq!(f[0].sync_word, 0x2B);
        }
    }
}

#[test]
fn carrier_offset_and_noise() {
    let payload = b"hello mesh, this is xng".to_vec();
    for (sf, cfo) in [
        (7u8, 12_000.0),
        (9, -9_000.0),
        (11, 3_210.0),
        (11, -20_500.0),
        (12, 7_777.0),
    ] {
        let f = run(sf, 4, 250e3, 0x12, cfo, 0.0, 1234, &payload);
        assert_eq!(f.len(), 1, "sf{sf} cfo{cfo}");
        assert_eq!(f[0].payload, payload, "sf{sf} cfo{cfo}");
        assert_eq!(f[0].crc_ok, Some(true));
        assert_eq!(f[0].sync_word, 0x12);
        assert!(
            (f[0].cfo_hz as f64 - cfo).abs() < 300.0,
            "cfo est {} vs {cfo}",
            f[0].cfo_hz
        );
    }
}

#[test]
fn below_noise_sf11() {
    let payload: Vec<u8> = (0..60u8).collect();
    let f = run(11, 1, 250e3, 0x2B, 1_500.0, -12.0, 4321, &payload);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].crc_ok, Some(true));
}

#[test]
fn ldro_narrow() {
    // SF12 at 62.5 kHz: 65 ms symbols, LDRO on.
    let payload = b"narrow and slow".to_vec();
    let f = run(12, 4, 62.5e3, 0x12, 900.0, 5.0, 999, &payload);
    assert_eq!(f.len(), 1);
    assert!(f[0].ldro);
    assert_eq!(f[0].payload, payload);
}

/// Long packets at carrier offsets whose clock drift walks the chirp wraps
/// across the sample grid (a modulator that mis-integrated the wrap once
/// failed these).
#[test]
fn long_packets_with_drift() {
    for &(sf, bw) in &[(11u8, 250e3), (7, 62.5e3), (12, 125e3)] {
        for len in [26usize, 46, 70, 120] {
            let payload: Vec<u8> = (0..len as u32).map(|i| (i * 73 + len as u32 * 11) as u8).collect();
            for &cfo in &[1800.0, -2500.0] {
                let f = run(sf, 1, bw, 0x2B, cfo, 25.0, 1234, &payload);
                assert_eq!(f.len(), 1, "sf{sf} len{len} cfo{cfo}");
                assert_eq!(f[0].crc_ok, Some(true), "sf{sf} len{len} cfo{cfo}");
                assert_eq!(f[0].payload, payload);
            }
        }
    }
}
