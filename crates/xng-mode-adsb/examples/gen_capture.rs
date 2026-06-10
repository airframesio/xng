//! Generate a synthetic Mode S IQ capture (cf32) at 2 MS/s:
//!
//! ```bash
//! cargo run -p xng-mode-adsb --example gen_capture -- /tmp/adsb.cf32
//! xng decode /tmp/adsb.cf32 --mode adsb -r 2000000 -c 1090.000M --channels 1090
//! ```

use num_complex::Complex;
use std::io::Write;
use xng_mode_adsb::modulate::frame_iq;

const ID_FRAME: [u8; 14] = [
    0x8D, 0x48, 0x40, 0xD6, 0x20, 0x2C, 0xC3, 0x71, 0xC3, 0x2C, 0xE0, 0x57, 0x60, 0x98,
];
const POS_FRAME: [u8; 14] = [
    0x8D, 0x40, 0x62, 0x1D, 0x58, 0xC3, 0x82, 0xD6, 0x90, 0xC8, 0xAC, 0x28, 0x63, 0xA7,
];

fn main() -> std::io::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/tmp/adsb.cf32".to_owned());
    let spu = 2;

    let mut iq = vec![Complex::new(0.0f32, 0.0f32); 20_000];
    for (frame, amp, gap) in
        [(&ID_FRAME, 0.6f32, 30_000usize), (&POS_FRAME, 0.4, 30_000), (&ID_FRAME, 0.25, 20_000)]
    {
        iq.extend(frame_iq(frame, spu, amp));
        iq.extend(vec![Complex::new(0.0, 0.0); gap]);
    }
    let mut state = 0x0123_4567_89ab_cdefu64;
    for s in &mut iq {
        let mut n = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state as f32 / u64::MAX as f32) * 2.0 - 1.0
        };
        *s += Complex::new(n() * 0.02, n() * 0.02);
    }

    let mut out = std::io::BufWriter::new(std::fs::File::create(&path)?);
    for s in &iq {
        out.write_all(&s.re.to_le_bytes())?;
        out.write_all(&s.im.to_le_bytes())?;
    }
    out.flush()?;
    eprintln!("wrote {} samples to {path}", iq.len());
    Ok(())
}
