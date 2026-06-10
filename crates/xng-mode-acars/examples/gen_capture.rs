//! Generate a synthetic multi-channel ACARS IQ capture (cf32) for testing
//! the decode pipeline without hardware:
//!
//! ```bash
//! cargo run -p xng-mode-acars --example gen_capture -- /tmp/acars.cf32
//! xng decode /tmp/acars.cf32 -r 2400000 -c 131.500M --channels 131.550,131.425
//! ```
//!
//! Capture: 2.4 MS/s centered at 131.500 MHz, bursts on 131.550 MHz
//! (+50 kHz) and 131.425 MHz (−75 kHz).

use num_complex::Complex;
use std::io::Write;
use xng_mode_acars::modulate::{burst_iq, FrameSpec};

fn main() -> std::io::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/tmp/acars.cf32".to_owned());
    let fs = 2_400_000.0;

    let burst_a = burst_iq(
        &FrameSpec {
            mode: '2',
            tail: "N471XG",
            ack: None,
            label: "H1",
            block_id: '3',
            msg_num: Some("M42A"),
            flight: Some("XG0042"),
            text: "POSN 4737.2N 12218.1W AT 120000Z",
            etb: false,
        },
        fs,
        50_000.0,
        0.4,
    );
    let burst_b = burst_iq(
        &FrameSpec {
            mode: '2',
            tail: "N818WX",
            ack: Some('2'),
            label: "Q0",
            block_id: 'A',
            msg_num: None,
            flight: None,
            text: "",
            etb: false,
        },
        fs,
        -75_000.0,
        0.35,
    );

    let b_delay = 120_000; // 50 ms in
    let total = burst_a.len().max(burst_b.len() + b_delay) + 48_000;
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); total];
    for (i, s) in burst_a.iter().enumerate() {
        iq[i + 24_000] += s;
    }
    for (i, s) in burst_b.iter().enumerate() {
        iq[i + b_delay] += s;
    }
    // Light deterministic noise.
    let mut state = 0x2545_f491_4f6c_dd1du64;
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
    eprintln!(
        "wrote {} samples ({:.3} s at {} S/s) to {path}",
        iq.len(),
        iq.len() as f64 / fs,
        fs
    );
    Ok(())
}
