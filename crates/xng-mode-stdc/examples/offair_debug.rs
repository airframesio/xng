//! Stage diagnostics for STD-C off-air captures.

use num_complex::Complex;
use xng_dsp::Ddc;
use xng_mode_stdc::{demod::BpskDemod, frame, CHANNEL_PASSBAND_HZ, CHANNEL_RATE};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: offair_debug <f32le IQ> <rate> <offset>");
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

    let mut ddc = Ddc::new(rate, CHANNEL_RATE, offset, CHANNEL_PASSBAND_HZ).unwrap();
    let mut chan = Vec::new();
    ddc.process(&samples, &mut chan);
    let pwr: f32 = chan.iter().map(|c| c.norm_sqr()).sum::<f32>() / chan.len() as f32;
    eprintln!("channel: {} samples, {:.1} dBFS", chan.len(), 10.0 * pwr.log10());

    let mut demod = BpskDemod::new(CHANNEL_RATE);
    let mut soft: Vec<f32> = Vec::new();
    for chunk in chan.chunks(65_536) {
        demod.process(chunk, &mut soft);
        let (a, b, c) = demod.debug_state();
        eprintln!("  state: carr_err={a:.3} other={b:.3} {c:.1} locked={}", demod.locked);
    }
    eprintln!("demod: {} symbols ({:.1}s of 1200 sym/s)", soft.len(), soft.len() as f64 / 1200.0);

    // UW scan over hard symbols: STD-C UW = 64 symbol PAIRS doubled per
    // row; frame.uw_score expects a full frame slice. Slide and score.
    let hard: Vec<u8> = soft.iter().map(|&s| (s > 0.0) as u8).collect();
    let mut best = (0u32, 0usize, false);
    for st in 0..hard.len().saturating_sub(frame::FRAME_SYMBOLS) {
        let (score, inv_score) = frame::uw_score(&hard[st..st + frame::FRAME_SYMBOLS]);
        if score > best.0 {
            best = (score, st, false);
        }
        if inv_score > best.0 {
            best = (inv_score, st, true);
        }
    }
    eprintln!("best UW score {}/128 at symbol {} inverted={}", best.0, best.1, best.2);

    // Decode the synced frame and dump bytes + parse attempts.
    let dec = frame::FrameDecoder::new();
    let bytes = dec.decode(&soft[best.1..best.1 + frame::FRAME_SYMBOLS], best.2);
    eprintln!("frame head: {:02X?}", &bytes[..48]);
    let mut parser = xng_mode_stdc::packet::PacketParser::new();
    let pkts = parser.parse_frame(&bytes);
    eprintln!("{} packets from synced frame", pkts.len());
    for p in pkts.iter().take(6) {
        eprintln!("  {} checksum_ok={}", p.name, p.checksum_ok);
    }
}
// appended: frame decode dump
