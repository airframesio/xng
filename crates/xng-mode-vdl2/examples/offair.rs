//! Off-air validation harness: feed a raw interleaved-f32 IQ capture
//! through the VDL2 decoder and print every AVLC frame.
//!
//!   ffmpeg -i "VDL2 IQ.wav" -f f32le -ac 2 -ar 50000 vdl2_50k.f32
//!   cargo run -p xng-mode-vdl2 --example offair -- vdl2_50k.f32 50000 0

use num_complex::Complex;
use xng_mode_vdl2::Vdl2ChannelDecoder;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: offair <f32le IQ> <rate> <offset_hz>");
    let rate: f64 = args.next().expect("rate").parse().expect("rate");
    let offset: f64 = args.next().expect("offset").parse().expect("offset");

    let raw = std::fs::read(&path).expect("read capture");
    let samples: Vec<Complex<f32>> = raw
        .chunks_exact(8)
        .map(|b| {
            Complex::new(
                f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                f32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            )
        })
        .collect();
    eprintln!(
        "{}: {} IQ samples ({:.1}s at {rate} Hz), offset {offset} Hz",
        path,
        samples.len(),
        samples.len() as f64 / rate
    );

    let mut dec = Vdl2ChannelDecoder::new(rate, offset).expect("decoder");
    let mut n = 0usize;
    for chunk in samples.chunks(65_536) {
        for f in dec.process(chunk) {
            n += 1;
            match &f.acars {
                Some(b) => println!(
                    "ACARS crc_ok={} tail={:?} label={} text={:?}",
                    b.crc_ok,
                    b.core.tail,
                    b.core.label,
                    b.core.text.chars().take(60).collect::<String>()
                ),
                None => println!(
                    "AVLC {:?} -> {:?} ctrl={:?} len={}",
                    f.avlc.src, f.avlc.dst, f.avlc.control, f.avlc.raw.len()
                ),
            }
        }
    }
    println!("total: {n} frames");
    use std::sync::atomic::Ordering;
    eprintln!(
        "stats: fit_pass={} hdr_fail={} rs_fail={} burst_ok={}",
        xng_mode_vdl2::demod::STAT_FIT_PASS.load(Ordering::Relaxed),
        xng_mode_vdl2::demod::STAT_HDR_FAIL.load(Ordering::Relaxed),
        xng_mode_vdl2::demod::STAT_RS_FAIL.load(Ordering::Relaxed),
        xng_mode_vdl2::demod::STAT_BURST_OK.load(Ordering::Relaxed),
    );
}
