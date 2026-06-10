//! Stage-level diagnostics for off-air captures: DDC level, demod soft-bit
//! statistics, and UW near-match counts under different bit conventions.

use num_complex::Complex;
use xng_dsp::Ddc;
use xng_mode_aero::{demod::MskDemod, frame, CHANNEL_RATE, CHANNEL_PASSBAND_HZ};

fn count_uw(hard: &[u8], xf: impl Fn(u32) -> u32) -> (usize, usize) {
    let (mut n, mut ni) = (0, 0);
    let mut shift: u32 = 0;
    for (k, &b) in hard.iter().enumerate() {
        shift = (shift << 1) | b as u32;
        if k >= 31 {
            let w = xf(shift);
            if (w ^ frame::UW).count_ones() <= 2 {
                n += 1;
            }
            if (w ^ !frame::UW).count_ones() <= 2 {
                ni += 1;
            }
        }
    }
    (n, ni)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: offair_debug <f32le> <rate> <center> [bps]");
    let rate: f64 = args.next().unwrap().parse().unwrap();
    let center: f64 = args.next().unwrap().parse().unwrap();
    let bps: u32 = args.next().map(|s| s.parse().unwrap()).unwrap_or(600);

    let raw = std::fs::read(&path).expect("read");
    let samples: Vec<Complex<f32>> = raw
        .chunks_exact(4)
        .map(|b| Complex::new(f32::from_le_bytes([b[0], b[1], b[2], b[3]]), 0.0))
        .collect();

    let mut ddc = Ddc::new(rate, CHANNEL_RATE, center, CHANNEL_PASSBAND_HZ).unwrap();
    let mut chan = Vec::new();
    ddc.process(&samples, &mut chan);
    let pwr: f32 = chan.iter().map(|c| c.norm_sqr()).sum::<f32>() / chan.len() as f32;
    eprintln!("channel: {} samples, {:.1} dBFS", chan.len(), 10.0 * pwr.log10());

    let mut demod = MskDemod::new(CHANNEL_RATE, bps as f64);
    let mut bits: Vec<(f32, u8)> = Vec::new();
    demod.process(&chan, &mut bits);
    let hard: Vec<u8> = bits.iter().map(|&(_, h)| h).collect();
    let soft_abs: f32 =
        bits.iter().map(|&(s, _)| s.abs()).sum::<f32>() / bits.len().max(1) as f32;
    let ones = hard.iter().filter(|&&b| b == 1).count();
    eprintln!(
        "demod: {} bits ({:.1}s of {bps} bps), mean|soft|={soft_abs:.2}, ones={:.1}%",
        bits.len(),
        bits.len() as f64 / bps as f64,
        100.0 * ones as f64 / hard.len().max(1) as f64
    );

    let (a, b) = count_uw(&hard, |w| w);
    eprintln!("UW as-is: {a} / inverted: {b}");
    // Differential re-decode: bit = prev XOR cur (and its inverse).
    let diff: Vec<u8> = hard.windows(2).map(|w| w[0] ^ w[1]).collect();
    let (c, d) = count_uw(&diff, |w| w);
    eprintln!("UW differential: {c} / inverted: {d}");
    // NRZI-style: bit = !(prev XOR cur).
    let ndiff: Vec<u8> = diff.iter().map(|&b| b ^ 1).collect();
    let (e, f) = count_uw(&ndiff, |w| w);
    eprintln!("UW ndiff: {e} / inverted: {f}");
    // Reversed-bit UW (endianness check).
    let (g, h) = count_uw(&hard, |w| w.reverse_bits());
    eprintln!("UW bit-reversed: {g} / inverted: {h}");

    // Frame decode matrix: collect 1200-bit frames at UW hits and try
    // convention combinations (polynomial set, pair order, soft polarity,
    // scrambler, packing) against the SU CRCs.
    use xng_dsp::scramble::Lfsr15;
    use xng_dsp::viterbi::Viterbi;
    use xng_mode_aero::su;

    fn deinterleave(soft: &[f32], cols: usize, out: &mut Vec<f32>) {
        for j in 0..cols {
            for i in 0..64 {
                out.push(soft[((27 * i) % 64) * cols + j]);
            }
        }
    }

    let mut frames_soft: Vec<Vec<f32>> = Vec::new();
    let mut shift: u32 = 0;
    let mut k = 0usize;
    while k < bits.len() {
        shift = (shift << 1) | bits[k].1 as u32;
        if k >= 31 && (shift ^ frame::UW).count_ones() <= 2 {
            let start = k + 1 + frame::HEADER_BITS;
            let end = start + frame::CODED_BITS;
            if end > bits.len() {
                break;
            }
            frames_soft.push(bits[start..end].iter().map(|&(s, _)| s).collect());
            k = end;
            shift = 0;
            continue;
        }
        k += 1;
    }
    eprintln!("collected {} frames", frames_soft.len());
    if let Ok(p) = std::env::var("DUMP_BITS") {
        let h: Vec<u8> = bits.iter().map(|&(_, h)| h).collect();
        std::fs::write(&p, &h).unwrap();
        eprintln!("dumped {} hard bits to {p}", h.len());
    }

    for (pname, g1, g2) in [("171/133", 0o171u32, 0o133u32), ("117/155", 0o117, 0o155)] {
        for swap in [false, true] {
            for inv in [false, true] {
                for scram in [true, false] {
                    for msb in [false, true] {
                        let vit = Viterbi::new(7, g1, g2);
                        let (mut ok, mut total) = (0usize, 0usize);
                        for f in &frames_soft {
                            let mut coded = f.clone();
                            if swap {
                                for p in coded.chunks_exact_mut(2) {
                                    p.swap(0, 1);
                                }
                            }
                            if inv {
                                coded.iter_mut().for_each(|s| *s = -*s);
                            }
                            let mut dl = Vec::with_capacity(coded.len());
                            for chunk in coded.chunks_exact(64 * 6) {
                                deinterleave(chunk, 6, &mut dl);
                            }
                            let mut dec = vit.decode(&dl);
                            dec.truncate(576);
                            if scram {
                                Lfsr15::new().apply(&mut dec);
                            }
                            let bytes: Vec<u8> = dec
                                .chunks_exact(8)
                                .map(|c| {
                                    c.iter().enumerate().fold(0u8, |b, (i, &v)| {
                                        if msb { b | (v << (7 - i)) } else { b | (v << i) }
                                    })
                                })
                                .collect();
                            for su_b in bytes.chunks_exact(su::SU_LEN) {
                                total += 1;
                                ok += su::su_crc_ok(su_b) as usize;
                            }
                        }
                        if ok > 0 {
                            eprintln!("HIT poly={pname} swap={swap} inv={inv} scram={scram} msb={msb}: {ok}/{total}");
                        }
                    }
                }
            }
        }
    }
    eprintln!("matrix done");
}
