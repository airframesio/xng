//! Demodulate an IQ capture and print each burst's bit stream (for
//! cross-validation against gr-iridium's generated test data).

use num_complex::Complex;
use xng_mode_iridium::{demod::IridiumDemod, CHANNEL_RATE};
use xng_dsp::Ddc;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: dumpbits <cf32> <rate> <offset>");
    let rate: f64 = args.next().unwrap().parse().unwrap();
    let offset: f64 = args.next().unwrap().parse().unwrap();

    let raw = std::fs::read(&path).expect("read");
    let samples: Vec<Complex<f32>> = raw
        .chunks_exact(8)
        .map(|b| {
            Complex::new(
                f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                f32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            )
        })
        .collect();
    eprintln!("{} samples ({:.3}s)", samples.len(), samples.len() as f64 / rate);

    let mut chan: Vec<Complex<f32>> = Vec::new();
    let channel: Vec<Complex<f32>> = if (rate - CHANNEL_RATE).abs() < 1e-6 && offset.abs() < 1e-9 {
        samples
    } else {
        let mut ddc = Ddc::new(rate, CHANNEL_RATE, offset, 25_000.0).unwrap();
        ddc.process(&samples, &mut chan);
        chan
    };

    let mut demod = IridiumDemod::new(CHANNEL_RATE);
    // Flush: the hunt needs max-burst lookahead beyond the last burst.
    let mut channel = channel;
    channel.extend(std::iter::repeat(Complex::new(0.0f32, 0.0)).take((CHANNEL_RATE * 0.15) as usize));
    for burst in demod.process(&channel) {
        let s: String = burst.bits.iter().map(|&b| char::from(b'0' + b)).collect();
        println!("{s}");
    }
}
