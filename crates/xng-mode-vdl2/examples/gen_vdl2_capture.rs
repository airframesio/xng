//! Generate a synthetic 7-channel VDL2 IQ capture (cf32) shaped like the live
//! workload: one 2.4 MS/s capture centered at 136.8125 MHz carrying the real
//! VDL2 plan, with RC-shaped D8PSK bursts on every channel.
//!
//! ```bash
//! cargo run --release -p xng-mode-vdl2 --example gen_vdl2_capture -- /tmp/vdl2.cf32 2.0
//! xng decode /tmp/vdl2.cf32 -f cf32 -m vdl2 -r 2400000 -c 136812500 \
//!   --channels 136.650,136.725,136.775,136.825,136.875,136.925,136.975
//! ```
//!
//! Bursts alternate between strong (0.4) and weak (0.1) amplitude. Every burst
//! carries a distinct ACARS message: the runtime drops byte-identical frames
//! seen within a 2.5 s wall-clock window (`DedupFilter`), so repeating one
//! frame would make the decoded count meaningless.

use num_complex::Complex;
use std::io::Write;
use std::sync::mpsc;
use xng_mode_vdl2::avlc::{encode_address, AddressType};
use xng_mode_vdl2::modulate::burst_iq_shaped;

const FS: f64 = 2_400_000.0;
const CENTER_HZ: f64 = 136_812_500.0;
const PLAN_HZ: [f64; 7] = [
    136_650_000.0,
    136_725_000.0,
    136_775_000.0,
    136_825_000.0,
    136_875_000.0,
    136_925_000.0,
    136_975_000.0,
];
const STAGGER: usize = 20_000;
const GAP: usize = 50_000;
const AMPS: [f32; 2] = [0.4, 0.1];
/// Idle (noise-only) tail after the last burst. The decoder can currently
/// hold a burst back until more samples arrive (observed, under
/// investigation), so ending on idle keeps decoded-vs-injected comparable.
const TAIL_S: f64 = 0.5;

/// Downlink ACARS-over-AVLC UI frame, unique per (channel, sequence).
fn aoa_frame(chan: usize, seq: usize) -> Vec<u8> {
    let tail = format!("N70{chan}XG");
    let flight = format!("XG070{chan}");
    let msg_num = format!("M{:02}A", seq % 100);
    let text = format!("CH{chan} SEQ {seq:04} POSN 4737.2N 12218.1W AT 120000Z");
    let mut f = Vec::new();
    f.extend(encode_address(
        AddressType::GroundIcao,
        0x10A234,
        false,
        false,
    ));
    f.extend(encode_address(
        AddressType::Aircraft,
        0xA0_0000 + chan as u32,
        false,
        true,
    ));
    f.push(0x03); // UI
    f.push(0xFF); // AOA IPI
    f.extend(xng_acars::block::build(
        '2',
        &tail,
        None,
        "H1",
        '3',
        Some(&msg_num),
        Some(&flight),
        &text,
        false,
    ));
    f
}

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "/tmp/vdl2.cf32".to_owned());
    let seconds: f64 = match args.next() {
        Some(s) => s.parse().expect("seconds must be a number"),
        None => 2.0,
    };
    let n = (FS * seconds) as usize;
    let last_end = n.saturating_sub((TAIL_S * FS) as usize);

    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut nx = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state as f32 / u64::MAX as f32) * 2.0 - 1.0
    };
    let mut iq: Vec<Complex<f32>> = (0..n)
        .map(|_| Complex::new(nx() * 0.01, nx() * 0.01))
        .collect();

    // Shaped modulation at 2.4 MS/s is slow and every burst must be unique,
    // so synthesize channels on worker threads and stream each placed burst
    // back (bounded queue keeps memory to a handful of bursts).
    let (tx, rx) = mpsc::sync_channel::<(usize, usize, Vec<Complex<f32>>)>(16);
    let mut per_chan = [0usize; PLAN_HZ.len()];
    std::thread::scope(|s| {
        for (c, &f) in PLAN_HZ.iter().enumerate() {
            let tx = tx.clone();
            s.spawn(move || {
                let off = f - CENTER_HZ;
                let mut at = c * STAGGER;
                let mut seq = 0;
                loop {
                    let amp = AMPS[seq % AMPS.len()];
                    let burst = burst_iq_shaped(&[aoa_frame(c, seq)], FS, off, amp);
                    if at + burst.len() > last_end {
                        break;
                    }
                    let len = burst.len();
                    if tx.send((c, at, burst)).is_err() {
                        break;
                    }
                    at += len + GAP;
                    seq += 1;
                }
            });
        }
        drop(tx);
        for (c, at, burst) in rx {
            for (d, s) in iq[at..at + burst.len()].iter_mut().zip(&burst) {
                *d += s;
            }
            per_chan[c] += 1;
        }
    });

    let mut out = std::io::BufWriter::new(std::fs::File::create(&path)?);
    for s in &iq {
        out.write_all(&s.re.to_le_bytes())?;
        out.write_all(&s.im.to_le_bytes())?;
    }
    out.flush()?;
    eprintln!(
        "wrote {} samples ({:.3} s at {} S/s) to {path}",
        n,
        n as f64 / FS,
        FS
    );
    eprintln!("idle tail: bursts end at least {TAIL_S} s before the end of the capture");
    for (c, &f) in PLAN_HZ.iter().enumerate() {
        eprintln!(
            "  {:.3} MHz ({:+.0} Hz): {} bursts",
            f / 1e6,
            f - CENTER_HZ,
            per_chan[c]
        );
    }
    eprintln!(
        "total bursts injected (expected frames): {}",
        per_chan.iter().sum::<usize>()
    );
    Ok(())
}
