//! Front-end comparison on a REAL off-air capture (PERF-14): how
//! [`AcarsMultiChannelDecoder`]'s front ends (default, shared, critically
//! sampled channelizer) compare to the pre-multi-channel "old path" — one
//! independent [`AcarsChannelDecoder`] per channel, each with its own DDC —
//! when decoding an actual RTL-SDR capture rather than synthetic bursts.
//!
//! ```bash
//! cargo run --release -p xng-mode-acars --example acars_rf_compare -- \
//!     acars_16ch_2m4.cu8 cu8 2400000 131137500 \
//!     130.425,130.450,130.550,130.825,130.850,131.125,131.250,131.425,131.450,131.475,131.525,131.550,131.650,131.725,131.825,131.850
//! ```
//!
//! The file is streamed in ~65536-sample chunks (never loaded whole as
//! `f32`); cu8/cs16/cf32 conversion is [`xng_sdr::file::FileIqSource`], the
//! same code the `xng` CLI uses, so this cannot drift from what `xng decode`
//! does to the same bytes.
//!
//! Four front ends see the identical byte stream, one full file pass each:
//!
//! | label        | construction                                           |
//! |--------------|---------------------------------------------------------|
//! | `per-channel`| N independent [`AcarsChannelDecoder`]s (old per-channel)  |
//! | `default`    | [`AcarsMultiChannelDecoder::new`] (what the daemon runs) |
//! | `shared`     | `new_shared` (shared-decimation front end)               |
//! | `channelized`| `new_channelized` (critically sampled channelizer)       |
//!
//! A CRC-OK frame is identified by `(channel, tail, msg_num, label, text)`;
//! frames found under that key by only one front end are reported as
//! "unique" to it, with channel/time/level for a handful of examples. CPU is
//! wall time spent inside `process()` only (file I/O and cu8 conversion are
//! excluded, and are identical cost across front ends regardless).

use num_complex::Complex;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};
use xng_mode_acars::frame::AcarsFrame;
use xng_mode_acars::{AcarsChannelDecoder, AcarsMultiChannelDecoder};
use xng_sdr::file::{FileIqSource, IqFormat};
use xng_sdr::{IqSource, SdrError};

const CHUNK: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fe {
    PerChannel,
    Default,
    Shared,
    Channelized,
}

const FES: [Fe; 4] = [Fe::PerChannel, Fe::Default, Fe::Shared, Fe::Channelized];

impl Fe {
    fn label(self) -> &'static str {
        match self {
            Self::PerChannel => "per-channel",
            Self::Default => "default",
            Self::Shared => "shared",
            Self::Channelized => "channelized",
        }
    }
}

/// Normalizes the "N independent decoders" and "one shared front end"
/// shapes behind a single `process` so the streaming loop below runs once.
enum FrontEndKind {
    PerChannel(Vec<AcarsChannelDecoder>),
    Multi(Box<AcarsMultiChannelDecoder>),
}

impl FrontEndKind {
    fn process(&mut self, chunk: &[Complex<f32>]) -> Vec<(usize, Vec<AcarsFrame>)> {
        match self {
            Self::PerChannel(v) => v
                .iter_mut()
                .enumerate()
                .filter_map(|(i, d)| {
                    let f = d.process(chunk);
                    if f.is_empty() {
                        None
                    } else {
                        Some((i, f))
                    }
                })
                .collect(),
            Self::Multi(m) => m.process(chunk),
        }
    }

    fn level_dbfs(&self, i: usize) -> f32 {
        match self {
            Self::PerChannel(v) => v[i].level_dbfs(),
            Self::Multi(m) => m.level_dbfs(i),
        }
    }

    fn noise_dbfs(&self, i: usize) -> f32 {
        match self {
            Self::PerChannel(v) => v[i].noise_dbfs(),
            Self::Multi(m) => m.noise_dbfs(i),
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::PerChannel(v) => {
                format!("per-channel: {} independent AcarsChannelDecoder (own DDC each)", v.len())
            }
            Self::Multi(m) => m.describe_front(),
        }
    }
}

fn build(fe: Fe, rate: f64, offs: &[f64]) -> Result<FrontEndKind, String> {
    match fe {
        Fe::PerChannel => {
            let v: Result<Vec<_>, _> =
                offs.iter().map(|&o| AcarsChannelDecoder::new(rate, o)).collect();
            Ok(FrontEndKind::PerChannel(v?))
        }
        Fe::Default => {
            Ok(FrontEndKind::Multi(Box::new(AcarsMultiChannelDecoder::new(rate, offs)?)))
        }
        Fe::Shared => {
            Ok(FrontEndKind::Multi(Box::new(AcarsMultiChannelDecoder::new_shared(rate, offs)?)))
        }
        Fe::Channelized => Ok(FrontEndKind::Multi(Box::new(
            AcarsMultiChannelDecoder::new_channelized(rate, offs)?,
        ))),
    }
}

#[derive(Clone)]
struct FrameRec {
    channel: usize,
    time_s: f64,
    level_dbfs: f32,
    noise_dbfs: f32,
    crc_ok: bool,
    tail: Option<String>,
    msg_num: Option<String>,
    label: String,
    text: String,
}

/// `(channel, tail, msg_num, label, text)` — identifies a CRC-OK frame
/// across front ends independent of which DDC chain produced it.
type FrameKey = (usize, Option<String>, Option<String>, String, String);

impl FrameRec {
    fn key(&self) -> FrameKey {
        (
            self.channel,
            self.tail.clone(),
            self.msg_num.clone(),
            self.label.clone(),
            self.text.clone(),
        )
    }
}

#[derive(Default, Clone, Copy)]
struct ChannelHealth {
    n: u64,
    level_sum: f64,
    noise_sum: f64,
    level_min: f32,
    level_max: f32,
}

impl ChannelHealth {
    fn sample(&mut self, level: f32, noise: f32) {
        if self.n == 0 {
            self.level_min = level;
            self.level_max = level;
        } else {
            self.level_min = self.level_min.min(level);
            self.level_max = self.level_max.max(level);
        }
        self.n += 1;
        self.level_sum += f64::from(level);
        self.noise_sum += f64::from(noise);
    }

    fn mean_level(&self) -> f32 {
        (self.level_sum / self.n.max(1) as f64) as f32
    }

    fn mean_noise(&self) -> f32 {
        (self.noise_sum / self.n.max(1) as f64) as f32
    }
}

struct FrontEndResult {
    fe: Fe,
    describe: String,
    total: Vec<usize>,
    crc_ok: Vec<usize>,
    frames: Vec<FrameRec>,
    cpu_ms: f64,
    duration_s: f64,
    health: Option<Vec<ChannelHealth>>,
}

/// One streaming pass over the file for front end `fe`. `collect_health`
/// snapshots per-channel level/noise at the end of every chunk (used once,
/// on the default front end, for the capture-health table).
fn run_front_end(
    fe: Fe,
    path: &Path,
    fmt: IqFormat,
    rate: f64,
    center_hz: u64,
    offs: &[f64],
    collect_health: bool,
) -> Result<FrontEndResult, String> {
    let nch = offs.len();
    let mut dec = build(fe, rate, offs)?;
    let mut src = FileIqSource::open(path, fmt, rate, center_hz).map_err(|e| e.to_string())?;
    let mut buf = vec![Complex::new(0.0f32, 0.0); CHUNK];
    let mut total = vec![0usize; nch];
    let mut crc_ok = vec![0usize; nch];
    let mut frames = Vec::new();
    let mut cpu = Duration::ZERO;
    let mut n0: u64 = 0;
    let mut health = if collect_health { Some(vec![ChannelHealth::default(); nch]) } else { None };

    loop {
        let n = match src.read(&mut buf) {
            Ok(n) => n,
            Err(SdrError::EndOfStream) => break,
            Err(e) => return Err(e.to_string()),
        };
        let chunk = &buf[..n];
        let t0 = Instant::now();
        let out = dec.process(chunk);
        cpu += t0.elapsed();
        n0 += n as u64;
        let time_s = n0 as f64 / rate;

        for (ch, fs) in out {
            total[ch] += fs.len();
            let level = dec.level_dbfs(ch);
            let noise = dec.noise_dbfs(ch);
            for f in &fs {
                if f.crc_ok {
                    crc_ok[ch] += 1;
                }
                frames.push(FrameRec {
                    channel: ch,
                    time_s,
                    level_dbfs: level,
                    noise_dbfs: noise,
                    crc_ok: f.crc_ok,
                    tail: f.tail.clone(),
                    msg_num: f.msg_num.clone(),
                    label: f.label.clone(),
                    text: f.text.clone(),
                });
            }
        }
        if let Some(h) = health.as_mut() {
            for (ch, slot) in h.iter_mut().enumerate() {
                slot.sample(dec.level_dbfs(ch), dec.noise_dbfs(ch));
            }
        }
    }

    Ok(FrontEndResult {
        fe,
        describe: dec.describe(),
        total,
        crc_ok,
        frames,
        cpu_ms: cpu.as_secs_f64() * 1e3,
        duration_s: n0 as f64 / rate,
        health,
    })
}

/// The exact `(negative rail, positive rail)` a full-scale ADC sample
/// converts to for `fmt`, straight out of [`xng_sdr::file::FileIqSource`]'s
/// conversion (`crates/xng-sdr/src/file.rs`): `None` when the format has no
/// meaningful clipping rail.
///
/// * cu8: `(byte - 127.5) / 127.5` lands on exactly +/-1.0 for byte 0/255.
/// * cs8: `byte as i8 as f32 / 128.0` is asymmetric — `i8::MIN / 128.0 =
///   -1.0` but `i8::MAX / 128.0 = 127.0 / 128.0 = 0.9921875`, not 1.0, so a
///   fixed +/-1.0 compare never flags cs8's positive rail.
/// * cs16: `i16 as f32 / 32768.0` is the same shape — `-1.0` and `32767.0 /
///   32768.0`, not 1.0.
/// * cf32: arbitrary floats with no fixed full-scale value; clipping is not
///   a meaningful concept here, so this returns `None`.
///
/// Every rail below is an exact power-of-two-denominator division of an
/// integer that fits the mantissa, so it reproduces the conversion's float
/// bit-for-bit and an exact `==` compare is safe (no epsilon needed).
fn clip_rails(fmt: IqFormat) -> Option<(f32, f32)> {
    match fmt {
        IqFormat::Cu8 => Some((-1.0, 1.0)),
        IqFormat::Cs8 => Some((-1.0, 127.0 / 128.0)),
        IqFormat::Cs16 => Some((-1.0, 32_767.0 / 32_768.0)),
        IqFormat::Cf32 => None,
    }
}

/// ADC/DC health scan: a plain streaming pass over the converted samples (no
/// decode), counting full-scale (clipped) samples and the DC bias in I and Q.
/// Clipping is only meaningful for fixed-point formats; see [`clip_rails`].
struct HealthScan {
    samples: u64,
    dc_re_sum: f64,
    dc_im_sum: f64,
    clip_re: u64,
    clip_im: u64,
    clip_rails: Option<(f32, f32)>,
}

fn health_scan(
    path: &Path,
    fmt: IqFormat,
    rate: f64,
    center_hz: u64,
) -> Result<HealthScan, String> {
    let mut src = FileIqSource::open(path, fmt, rate, center_hz).map_err(|e| e.to_string())?;
    let mut buf = vec![Complex::new(0.0f32, 0.0); CHUNK];
    let rails = clip_rails(fmt);
    let mut scan = HealthScan {
        samples: 0,
        dc_re_sum: 0.0,
        dc_im_sum: 0.0,
        clip_re: 0,
        clip_im: 0,
        clip_rails: rails,
    };
    loop {
        let n = match src.read(&mut buf) {
            Ok(n) => n,
            Err(SdrError::EndOfStream) => break,
            Err(e) => return Err(e.to_string()),
        };
        for s in &buf[..n] {
            scan.dc_re_sum += f64::from(s.re);
            scan.dc_im_sum += f64::from(s.im);
            if let Some((neg, pos)) = rails {
                if s.re <= neg || s.re >= pos {
                    scan.clip_re += 1;
                }
                if s.im <= neg || s.im >= pos {
                    scan.clip_im += 1;
                }
            }
        }
        scan.samples += n as u64;
    }
    Ok(scan)
}

fn pct_of_core(cpu_ms: f64, duration_s: f64) -> f64 {
    cpu_ms / (duration_s * 1000.0) * 100.0
}

fn print_per_channel_table(mhz: &[f64], results: &[FrontEndResult]) {
    print!("{:<10}", "ch MHz");
    for r in results {
        print!("{:>16}", r.fe.label());
    }
    println!();
    print!("{:<10}", "");
    for _ in results {
        print!("{:>16}", "total/crc-ok");
    }
    println!();
    for (ch, &f) in mhz.iter().enumerate() {
        print!("{f:<10.3}");
        for r in results {
            print!("{:>16}", format!("{}/{}", r.total[ch], r.crc_ok[ch]));
        }
        println!();
    }
    print!("{:<10}", "TOTAL");
    for r in results {
        let t: usize = r.total.iter().sum();
        let c: usize = r.crc_ok.iter().sum();
        print!("{:>16}", format!("{t}/{c}"));
    }
    println!();
}

fn crc_ok_keys(r: &FrontEndResult) -> HashMap<FrameKey, &FrameRec> {
    let mut m = HashMap::new();
    for f in &r.frames {
        if f.crc_ok {
            m.entry(f.key()).or_insert(f);
        }
    }
    m
}

fn print_examples(
    keys: &[&FrameKey],
    recs: &HashMap<FrameKey, &FrameRec>,
    mhz: &[f64],
    limit: usize,
) {
    for k in keys.iter().take(limit) {
        let f = recs[*k];
        let ch_mhz = mhz.get(f.channel).copied().unwrap_or(f64::NAN);
        let tail = f.tail.as_deref().unwrap_or("-");
        let msg = f.msg_num.as_deref().unwrap_or("-");
        let text: String = f.text.chars().take(48).collect();
        println!(
            "    ch{:>2} {ch_mhz:>8.3}  t={:>7.1}s  level={:>6.1}dBFS  noise={:>6.1}dBFS  \
             {}/{tail}/{msg}  {text:?}",
            f.channel, f.time_s, f.level_dbfs, f.noise_dbfs, f.label
        );
    }
    if keys.len() > limit {
        println!("    ... and {} more", keys.len() - limit);
    }
}

fn unique_frame_analysis(results: &[FrontEndResult], mhz: &[f64]) {
    let sets: Vec<HashMap<FrameKey, &FrameRec>> = results.iter().map(crc_ok_keys).collect();

    println!("\n-- frames unique to one front end (CRC-OK only) --\n");
    for (i, r) in results.iter().enumerate() {
        let mine = &sets[i];
        let mut unique: Vec<&FrameKey> = mine
            .keys()
            .filter(|k| sets.iter().enumerate().all(|(j, s)| j == i || !s.contains_key(*k)))
            .collect();
        unique.sort_by(|a, b| a.0.cmp(&b.0).then(mine[*a].time_s.total_cmp(&mine[*b].time_s)));
        println!("{} ({} unique of {} CRC-OK):", r.fe.label(), unique.len(), mine.len());
        print_examples(&unique, mine, mhz, 20);
    }

    // The case called out explicitly: frames the default misses that at
    // least one of the other three front ends decodes.
    let default_idx = results.iter().position(|r| r.fe == Fe::Default).expect("default present");
    let default_set = &sets[default_idx];
    let mut missed: Vec<&FrameKey> = Vec::new();
    let mut missed_recs: HashMap<FrameKey, &FrameRec> = HashMap::new();
    for (i, s) in sets.iter().enumerate() {
        if i == default_idx {
            continue;
        }
        for (k, f) in s {
            if !default_set.contains_key(k) && !missed_recs.contains_key(k) {
                missed_recs.insert(k.clone(), f);
            }
        }
    }
    missed.extend(missed_recs.keys());
    missed.sort_by(|a, b| {
        a.0.cmp(&b.0).then(missed_recs[*a].time_s.total_cmp(&missed_recs[*b].time_s))
    });
    println!(
        "\ndefault misses ({} frames CRC-OK under per-channel/shared/channelized but not default):",
        missed.len()
    );
    print_examples(&missed, &missed_recs, mhz, 20);
}

fn print_capture_health(
    mhz: &[f64],
    rate: f64,
    default_result: &FrontEndResult,
    scan: &HealthScan,
) {
    println!("\n-- capture health --\n");
    println!("samples scanned: {}", scan.samples);
    println!(
        "DC bias: I mean {:+.5} ({:+.1} dBFS), Q mean {:+.5} ({:+.1} dBFS)",
        scan.dc_re_sum / scan.samples as f64,
        20.0 * (scan.dc_re_sum.abs() / scan.samples as f64).log10(),
        scan.dc_im_sum / scan.samples as f64,
        20.0 * (scan.dc_im_sum.abs() / scan.samples as f64).log10(),
    );
    match scan.clip_rails {
        Some((neg, pos)) => {
            println!(
                "ADC clipping (full-scale rails {neg:+.6}/{pos:+.6}): I {} ({:.4}%), Q {} ({:.4}%)",
                scan.clip_re,
                100.0 * scan.clip_re as f64 / scan.samples as f64,
                scan.clip_im,
                100.0 * scan.clip_im as f64 / scan.samples as f64,
            );
        }
        None => println!("ADC clipping: not meaningful for cf32 (arbitrary-range floats)"),
    }

    let Some(health) = &default_result.health else {
        return;
    };
    println!(
        "\nper-channel level (default front end, end-of-chunk EMA snapshots, ~{:.2}s apart):",
        CHUNK as f64 / rate
    );
    println!(
        "{:<10}{:>10}{:>10}{:>10}{:>10}{:>10}",
        "ch MHz", "mean lvl", "min lvl", "max lvl", "mean nf", "SNR"
    );
    for (ch, &f) in mhz.iter().enumerate() {
        let h = &health[ch];
        println!(
            "{f:<10.3}{:>9.1} {:>9.1} {:>9.1} {:>9.1} {:>9.1}",
            h.mean_level(),
            h.level_min,
            h.level_max,
            h.mean_noise(),
            h.mean_level() - h.mean_noise(),
        );
    }
}

fn parse_mhz_list(s: &str) -> Vec<f64> {
    s.split(',').map(|x| x.trim().parse().expect("bad channel MHz")).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 6 {
        eprintln!(
            "usage: acars_rf_compare <file> <fmt cu8|cs16|cf32> <rate> <center_hz> <ch_mhz,ch_mhz,...>"
        );
        std::process::exit(2);
    }
    let path = Path::new(&args[1]);
    let fmt: IqFormat = args[2].parse().unwrap_or_else(|e| {
        eprintln!("bad format {:?}: {e}", args[2]);
        std::process::exit(2);
    });
    let rate: f64 = args[3].parse().expect("bad rate");
    let center_hz: u64 = args[4].parse().expect("bad center_hz");
    let mhz = parse_mhz_list(&args[5]);
    let offs: Vec<f64> = mhz.iter().map(|f| f * 1e6 - center_hz as f64).collect();
    let nch = offs.len();

    println!(
        "acars_rf_compare: {} ({fmt:?}, {rate:.0} S/s, center {center_hz} Hz, {nch} channels)\n",
        path.display()
    );

    let mut results = Vec::new();
    for fe in FES {
        let collect_health = fe == Fe::Default;
        print!("running {:<13} ... ", fe.label());
        use std::io::Write as _;
        std::io::stdout().flush().ok();
        match run_front_end(fe, path, fmt, rate, center_hz, &offs, collect_health) {
            Ok(r) => {
                println!(
                    "{:.1} CPU ms over {:.1} s ({:.3}% of one core); describe_front: {}",
                    r.cpu_ms,
                    r.duration_s,
                    pct_of_core(r.cpu_ms, r.duration_s),
                    r.describe
                );
                results.push(r);
            }
            Err(e) => {
                println!("REFUSED: {e}");
            }
        }
    }

    println!("\n-- per-channel frame counts (total/CRC-OK) --\n");
    print_per_channel_table(&mhz, &results);

    println!("\n-- CPU --\n");
    println!("{:<13}{:>14}{:>12}", "front end", "CPU ms", "% of 1 core");
    for r in &results {
        println!(
            "{:<13}{:>14.1}{:>11.3}%",
            r.fe.label(),
            r.cpu_ms,
            pct_of_core(r.cpu_ms, r.duration_s)
        );
    }

    unique_frame_analysis(&results, &mhz);

    println!("\nscanning capture health (DC bias, clipping) ...");
    match health_scan(path, fmt, rate, center_hz) {
        Ok(scan) => {
            if let Some(default_result) = results.iter().find(|r| r.fe == Fe::Default) {
                print_capture_health(&mhz, rate, default_result, &scan);
            }
        }
        Err(e) => eprintln!("health scan failed: {e}"),
    }
}
