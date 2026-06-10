//! Off-air validation harness: feed a raw interleaved-f32 IQ capture
//! through the STD-C decoder and print every packet.
//!
//!   ffmpeg -i "Inmarsat-C TDM EGC.wav" -f f32le -ac 2 stdc_48k.f32
//!   cargo run -p xng-mode-stdc --example offair -- stdc_48k.f32 48000 216

use num_complex::Complex;
use xng_mode_stdc::StdcChannelDecoder;

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

    let mut dec = StdcChannelDecoder::new(rate, offset).expect("decoder");
    let mut n = 0usize;
    for chunk in samples.chunks(65_536) {
        for p in dec.process(chunk) {
            n += 1;
            println!("{}: {}", p.name, serde_json::to_string(&p.details).unwrap_or_default());
            if let Some(text) = &p.text {
                println!("  text: {:?}", text.chars().take(100).collect::<String>());
            }
        }
    }
    println!("total: {n} packets");
}
