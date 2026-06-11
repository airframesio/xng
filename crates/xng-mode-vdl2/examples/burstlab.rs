//! Single-burst lab: sweep input perturbations (CFO rotation,
//! fractional delay) over one captured burst segment; report any
//! combination that yields an FCS-valid frame. Identifies which
//! parameter the streaming demod mis-estimates on this burst.
use num_complex::Complex;
use xng_mode_vdl2::{avlc, demod::Vdl2Demod, interleave};

fn load(path: &str) -> Vec<Complex<f32>> {
    let raw = std::fs::read(path).expect("read");
    raw.chunks_exact(8)
        .map(|b| {
            Complex::new(
                f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                f32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            )
        })
        .collect()
}

fn frac_delay(x: &[Complex<f32>], d: f32) -> Vec<Complex<f32>> {
    (0..x.len() - 1)
        .map(|i| x[i] * (1.0 - d) + x[i + 1] * d)
        .collect()
}

fn rotate(x: &[Complex<f32>], hz: f32, fs: f32) -> Vec<Complex<f32>> {
    let step = Complex::from_polar(1.0, 2.0 * std::f32::consts::PI * hz / fs);
    let mut r = Complex::new(1.0f32, 0.0);
    x.iter()
        .map(|&s| {
            let y = s * r;
            r *= step;
            y
        })
        .collect()
}

fn main() {
    let path = std::env::args().nth(1).expect("segment file");
    let fs = 105_000.0f32;
    let x = load(&path);
    let rs = interleave::vdl2_rs();
    let mut hits = 0;
    for df in (-12..=12).map(|k| k as f32 * 5.0) {
        for dt in (0..10).map(|k| k as f32 * 0.1) {
            let y = frac_delay(&rotate(&x, df, fs), dt);
            let mut demod = Vdl2Demod::new(fs as f64);
            let bursts = demod.process(&y, &rs);
            for b in &bursts {
                let frames = avlc::scan(&b.bits);
                if !frames.is_empty() {
                    hits += 1;
                    println!(
                        "HIT df={df:+.0}Hz dt={dt:.1} -> {} frame(s), first {:?}->{:?}",
                        frames.len(),
                        frames[0].src.addr,
                        frames[0].dst.addr
                    );
                }
            }
        }
    }
    println!("total parameter hits: {hits} of 250");
}
