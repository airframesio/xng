//! PERF-14 (VDL2): validate the multi-channel front ends on a real off-air
//! capture by streaming it through every `Vdl2MultiChannelDecoder` front end
//! plus the per-channel reference, then diffing the decoded frame sets.
//!
//! ```bash
//! cargo run --release -p xng-mode-vdl2 --example vdl2_rf_compare -- \
//!   vdl2_7ch_2m4.cu8 cu8 2400000 136812500 \
//!   136.650,136.725,136.775,136.825,136.875,136.925,136.975
//! ```
//!
//! Arguments: `<file> <fmt cu8|cs16|cf32> <rate> <center_hz> <ch_mhz,...>`.
//! The file is streamed in ~65536-sample blocks (never materialized whole);
//! the cu8/cs16/cf32 conversion is `xng_sdr::FileIqSource`, the same code the
//! `xng` CLI uses. Each block is fed, unmodified, to every front end under
//! test so the comparison is apples-to-apples and the file is read once.
//!
//! Frames are identified by `(channel, AVLC raw bytes)`; set differences
//! between front ends are reported with up to ~20 example frames each way.
//! Capture health (RSSI/noise floor per channel, ADC clipping, DC spike) is
//! measured from the same stream pass.

use num_complex::Complex;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use xng_dsp::Oversample;
use xng_mode_vdl2::{Vdl2ChannelDecoder, Vdl2Frame, Vdl2MultiChannelDecoder};
use xng_sdr::{FileIqSource, IqFormat, IqSource, SdrError};

const BLOCK: usize = 65_536;
/// Up to this many example frames are kept per direction of a set diff.
const MAX_EXAMPLES: usize = 20;

/// One decoded frame, keyed by `(channel, raw bytes)` for set comparisons.
#[derive(Clone)]
struct FrameRec {
    channel: usize,
    /// Wideband-capture time (s) at the end of the block the frame surfaced
    /// in (block resolution, ~27 ms at 2.4 MS/s).
    time_s: f64,
    snr_db: f32,
    rssi_dbfs: f32,
    raw: Vec<u8>,
}

/// One front end under test.
enum Under {
    PerChannel(Vec<Vdl2ChannelDecoder>),
    Multi(Box<Vdl2MultiChannelDecoder>),
}

struct FrontEnd {
    name: &'static str,
    describe: String,
    dec: Under,
    /// Time spent inside `process()` only (decode CPU, excludes file I/O).
    cpu: Duration,
    frames: Vec<FrameRec>,
    per_channel: Vec<usize>,
}

impl FrontEnd {
    fn new(name: &'static str, describe: String, dec: Under, channels: usize) -> Self {
        Self {
            name,
            describe,
            dec,
            cpu: Duration::ZERO,
            frames: Vec::new(),
            per_channel: vec![0; channels],
        }
    }

    fn record(&mut self, channel: usize, time_s: f64, rssi_dbfs: f32, frames: Vec<Vdl2Frame>) {
        self.per_channel[channel] += frames.len();
        for f in frames {
            self.frames.push(FrameRec {
                channel,
                time_s,
                snr_db: f.snr_db,
                rssi_dbfs,
                raw: f.avlc.raw.clone(),
            });
        }
    }

    /// Current channel level (dBFS, an EMA of channel power tracked by the
    /// demod continuously, independent of whether a burst was found) —
    /// used for the end-of-run per-channel noise-floor/RSSI report.
    fn level_dbfs(&self, channel: usize) -> f32 {
        match &self.dec {
            Under::PerChannel(decs) => decs[channel].level_dbfs(),
            Under::Multi(d) => d.level_dbfs(channel),
        }
    }

    fn feed(&mut self, block: &[Complex<f32>], now: f64) {
        let t0 = Instant::now();
        // Levels are read right after `process` (inside the timed+borrowed
        // section) so the record step below never needs to re-borrow `self.dec`.
        let results: Vec<(usize, f32, Vec<Vdl2Frame>)> = match &mut self.dec {
            Under::PerChannel(decs) => {
                let mut out = Vec::new();
                for (i, d) in decs.iter_mut().enumerate() {
                    let frames = d.process(block);
                    if !frames.is_empty() {
                        out.push((i, d.level_dbfs(), frames));
                    }
                }
                out
            }
            Under::Multi(d) => d
                .process(block)
                .into_iter()
                .map(|(i, frames)| (i, d.level_dbfs(i), frames))
                .collect(),
        };
        self.cpu += t0.elapsed();
        for (i, rssi, frames) in results {
            self.record(i, now, rssi, frames);
        }
    }
}

/// Frame identity for set comparisons: `(channel, raw bytes)`.
type Key = (usize, Vec<u8>);

/// Multiset-aware diff: frames in `a` with no matching count in `b`, and
/// vice versa. Returns `(count_a_only, examples_a_only, count_b_only,
/// examples_b_only)`.
fn diff(a: &[FrameRec], b: &[FrameRec]) -> (usize, Vec<FrameRec>, usize, Vec<FrameRec>) {
    let mut by_key_a: HashMap<Key, Vec<&FrameRec>> = HashMap::new();
    for f in a {
        by_key_a.entry((f.channel, f.raw.clone())).or_default().push(f);
    }
    let mut by_key_b: HashMap<Key, Vec<&FrameRec>> = HashMap::new();
    for f in b {
        by_key_b.entry((f.channel, f.raw.clone())).or_default().push(f);
    }
    let mut keys: Vec<&Key> = by_key_a.keys().chain(by_key_b.keys()).collect();
    keys.sort();
    keys.dedup();

    let mut count_a_only = 0usize;
    let mut count_b_only = 0usize;
    let mut ex_a = Vec::new();
    let mut ex_b = Vec::new();
    for key in keys {
        let va = by_key_a.get(key).map(|v| v.len()).unwrap_or(0);
        let vb = by_key_b.get(key).map(|v| v.len()).unwrap_or(0);
        if va > vb {
            count_a_only += va - vb;
            if ex_a.len() < MAX_EXAMPLES {
                for f in by_key_a[key].iter().take(va - vb) {
                    if ex_a.len() >= MAX_EXAMPLES {
                        break;
                    }
                    ex_a.push((*f).clone());
                }
            }
        } else if vb > va {
            count_b_only += vb - va;
            if ex_b.len() < MAX_EXAMPLES {
                for f in by_key_b[key].iter().take(vb - va) {
                    if ex_b.len() >= MAX_EXAMPLES {
                        break;
                    }
                    ex_b.push((*f).clone());
                }
            }
        }
    }
    (count_a_only, ex_a, count_b_only, ex_b)
}

fn print_examples(label: &str, ch_mhz: &[f64], examples: &[FrameRec]) {
    if examples.is_empty() {
        return;
    }
    println!("    {label} (showing up to {MAX_EXAMPLES}):");
    for f in examples {
        println!(
            "      ch={:>8.3} MHz  t={:>7.2}s  snr={:>5.1} dB  rssi={:>6.1} dBFS  raw_len={}  raw[..16]={}",
            ch_mhz[f.channel],
            f.time_s,
            f.snr_db,
            f.rssi_dbfs,
            f.raw.len(),
            f.raw.iter().take(16).map(|b| format!("{b:02x}")).collect::<String>()
        );
    }
}

/// The exact `(negative rail, positive rail)` a full-scale ADC sample
/// converts to for `fmt`, straight out of `FileIqSource`'s conversion
/// (`crates/xng-sdr/src/file.rs`): `None` when the format has no meaningful
/// clipping rail.
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
/// bit-for-bit and an exact compare is safe (no epsilon needed).
fn clip_rails(fmt: IqFormat) -> Option<(f32, f32)> {
    match fmt {
        IqFormat::Cu8 => Some((-1.0, 1.0)),
        IqFormat::Cs8 => Some((-1.0, 127.0 / 128.0)),
        IqFormat::Cs16 => Some((-1.0, 32_767.0 / 32_768.0)),
        IqFormat::Cf32 => None,
    }
}

/// Wideband-stream capture health: mean power (dBFS), DC spike, and ADC
/// clipping against the format's full-scale rails (see [`clip_rails`]); not
/// meaningful for cf32.
struct Health {
    rails: Option<(f32, f32)>,
    sum_sq: f64,
    n: u64,
    clip_lo: u64,
    clip_hi: u64,
    dc_sum_re: f64,
    dc_sum_im: f64,
}

impl Health {
    fn new(fmt: IqFormat) -> Self {
        Self {
            rails: clip_rails(fmt),
            sum_sq: 0.0,
            n: 0,
            clip_lo: 0,
            clip_hi: 0,
            dc_sum_re: 0.0,
            dc_sum_im: 0.0,
        }
    }

    fn update(&mut self, block: &[Complex<f32>]) {
        for s in block {
            self.sum_sq += (s.re as f64).powi(2) + (s.im as f64).powi(2);
            self.dc_sum_re += s.re as f64;
            self.dc_sum_im += s.im as f64;
            if let Some((neg, pos)) = self.rails {
                if s.re <= neg || s.im <= neg {
                    self.clip_lo += 1;
                }
                if s.re >= pos || s.im >= pos {
                    self.clip_hi += 1;
                }
            }
        }
        self.n += block.len() as u64;
    }

    fn report(&self) -> String {
        let mean_power = self.sum_sq / self.n.max(1) as f64;
        let rms_dbfs = 10.0 * mean_power.log10();
        let dc_re = self.dc_sum_re / self.n.max(1) as f64;
        let dc_im = self.dc_sum_im / self.n.max(1) as f64;
        let dc_mag_db = 10.0 * (dc_re * dc_re + dc_im * dc_im).log10();
        let clip = match self.rails {
            Some((neg, pos)) => {
                format!(
                    "clip_lo(<={neg:+.6})={}  clip_hi(>={pos:+.6})={}",
                    self.clip_lo, self.clip_hi
                )
            }
            None => "clip=n/a (cf32)".to_string(),
        };
        format!(
            "rms={rms_dbfs:>6.1} dBFS  dc=({dc_re:+.4},{dc_im:+.4}) |dc|={dc_mag_db:>6.1} dBFS  {clip}"
        )
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path =
        args.next().expect("usage: vdl2_rf_compare <file> <fmt> <rate> <center_hz> <ch_mhz,...>");
    let fmt_s = args.next().expect("fmt: cu8|cs16|cf32");
    let rate: f64 = args.next().expect("rate").parse().expect("rate must be a number");
    let center_hz: f64 =
        args.next().expect("center_hz").parse().expect("center_hz must be a number");
    let ch_s = args.next().expect("ch_mhz,...");

    let fmt: IqFormat = fmt_s.parse().expect("fmt must be cu8, cs16 or cf32");
    let ch_mhz: Vec<f64> =
        ch_s.split(',').map(|s| s.trim().parse().expect("channel MHz")).collect();
    let offsets: Vec<f64> = ch_mhz.iter().map(|&m| m * 1e6 - center_hz).collect();
    let n_ch = offsets.len();

    println!("{path} ({fmt_s}, {rate} S/s, center {:.4} MHz, {} channels)", center_hz / 1e6, n_ch);
    for (m, o) in ch_mhz.iter().zip(&offsets) {
        println!("  channel {m:>8.3} MHz  offset {o:>+10.1} Hz");
    }

    let per_channel = FrontEnd::new(
        "per-channel",
        "one Vdl2ChannelDecoder per channel (old path)".to_string(),
        Under::PerChannel(
            offsets
                .iter()
                .map(|&o| Vdl2ChannelDecoder::new(rate, o).expect("per-channel decoder"))
                .collect(),
        ),
        n_ch,
    );
    let default_multi =
        Vdl2MultiChannelDecoder::new(rate, &offsets).expect("default multi decoder");
    let default_describe = default_multi.describe_front();
    let default = FrontEnd::new(
        "default",
        default_describe.clone(),
        Under::Multi(Box::new(default_multi)),
        n_ch,
    );
    let shared_multi =
        Vdl2MultiChannelDecoder::new_shared(rate, &offsets).expect("shared multi decoder");
    let shared_describe = shared_multi.describe_front();
    let shared =
        FrontEnd::new("shared", shared_describe, Under::Multi(Box::new(shared_multi)), n_ch);

    // The explicit 2x bank is a row of interest, not a precondition for the
    // rest of the comparison: some rate/offset sets the default's fallback
    // (`SharedDdc`) serves fine are rejected by the 2x channelizer outright
    // (see `ChannelizedDdc::build`'s per-channel edge check). Skip the row
    // rather than aborting the whole run when that happens.
    let mut fronts = vec![per_channel, default, shared];
    match Vdl2MultiChannelDecoder::new_channelized_with_oversample(
        rate,
        &offsets,
        Oversample::Twice,
    ) {
        Ok(explicit_2x) => {
            let explicit_2x_describe = explicit_2x.describe_front();
            if explicit_2x_describe == default_describe {
                println!(
                    "  (new_channelized_with_oversample(Twice) == default's front end: {explicit_2x_describe:?}; not run separately)"
                );
            } else {
                fronts.push(FrontEnd::new(
                    "channelized-2x",
                    explicit_2x_describe,
                    Under::Multi(Box::new(explicit_2x)),
                    n_ch,
                ));
            }
        }
        Err(e) => {
            println!("  (new_channelized_with_oversample(Twice) skipped: {e})");
        }
    }

    println!("\nfront ends under test:");
    for f in &fronts {
        println!("  {:<14} describe_front() = {}", f.name, f.describe);
    }

    let mut src = FileIqSource::open(&path, fmt, rate, center_hz as u64).expect("open capture");
    let mut block = vec![Complex::new(0.0f32, 0.0f32); BLOCK];
    let mut samples_seen: u64 = 0;
    let mut health = Health::new(fmt);
    let io_t0 = Instant::now();
    let mut blocks = 0usize;

    loop {
        let n = match src.read(&mut block) {
            Ok(n) => n,
            Err(SdrError::EndOfStream) => break,
            Err(e) => panic!("capture read failed: {e}"),
        };
        let chunk = &block[..n];
        samples_seen += n as u64;
        let now = samples_seen as f64 / rate;

        health.update(chunk);

        for f in &mut fronts {
            f.feed(chunk, now);
        }
        blocks += 1;
    }
    let io_wall = io_t0.elapsed();

    println!(
        "\nstreamed {samples_seen} samples ({:.1} s of capture) in {blocks} blocks, wall {:.2} s (includes decode)",
        samples_seen as f64 / rate,
        io_wall.as_secs_f64()
    );

    println!("\ncapture health (wideband): {}", health.report());

    println!("\nper-channel end-of-capture level (dBFS EMA of channel power; noise floor where no carrier):");
    let mut lvl_header = format!("{:<10}", "channel");
    for f in &fronts {
        lvl_header += &format!("{:>14}", f.name);
    }
    println!("{lvl_header}");
    for (i, m) in ch_mhz.iter().enumerate() {
        let mut row = format!("{:<10.3}", m);
        for f in &fronts {
            row += &format!("{:>14.1}", f.level_dbfs(i));
        }
        println!("{row}");
    }

    println!("\nframes per channel per front end:");
    let mut header = format!("{:<10}", "channel");
    for f in &fronts {
        header += &format!("{:>14}", f.name);
    }
    println!("{header}");
    for (i, m) in ch_mhz.iter().enumerate() {
        let mut row = format!("{:<10.3}", m);
        for f in &fronts {
            row += &format!("{:>14}", f.per_channel[i]);
        }
        println!("{row}");
    }
    let mut total_row = format!("{:<10}", "total");
    for f in &fronts {
        total_row += &format!("{:>14}", f.frames.len());
    }
    println!("{total_row}");

    println!("\ndecode CPU (process() time only):");
    for f in &fronts {
        println!("  {:<14} {:>10.2} s", f.name, f.cpu.as_secs_f64());
    }

    println!("\nset differences ((channel, AVLC raw bytes) identity):");
    for i in 0..fronts.len() {
        for j in (i + 1)..fronts.len() {
            let (a_only, ex_a, b_only, ex_b) = diff(&fronts[i].frames, &fronts[j].frames);
            println!(
                "  {} vs {}: unique to {}={}  unique to {}={}",
                fronts[i].name, fronts[j].name, fronts[i].name, a_only, fronts[j].name, b_only
            );
            print_examples(&format!("unique to {}", fronts[i].name), &ch_mhz, &ex_a);
            print_examples(&format!("unique to {}", fronts[j].name), &ch_mhz, &ex_b);
        }
    }
}
