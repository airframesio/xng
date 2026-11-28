//! Benchmark helper for `bench/dme`.
//!
//! ```text
//! dme_bench sim  <out.cf32> <truth.csv> <rate> <seconds> <snr_db> [ident] [offset_hz] [shape] [seed] [random_pps] [dot_ms]
//! dme_bench run  <in.cf32> <rate> <pairs.txt> [carrier_hz]
//! dme_bench score <truth.csv> <pairs.txt> <tol_us> <seconds>
//! ```
//!
//! `sim` writes a synthetic X-channel transponder (2700 pp/s random pairs and
//! an optional ident). `run` decodes a cf32 file and writes one pair time
//! (µs) per line, printing idents and the CPU time (file read, decode and
//! pair-file write: the same work gr-dme's timed flowgraph does). `score` matches pair
//! times against the truth.

use num_complex::Complex;
use std::io::{BufRead, BufWriter, Read, Write};
use std::time::Instant;
use xng_mode_dme::modulate::{generate, score, PulseShape, SimConfig, TruthPair};
use xng_mode_dme::{detect::Thresholds, DmeChannelDecoder, DmeEvent};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("sim") => sim(&a[2..]),
        Some("run") => run(&a[2..]),
        Some("score") => score_cmd(&a[2..]),
        _ => {
            eprintln!("usage: dme_bench sim|run|score ...");
            std::process::exit(2);
        }
    }
}

fn sim(a: &[String]) {
    let cfg = SimConfig {
        sample_rate: a[2].parse().unwrap(),
        duration_s: a[3].parse().unwrap(),
        snr_db: a[4].parse().unwrap(),
        ident: a.get(5).filter(|s| *s != "-").cloned(),
        freq_offset_hz: a.get(6).map(|s| s.parse().unwrap()).unwrap_or(0.0),
        shape: match a.get(7).map(String::as_str) {
            Some("cos2") => PulseShape::CosSquared,
            _ => PulseShape::Gaussian,
        },
        seed: a.get(8).map(|s| s.parse().unwrap()).unwrap_or(1),
        random_pps: a.get(9).map(|s| s.parse().unwrap()).unwrap_or(2700.0),
        dot_ms: a.get(10).map(|s| s.parse().unwrap()).unwrap_or(125.0),
        ident_start_s: 1.0,
        ..Default::default()
    };
    let out = generate(&cfg);
    let mut f = BufWriter::new(std::fs::File::create(&a[0]).unwrap());
    for s in &out.iq {
        f.write_all(&s.re.to_le_bytes()).unwrap();
        f.write_all(&s.im.to_le_bytes()).unwrap();
    }
    let mut t = BufWriter::new(std::fs::File::create(&a[1]).unwrap());
    for p in &out.pairs {
        writeln!(t, "{:.3},{}", p.t_us, p.ident as u8).unwrap();
    }
}

fn run(a: &[String]) {
    let rate: f64 = a[1].parse().unwrap();
    // Timed from the file read on, like gr-dme's file source flowgraph.
    let t0 = Instant::now();
    let cpu0 = cpu_seconds();
    let mut bytes = Vec::new();
    std::fs::File::open(&a[0])
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    let iq: Vec<Complex<f32>> = bytes
        .chunks_exact(8)
        .map(|c| {
            Complex::new(
                f32::from_le_bytes(c[0..4].try_into().unwrap()),
                f32::from_le_bytes(c[4..8].try_into().unwrap()),
            )
        })
        .collect();
    let th = Thresholds::default();
    let mut dec = DmeChannelDecoder::with_thresholds(rate, 0.0, th).unwrap();
    if let Some(c) = a.get(3) {
        dec = dec.with_carrier(c.parse().unwrap(), th);
    }
    let mut pairs = Vec::new();
    let mut events = Vec::new();
    for chunk in iq.chunks(65_536) {
        events.extend(dec.process_with_pairs(chunk, &mut pairs));
    }
    events.extend(dec.finish(Some(&mut pairs)));
    let mut f = BufWriter::new(std::fs::File::create(&a[2]).unwrap());
    for p in &pairs {
        writeln!(f, "{:.3}", p.t_us).unwrap();
    }
    f.flush().unwrap();
    drop(f);
    let cpu = cpu_seconds() - cpu0;
    let wall = t0.elapsed().as_secs_f64();
    for e in &events {
        match e {
            DmeEvent::Ident(r) => println!("IDENT {} {} repeats={}", r.ident, r.morse, r.repeats),
            DmeEvent::Status(s) => println!(
                "STATUS pps={:.0} ident_pps={:.0} snr={:.1}",
                s.pps, s.ident_pps, s.snr_db
            ),
        }
    }
    println!("PAIRS {}", pairs.len());
    println!(
        "CPU {cpu:.4} WALL {wall:.4} SECONDS {:.3}",
        iq.len() as f64 / rate
    );
}

fn score_cmd(a: &[String]) {
    let truth: Vec<TruthPair> = std::io::BufReader::new(std::fs::File::open(&a[0]).unwrap())
        .lines()
        .map(|l| {
            let l = l.unwrap();
            let mut it = l.split(',');
            TruthPair {
                t_us: it.next().unwrap().parse().unwrap(),
                ident: it.next() == Some("1"),
            }
        })
        .collect();
    let det: Vec<f64> = std::fs::read_to_string(&a[1])
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.parse().unwrap())
        .collect();
    let tol: f64 = a[2].parse().unwrap();
    let secs: f64 = a[3].parse().unwrap();
    let (tp, fp) = score(&truth, &det, tol);
    println!(
        "{} {} {} {:.4} {:.2}",
        truth.len(),
        tp,
        fp,
        tp as f64 / truth.len() as f64,
        fp as f64 / secs
    );
}

/// CPU time of this (single) thread, s, from /proc/self/schedstat (ns).
fn cpu_seconds() -> f64 {
    std::fs::read_to_string("/proc/self/schedstat")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok())
        })
        .unwrap_or(0.0)
        / 1e9
}
