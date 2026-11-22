//! Throughput benchmark for [`FileIqSource`] sample conversion across every
//! on-disk [`IqFormat`].
//!
//! Usage:
//!
//! ```text
//! cargo run --release -p xng-sdr --example file_read_bench
//! ```
//!
//! Each format writes 12 s of deterministic noise at 2.4 MS/s (~55 MiB of
//! CU8, ~220 MiB of CF32) to a temp file once, then reads the whole file in
//! 65 536-sample buffers (the `READ_CHUNK` the runtime decode loop uses) and
//! reports the best of 5 runs after one warm-up. The file sits in the page
//! cache after the warm-up, so this measures the read + conversion path, not
//! the disk. "% core" is wall time over capture time.

use num_complex::Complex;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use xng_sdr::file::{FileIqSource, IqFormat};
use xng_sdr::{IqSource, SdrError};

const RATE: usize = 2_400_000;
const SECONDS: usize = 12;
const READ_CHUNK: usize = 65_536;
const RUNS: usize = 5;

/// (format, file extension)
const FORMATS: &[(IqFormat, &str)] = &[
    (IqFormat::Cu8, "cu8"),
    (IqFormat::Cs8, "cs8"),
    (IqFormat::Cs16, "cs16"),
    (IqFormat::Cf32, "cf32"),
];

fn noise_bytes(format: IqFormat, samples: usize) -> Vec<u8> {
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut out = Vec::with_capacity(samples * format.bytes_per_sample());
    for _ in 0..samples * 2 {
        let r = next();
        match format {
            IqFormat::Cu8 | IqFormat::Cs8 => out.push((r >> 56) as u8),
            IqFormat::Cs16 => out.extend_from_slice(&((r >> 48) as u16).to_le_bytes()),
            IqFormat::Cf32 => {
                let v = (r >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0;
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    out
}

fn run_once(path: &Path, format: IqFormat, buf: &mut [Complex<f32>]) -> (Duration, usize) {
    let mut src = FileIqSource::open(path, format, RATE as f64, 0).expect("open temp file");
    let mut total = 0;
    let start = Instant::now();
    loop {
        match src.read(buf) {
            Ok(n) => {
                total += n;
                black_box(&*buf);
            }
            Err(SdrError::EndOfStream) => break,
            Err(e) => panic!("read failed: {e}"),
        }
    }
    (start.elapsed(), total)
}

fn main() {
    let samples = RATE * SECONDS;
    println!(
        "{:>6} {:>8} {:>9} {:>10} {:>8} {:>8}",
        "format", "MiB", "ms", "ns/sample", "MS/s", "% core"
    );
    let mut buf = vec![Complex::new(0.0f32, 0.0); READ_CHUNK];
    for &(format, ext) in FORMATS {
        let path: PathBuf =
            std::env::temp_dir().join(format!("xng_file_read_bench_{}.{ext}", std::process::id()));
        let bytes = noise_bytes(format, samples);
        std::fs::write(&path, &bytes).expect("write temp file");
        let mib = bytes.len() as f64 / (1 << 20) as f64;
        drop(bytes);

        run_once(&path, format, &mut buf);
        let mut best = Duration::MAX;
        let mut total = 0;
        for _ in 0..RUNS {
            let (t, n) = run_once(&path, format, &mut buf);
            best = best.min(t);
            total = n;
        }
        assert_eq!(total, samples, "short read of {ext} file");
        std::fs::remove_file(&path).expect("remove temp file");

        let ms = best.as_secs_f64() * 1e3;
        let ns_per_sample = best.as_secs_f64() * 1e9 / total as f64;
        let msps = total as f64 / best.as_secs_f64() / 1e6;
        let core = best.as_secs_f64() / SECONDS as f64 * 100.0;
        println!("{ext:>6} {mib:>8.1} {ms:>9.3} {ns_per_sample:>10.3} {msps:>8.1} {core:>7.3}%");
    }
}
