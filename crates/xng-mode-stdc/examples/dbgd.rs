use num_complex::Complex;
use xng_mode_stdc::frame::{encode_frame, uw_score, FRAME_SYMBOLS, UW_MIN_MATCH};
use xng_mode_stdc::modulate::modulate;
use xng_mode_stdc::packet::build_packet;
use xng_mode_stdc::demod::BpskDemod;
use xng_mode_stdc::CHANNEL_RATE;

fn egc_packet(text: &[u8]) -> Vec<u8> {
    let mut body = vec![0xB0, 0u8, 0x31, (1 << 5) | 1];
    body.extend(881u16.to_be_bytes());
    body.push(1);
    body.push(0);
    body.extend([0x12, 0x34, 0x56, 0x78]);
    body.extend(text);
    body[1] = body.len() as u8;
    build_packet(&body)
}

fn main() {
    const TEXT: &[u8] = b"SECURITE. NAVAREA XII 123/26. PACIFIC. BUOY ADRIFT NEAR 38-00N 123-30W.";
    let mut payload = Vec::new();
    payload.extend(build_packet(&[0x7D, 1, 0x03, 0xE8, 0, 0, 1, 0x10, 0, 0, 0, 0]));
    payload.extend(egc_packet(TEXT));
    payload.resize(639, 0);
    let symbols = encode_frame(&payload);
    // Settling preamble: real STD-C is continuous; loops have time to
    // converge before any frame of interest.
    let mut all: Vec<u8> = (0..6000).map(|i| (i % 2) as u8).collect();
    all.extend(&symbols);
    all.extend(&symbols);
    all.extend(&symbols);
    let mut iq = modulate(&all, 1200.0, CHANNEL_RATE, 230.0, 0.5);
    let mut s = 0x5a5a_1234_8765_dcbau64;
    for x in iq.iter_mut() {
        s ^= s << 13; s ^= s >> 7; s ^= s << 17;
        let a = (s as f32 / u64::MAX as f32) * 2.0 - 1.0;
        s ^= s << 13; s ^= s >> 7; s ^= s << 17;
        let b = (s as f32 / u64::MAX as f32) * 2.0 - 1.0;
        *x += Complex::new(a * 0.02, b * 0.02);
    }

    // Mirror StdcChannelDecoder's sliding sync, with stats.
    let mut demod = BpskDemod::new(CHANNEL_RATE);
    let mut syms: Vec<f32> = Vec::new();
    let mut best = 0u32;
    let mut matches = 0u32;
    let mut slid = 0u64;
    let chunk_size: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(8192);
    for chunk in iq.chunks(chunk_size) {
        demod.process(chunk, &mut syms);
        loop {
            if syms.len() < FRAME_SYMBOLS {
                break;
            }
            let hard: Vec<u8> = syms[..FRAME_SYMBOLS].iter().map(|&v| (v > 0.0) as u8).collect();
            let (n, i) = uw_score(&hard);
            best = best.max(n.max(i));
            if n >= UW_MIN_MATCH || i >= UW_MIN_MATCH {
                matches += 1;
                let bytes = xng_mode_stdc::frame::FrameDecoder::new()
                    .decode(&syms[..FRAME_SYMBOLS], i > n);
                let mismatches: Vec<usize> = (0..639).filter(|&k| bytes[k] != payload[k]).collect();
                println!("frame: {} byte mismatches, first 12: {:?}", mismatches.len(), &mismatches[..mismatches.len().min(12)]);
                let mut parser = xng_mode_stdc::packet::PacketParser::new();
                for e in parser.parse_frame(&bytes) {
                    println!("event: {} text={:?}", e.name, e.text);
                }
                syms.drain(..FRAME_SYMBOLS);
                demod.locked = true;
            } else {
                syms.remove(0);
                slid += 1;
            }
        }
    }
    let (f, e, t) = demod.debug_state();
    let true_w = std::f32::consts::TAU * 230.0 / 12000.0;
    println!("matches {matches}, best {best}, slid {slid}; nco_freq {f:.6} (want {:.6}), carr_err {e:.3}, timing {t:.2}, syms {}", -true_w, syms.len() + slid as usize + matches as usize * 10368);
}
