//! Decode a navaid IQ file and print one JSON observation per line.
//!
//! ```text
//! cargo run --release -p xng-mode-navaid --example navaid_replay -- \
//!     FILE --rate 2000000 --offset 50000 --format cu8 --kind vor --report 2
//! ```
//!
//! Prints `{"cpu_s": …, "audio_s": …}` on stderr at the end, for benchmarks.

use num_complex::Complex;
use std::io::Read;
use std::time::Instant;
use xng_mode_navaid::{NavaidChannelDecoder, NavaidKind};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut path = None;
    let (mut rate, mut offset, mut format, mut kind, mut report) =
        (2_000_000.0, 0.0, "cu8".to_string(), NavaidKind::Vor, 10.0);
    let mut i = 1;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--rate" => rate = v.parse().unwrap(),
            "--offset" => offset = v.parse().unwrap(),
            "--format" => format = v,
            "--report" => report = v.parse().unwrap(),
            "--kind" => {
                kind = match v.as_str() {
                    "vor" => NavaidKind::Vor,
                    "loc" | "localizer" => NavaidKind::Localizer,
                    "gs" | "glideslope" => NavaidKind::Glideslope,
                    k => panic!("unknown kind {k}"),
                }
            }
            p => {
                path = Some(p.to_string());
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    let mut f = std::fs::File::open(path.expect("IQ file")).expect("open");
    let mut dec = NavaidChannelDecoder::new(kind, rate, offset).expect("decoder");
    dec.set_report_interval(report);
    let bytes_per = if format == "cu8" { 2 } else { 8 };
    let chunk = (rate as usize / 10) * bytes_per;
    let mut buf = vec![0u8; chunk];
    let mut iq: Vec<Complex<f32>> = Vec::with_capacity(chunk / bytes_per);
    let t0 = Instant::now();
    let mut samples = 0u64;
    loop {
        let n = read_full(&mut f, &mut buf).unwrap_or_else(|e| {
            eprintln!("read error: {e}");
            std::process::exit(1);
        });
        if n < bytes_per {
            break;
        }
        iq.clear();
        if format == "cu8" {
            for p in buf[..n - n % 2].chunks_exact(2) {
                iq.push(Complex::new(
                    (p[0] as f32 - 127.5) / 127.5,
                    (p[1] as f32 - 127.5) / 127.5,
                ));
            }
        } else {
            for p in buf[..n - n % 8].chunks_exact(8) {
                iq.push(Complex::new(
                    f32::from_le_bytes([p[0], p[1], p[2], p[3]]),
                    f32::from_le_bytes([p[4], p[5], p[6], p[7]]),
                ));
            }
        }
        samples += iq.len() as u64;
        for o in dec.process(&iq) {
            println!("{}", serde_json::to_string(&o).unwrap());
        }
        if n < chunk {
            break;
        }
    }
    eprintln!(
        "{{\"cpu_s\": {:.3}, \"audio_s\": {:.3}}}",
        t0.elapsed().as_secs_f64(),
        samples as f64 / rate
    );
}

fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}
