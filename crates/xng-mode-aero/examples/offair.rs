//! Off-air validation harness: feed a raw mono f32le audio capture (e.g.
//! JAERO's sample recordings, decoded with ffmpeg) through the Aero
//! decoder and print everything that comes out.
//!
//!   ffmpeg -i 600bps_sample.ogg -f f32le -ac 1 600bps.f32
//!   cargo run -p xng-mode-aero --example offair -- 600bps.f32 48000 1626

use num_complex::Complex;
use xng_mode_aero::AeroChannelDecoder;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: offair <f32le file> <rate> <center_hz>");
    let rate: f64 = args.next().expect("rate").parse().expect("rate");
    let center: f64 = args.next().expect("center_hz").parse().expect("center_hz");

    let raw = std::fs::read(&path).expect("read capture");
    let samples: Vec<Complex<f32>> = raw
        .chunks_exact(4)
        .map(|b| Complex::new(f32::from_le_bytes([b[0], b[1], b[2], b[3]]), 0.0))
        .collect();
    eprintln!("{}: {} samples ({:.1}s at {rate} Hz), tuning {center} Hz", path, samples.len(), samples.len() as f64 / rate);

    let mut dec = AeroChannelDecoder::new(rate, center).expect("decoder");
    let mut n_events = 0usize;
    let mut n_acars = 0usize;
    for chunk in samples.chunks(65_536) {
        for e in dec.process(chunk) {
            n_events += 1;
            match &e.acars {
                Some(b) => {
                    n_acars += 1;
                    println!(
                        "[{} bps] ACARS crc_ok={} tail={:?} label={} flight={:?} text={:?}",
                        e.bit_rate,
                        b.crc_ok,
                        b.core.tail,
                        b.core.label,
                        b.core.flight,
                        b.core.text.chars().take(60).collect::<String>(),
                    );
                }
                None => println!(
                    "[{} bps] user data {} bytes: {:02X?}",
                    e.bit_rate,
                    e.user.data.len(),
                    &e.user.data[..e.user.data.len().min(24)]
                ),
            }
        }
    }
    println!("total: {n_events} events, {n_acars} ACARS");
}
