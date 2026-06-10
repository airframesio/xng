//! Generate a synthetic dual-channel AIS IQ capture (cf32):
//!
//! ```bash
//! cargo run -p xng-mode-ais --example gen_capture -- /tmp/ais.cf32
//! xng decode /tmp/ais.cf32 --mode ais -r 2400000 -c 162.000M --channels 161.975,162.025
//! ```
//!
//! Capture: 2.4 MS/s centered at 162.000 MHz; one burst on channel A
//! (161.975) and one on channel B (162.025, with a 600 Hz carrier offset).

use num_complex::Complex;
use std::io::Write;
use xng_mode_ais::modulate::burst_iq;
use xng_mode_ais::nmea::payload_to_bits;

fn main() -> std::io::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/tmp/ais.cf32".to_owned());
    let fs = 2_400_000.0;

    // Type 1 position report (the canonical gpsd documentation example).
    let msg_a = payload_to_bits("177KQJ5000G?tO`K>RA1wUbN0TKH");
    let mut msg_b = msg_a.clone();
    msg_b[12] ^= 1; // different MMSI
    msg_b[25] ^= 1;

    let burst_a = burst_iq(&msg_a, fs, -25_000.0, 0.4);
    let burst_b = burst_iq(&msg_b, fs, 25_000.0 + 600.0, 0.35);

    let b_delay = 120_000;
    let total = burst_a.len().max(burst_b.len() + b_delay) + 48_000;
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); total];
    for (i, s) in burst_a.iter().enumerate() {
        iq[i + 24_000] += s;
    }
    for (i, s) in burst_b.iter().enumerate() {
        iq[i + b_delay] += s;
    }
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for s in &mut iq {
        let mut n = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state as f32 / u64::MAX as f32) * 2.0 - 1.0
        };
        *s += Complex::new(n() * 0.008, n() * 0.008);
    }

    let mut out = std::io::BufWriter::new(std::fs::File::create(&path)?);
    for s in &iq {
        out.write_all(&s.re.to_le_bytes())?;
        out.write_all(&s.im.to_le_bytes())?;
    }
    out.flush()?;
    eprintln!("wrote {} samples ({:.3} s at {} S/s) to {path}", iq.len(), iq.len() as f64 / fs, fs);
    Ok(())
}
