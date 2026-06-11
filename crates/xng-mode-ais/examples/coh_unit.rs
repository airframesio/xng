//! Clean-signal coherent unit check with bit diff.
use num_complex::Complex;
use xng_mode_ais::coherent::CoherentDemod;
use xng_mode_ais::modulate::{burst_iq_gmsk, hdlc_bits, wire_bytes_from_message_bits};
use xng_mode_ais::nmea::payload_to_bits;

fn main() {
    let msg = payload_to_bits("177KQJ5000G?tO`K>RA1wUbN0TKH");
    let tx_bits = hdlc_bits(&wire_bytes_from_message_bits(&msg));
    let expect: Vec<u8> = tx_bits[32..].to_vec();
    let mut iq = vec![Complex::new(0.0f32, 0.0); 700];
    let cfo: f64 = std::env::args().nth(1).map(|a| a.parse().unwrap()).unwrap_or(0.0);
    iq.extend(burst_iq_gmsk(&msg, 48_000.0, cfo, 0.5));
    iq.extend(vec![Complex::new(0.0f32, 0.0); 3000]);
    // Also synthesize at 96k and decode through the DDC path (the
    // off-air route) with the full decoder.
    let mut iq96 = vec![Complex::new(0.0f32, 0.0); 1400];
    iq96.extend(burst_iq_gmsk(&msg, 96_000.0, cfo + 25_000.0, 0.5));
    iq96.extend(vec![Complex::new(0.0f32, 0.0); 6000]);
    let mut full = xng_mode_ais::AisChannelDecoder::new(96_000.0, 25_000.0, 162_025_000).unwrap();
    let mut nf = 0;
    for chunk in iq96.chunks(512) {
        nf += full.process(chunk).len();
    }
    eprintln!("via DDC at 96k: {nf} frame(s)");

    let mut dec = CoherentDemod::new(48_000.0);
    let mut got = Vec::new();
    for chunk in iq.chunks(512) {
        got.extend(dec.process(chunk));
    }
    eprintln!("bursts: {}", got.len());
    for bits in &got {
        let n = bits.iter().zip(&expect).filter(|(a, b)| a == b).count();
        let total = expect.len().min(bits.len());
        eprintln!("  {n}/{total} bits match; first 16 got {:?} want {:?}",
            &bits[..16.min(bits.len())], &expect[..16]);
    }
}
