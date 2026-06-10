//! `xng scan` — auto-scanning site survey: steps the SDR across each
//! mode's known frequency plan, runs the real decoders as signature
//! detectors for a dwell period, and proposes a decode configuration.
//! (Foundation for Airwaves OS site surveys.)

use crate::bus::MessageBus;
use crate::runtime::{self, SessionConfig};
use serde::Serialize;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};
use xng_types::{Mode, StationIdentity};

/// Built-in frequency plans (kHz). Curated, worldwide-common channels;
/// the HFDL list follows the public system table.
fn plan(mode: Mode) -> (f64, Vec<u64>) {
    let k = |v: &[u32]| v.iter().map(|&f| f as u64 * 1_000).collect::<Vec<u64>>();
    match mode {
        Mode::AcarsPoa => (
            2_400_000.0,
            k(&[129_125, 130_025, 130_425, 130_450, 131_125, 131_425, 131_475,
                131_525, 131_550, 131_725, 131_825, 131_850]),
        ),
        Mode::Vdl2 => (
            2_400_000.0,
            k(&[136_650, 136_725, 136_775, 136_825, 136_875, 136_925, 136_975]),
        ),
        Mode::Ais => (2_400_000.0, k(&[161_975, 162_025])),
        Mode::Adsb => (2_000_000.0, k(&[1_090_000])),
        Mode::StdC => (2_400_000.0, k(&[1_537_100, 1_537_700, 1_541_450])),
        Mode::Hfdl => (
            768_000.0,
            k(&[2_941, 2_944, 2_992, 2_998, 3_007, 3_016, 3_455, 3_497, 4_654,
                4_660, 4_681, 4_687, 5_502, 5_508, 5_514, 5_529, 5_538, 5_544,
                5_547, 5_583, 5_589, 5_622, 5_652, 5_655, 5_720, 6_529, 6_532,
                6_535, 6_559, 6_565, 6_589, 6_596, 6_619, 6_646, 6_652, 6_661,
                6_712, 8_825, 8_834, 8_843, 8_885, 8_886, 8_894, 8_912, 8_921,
                8_927, 8_936, 8_939, 8_942, 8_948, 8_957, 8_977, 10_027,
                10_030, 10_060, 10_063, 10_066, 10_075, 10_081, 10_084,
                10_087, 10_093, 11_184, 11_306, 11_312, 11_318, 11_321,
                11_327, 11_348, 11_354, 11_384, 11_387, 13_264, 13_270,
                13_276, 13_303, 13_312, 13_315, 13_321, 13_324, 13_342,
                13_351, 13_354, 17_901, 17_912, 17_916, 17_919, 17_922,
                17_928, 17_934, 17_958, 17_967, 17_985, 21_928, 21_931,
                21_934, 21_937, 21_949, 21_955, 21_982, 21_990, 21_997]),
        ),
        _ => (2_400_000.0, vec![]),
    }
}

/// Cluster channels into capture-width groups.
fn group_channels(channels: &[u64], sample_rate: f64, passband: f64) -> Vec<(u64, Vec<u64>)> {
    let usable = sample_rate * 0.8 - 2.0 * passband;
    let mut sorted = channels.to_vec();
    sorted.sort_unstable();
    let mut groups = Vec::new();
    let mut cur: Vec<u64> = Vec::new();
    for &f in &sorted {
        if cur.is_empty() || (f - cur[0]) as f64 <= usable {
            cur.push(f);
        } else {
            groups.push(cur.clone());
            cur = vec![f];
        }
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    groups
        .into_iter()
        .map(|g| {
            let center = (g[0] + g[g.len() - 1]) / 2;
            // Avoid a channel landing exactly at DC.
            let center = if g.iter().any(|&c| c == center) { center + 25_000 } else { center };
            (center, g)
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelResult {
    pub mode: String,
    pub frequency_hz: u64,
    pub frames: u64,
    pub crc_ok: u64,
    pub level_dbfs: f32,
    pub verdict: &'static str,
}

#[derive(Serialize)]
struct ScanReport<'a> {
    dwell_secs: u64,
    results: &'a [ChannelResult],
    proposals: Vec<String>,
}

pub fn run(
    sdr: &str,
    gain: Option<f64>,
    modes: &[Mode],
    dwell_secs: u64,
    out_json: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let mut results: Vec<ChannelResult> = Vec::new();
    let mut groups_total = 0usize;
    let mut plans = Vec::new();
    for &mode in modes {
        let (rate, channels) = plan(mode);
        if channels.is_empty() {
            tracing::warn!("no built-in frequency plan for mode {mode}; skipping");
            continue;
        }
        let passband = match mode {
            Mode::Ais => xng_mode_ais::CHANNEL_PASSBAND_HZ,
            Mode::Vdl2 => xng_mode_vdl2::CHANNEL_PASSBAND_HZ,
            Mode::Hfdl => xng_mode_hfdl::CHANNEL_PASSBAND_HZ,
            Mode::StdC => xng_mode_stdc::CHANNEL_PASSBAND_HZ,
            Mode::Adsb => 0.0,
            _ => xng_mode_acars::CHANNEL_PASSBAND_HZ,
        };
        let groups = group_channels(&channels, rate, passband);
        groups_total += groups.len();
        plans.push((mode, rate, groups));
    }
    println!(
        "scanning {} group(s) across {} mode(s), {dwell_secs}s dwell each (~{}s total)",
        groups_total,
        plans.len(),
        groups_total as u64 * (dwell_secs + 2)
    );

    for (mode, rate, groups) in plans {
        for (center, channels) in groups {
            let span: Vec<String> =
                channels.iter().map(|&c| format!("{:.3}", c as f64 / 1e6)).collect();
            println!("→ {mode} @ {:.3} MHz: {}", center as f64 / 1e6, span.join(", "));
            match scan_group(sdr, gain, mode, rate, center, &channels, dwell_secs) {
                Ok(mut group_results) => results.append(&mut group_results),
                Err(e) => println!("  group skipped: {e}"),
            }
        }
    }

    // Verdicts: ACTIVE (decodes), SIGNAL (level well above the mode's
    // median), quiet.
    let mut by_mode: std::collections::HashMap<String, Vec<f32>> = Default::default();
    for r in &results {
        by_mode.entry(r.mode.clone()).or_default().push(r.level_dbfs);
    }
    let medians: std::collections::HashMap<String, f32> = by_mode
        .into_iter()
        .map(|(m, mut v)| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let med = v[v.len() / 2];
            (m, med)
        })
        .collect();
    for r in &mut results {
        r.verdict = if r.crc_ok > 0 {
            "ACTIVE"
        } else if r.frames > 0 {
            "partial"
        } else if r.level_dbfs > medians.get(&r.mode).copied().unwrap_or(0.0) + 6.0 {
            "signal?"
        } else {
            "quiet"
        };
    }

    // Report.
    println!("\n{:<8} {:<12} {:>7} {:>7} {:>8}  verdict", "mode", "freq MHz", "frames", "ok", "dBFS");
    for r in &results {
        if r.verdict == "quiet" {
            continue;
        }
        println!(
            "{:<8} {:<12.3} {:>7} {:>7} {:>8.1}  {}",
            r.mode,
            r.frequency_hz as f64 / 1e6,
            r.frames,
            r.crc_ok,
            r.level_dbfs,
            r.verdict
        );
    }
    let quiet = results.iter().filter(|r| r.verdict == "quiet").count();
    println!("({quiet} quiet channel(s) omitted)");

    // Proposals: one listen command per mode with active channels.
    let mut proposals = Vec::new();
    for &mode in modes {
        let active: Vec<&ChannelResult> = results
            .iter()
            .filter(|r| r.mode == mode.as_str() && r.verdict == "ACTIVE")
            .collect();
        if active.is_empty() {
            continue;
        }
        let (rate, _) = plan(mode);
        let freqs: Vec<u64> = active.iter().map(|r| r.frequency_hz).collect();
        let center = (freqs.iter().min().unwrap() + freqs.iter().max().unwrap()) / 2;
        let center = if freqs.contains(&center) { center + 25_000 } else { center };
        let chans: Vec<String> =
            freqs.iter().map(|&f| format!("{:.3}", f as f64 / 1e6)).collect();
        proposals.push(format!(
            "xng listen --sdr '{sdr}' --mode {} -r {} -c {:.3}M --channels {}",
            mode,
            rate as u64,
            center as f64 / 1e6,
            chans.join(",")
        ));
    }
    if proposals.is_empty() {
        println!("\nno active channels found — try a longer dwell or check the antenna");
    } else {
        println!("\nproposed configuration:");
        for p in &proposals {
            println!("  {p}");
        }
    }

    if let Some(path) = out_json {
        let report = ScanReport { dwell_secs, results: &results, proposals };
        std::fs::write(path, serde_json::to_string_pretty(&report)?)?;
        println!("\nreport written to {}", path.display());
    }
    Ok(())
}

fn scan_group(
    sdr: &str,
    gain: Option<f64>,
    mode: Mode,
    rate: f64,
    center: u64,
    channels: &[u64],
    dwell_secs: u64,
) -> anyhow::Result<Vec<ChannelResult>> {
    let mut source = crate::open_sdr(sdr, rate, center, gain)?;
    let cfg = SessionConfig {
        mode,
        center_hz: center,
        channels_hz: channels.to_vec(),
        station_ident: "XNG-SCAN".into(),
        sdr: None,
        outputs: runtime::OutputConfig {
            console: crate::outputs::console::ConsoleFormat::Pretty,
            jsonl: None,
            udp: vec![],
            asf2_grpc: None,
            asf2_quic: None,
            asf2_quic_trust: crate::outputs::asf2_quic::TrustMode::SystemRoots,
            metrics: None,
        },
    };
    let decoders = runtime::build_decoders(rate, center, &cfg)?;
    let bus = MessageBus::new();
    let _keep = bus.subscribe(); // keep the bus open; messages are counted via stats
    let live = runtime::LiveState::new();
    let stop = Arc::new(AtomicBool::new(false));
    let station = StationIdentity::new("XNG-SCAN");

    let deadline = Instant::now() + Duration::from_secs(dwell_secs);
    let stop_thread = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        })
    };
    let stats = runtime::decode_loop(
        &mut *source,
        decoders,
        station,
        None,
        bus,
        stop,
        Some((live.clone(), center, rate)),
    )?;
    let _ = stop_thread.join();

    let levels = live.stats.lock().unwrap().clone();
    Ok(stats
        .iter()
        .enumerate()
        .map(|(i, &(freq, frames, ok))| ChannelResult {
            mode: mode.as_str().to_string(),
            frequency_hz: freq,
            frames,
            crc_ok: ok,
            level_dbfs: levels.get(i).map(|l| l.3).unwrap_or(-120.0),
            verdict: "quiet",
        })
        .collect())
}
