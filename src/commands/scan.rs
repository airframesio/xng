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
pub(crate) fn plan(mode: Mode) -> (f64, Vec<u64>) {
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
        // Simplex ring-alert + primary messaging channels.
        Mode::Iridium => (2_400_000.0, k(&[1_626_271, 1_626_437, 1_626_104])),
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

/// Channels worth watching even when a short scan finds them quiet:
/// the worldwide/primary frequencies where traffic eventually shows up.
/// (A 90 s dwell routinely undersells a site — observed first-hand: the
/// channels busiest over 30 minutes had been scored quiet.)
pub(crate) fn core_channels(mode: Mode) -> Vec<u64> {
    let k = |v: &[u32]| v.iter().map(|&f| f as u64 * 1_000).collect::<Vec<u64>>();
    match mode {
        // Worldwide primary + the common US/EU secondaries.
        Mode::AcarsPoa => k(&[131_550, 130_025, 131_725, 131_125]),
        // The worldwide Common Signaling Channel + busiest secondary.
        Mode::Vdl2 => k(&[136_975, 136_650]),
        Mode::Ais => k(&[161_975, 162_025]),
        Mode::Adsb => k(&[1_090_000]),
        // Primary ring-alert channel.
        Mode::Iridium => k(&[1_626_271]),
        _ => Vec::new(),
    }
}

/// Per-channel passband the DDC must preserve, by mode.
pub(crate) fn passband(mode: Mode) -> f64 {
    match mode {
        Mode::Ais => xng_mode_ais::CHANNEL_PASSBAND_HZ,
        Mode::Vdl2 => xng_mode_vdl2::CHANNEL_PASSBAND_HZ,
        Mode::Hfdl => xng_mode_hfdl::CHANNEL_PASSBAND_HZ,
        Mode::StdC => xng_mode_stdc::CHANNEL_PASSBAND_HZ,
        Mode::Iridium => xng_mode_iridium::CHANNEL_PASSBAND_HZ,
        Mode::Adsb => 0.0,
        _ => xng_mode_acars::CHANNEL_PASSBAND_HZ,
    }
}

/// Per-channel decoder input rate, by mode: an acceptable capture rate
/// must be an integer multiple of this (the DDC decimates by integers).
pub(crate) fn channel_rate(mode: Mode) -> f64 {
    match mode {
        Mode::Ais => xng_mode_ais::CHANNEL_RATE,
        Mode::Vdl2 => xng_mode_vdl2::CHANNEL_RATE,
        Mode::Hfdl => xng_mode_hfdl::CHANNEL_RATE,
        Mode::StdC => xng_mode_stdc::CHANNEL_RATE,
        Mode::Iridium => xng_mode_iridium::CHANNEL_RATE,
        // Consumes the whole capture at its native rate.
        Mode::Adsb => 1.0,
        _ => xng_mode_acars::CHANNEL_RATE,
    }
}

/// Choose a capture rate the device can actually do: the smallest
/// advertised rate that divides the mode's channel rate cleanly (CPU
/// scales with rate). Falls back to the plan rate when nothing matches
/// or nothing was probed.
pub(crate) fn pick_auto_rate(advertised: &[u32], mode: Mode, plan_rate: f64) -> f64 {
    let ch = channel_rate(mode);
    let mut rates: Vec<u32> = advertised.to_vec();
    rates.sort_unstable();
    if rates.iter().any(|&r| r as f64 == plan_rate && (plan_rate / ch).fract().abs() < 1e-9) {
        return plan_rate;
    }
    rates
        .iter()
        .map(|&r| r as f64)
        .find(|r| (r / ch).fract().abs() < 1e-9)
        .unwrap_or(plan_rate)
}

/// Cluster channels into capture-width groups.
pub(crate) fn group_channels(channels: &[u64], sample_rate: f64, passband: f64) -> Vec<(u64, Vec<u64>)> {
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

/// Pick the capture window and channel set for zero-config tuning: the
/// densest window of the mode's plan, trimmed to a CPU budget with the
/// core (worldwide-primary) channels kept first, then nearest-to-center.
pub(crate) fn auto_window(mode: Mode, rate: f64, max_channels: usize) -> Option<(u64, Vec<u64>)> {
    let (_, plan_channels) = plan(mode);
    if plan_channels.is_empty() {
        return None;
    }
    let groups = group_channels(&plan_channels, rate, passband(mode));
    let (center, mut chans) = groups.into_iter().max_by_key(|(_, g)| g.len())?;
    if chans.len() > max_channels {
        let core = core_channels(mode);
        chans.sort_by_key(|f| {
            (!core.contains(f), (*f as i64 - center as i64).unsigned_abs())
        });
        chans.truncate(max_channels);
        chans.sort_unstable();
    }
    Some((center, chans))
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
    /// HFDL system tables decoded over the air during the scan.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    systables: Vec<serde_json::Value>,
}

/// Frequencies advertised in a decoded HFDL system table (Hz).
fn systable_freqs(table: &serde_json::Value) -> Vec<u64> {
    table["stations"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|gs| gs["frequencies"].as_array().into_iter().flatten())
        .filter_map(|f| f["freq_hz"].as_u64())
        .collect()
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
    // Plan rates target common hardware (RTL-SDR); devices with a fixed
    // rate list (Airspy) get the smallest advertised rate that divides
    // the mode's channel rate.
    let advertised = crate::probe_device_rates(sdr);
    for &mode in modes {
        let (plan_rate, channels) = plan(mode);
        if channels.is_empty() {
            tracing::warn!("no built-in frequency plan for mode {mode}; skipping");
            continue;
        }
        let rate = pick_auto_rate(&advertised, mode, plan_rate);
        if rate != plan_rate {
            tracing::info!("{mode}: device prefers {} S/s over the plan's {} S/s", rate as u64, plan_rate as u64);
        }
        let groups = group_channels(&channels, rate, passband(mode));
        groups_total += groups.len();
        plans.push((mode, rate, groups));
    }
    println!(
        "scanning {} group(s) across {} mode(s), {dwell_secs}s dwell each (~{}s total)",
        groups_total,
        plans.len(),
        groups_total as u64 * (dwell_secs + 2)
    );

    let mut systables: Vec<serde_json::Value> = Vec::new();
    for (mode, rate, groups) in plans {
        let mut known: std::collections::BTreeSet<u64> =
            groups.iter().flat_map(|(_, g)| g.iter().copied()).collect();
        let mut queue: std::collections::VecDeque<(u64, Vec<u64>)> = groups.into();
        while let Some((center, channels)) = queue.pop_front() {
            let span: Vec<String> =
                channels.iter().map(|&c| format!("{:.3}", c as f64 / 1e6)).collect();
            println!("→ {mode} @ {:.3} MHz: {}", center as f64 / 1e6, span.join(", "));
            match scan_group(sdr, gain, mode, rate, center, &channels, dwell_secs) {
                Ok((mut group_results, tables)) => {
                    results.append(&mut group_results);
                    // A decoded system table extends the plan with any
                    // frequencies the network advertises that we aren't
                    // already scanning.
                    for table in tables {
                        let fresh: Vec<u64> = systable_freqs(&table)
                            .into_iter()
                            .filter(|f| *f > 0 && known.insert(*f))
                            .collect();
                        println!(
                            "  system table v{}: {} station(s), {} new frequency(ies)",
                            table["version"],
                            table["stations"].as_array().map_or(0, |s| s.len()),
                            fresh.len()
                        );
                        if !fresh.is_empty() {
                            let passband = xng_mode_hfdl::CHANNEL_PASSBAND_HZ;
                            queue.extend(group_channels(&fresh, rate, passband));
                        }
                        systables.push(table);
                    }
                }
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
        let rate = pick_auto_rate(&advertised, mode, plan(mode).0);
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
        let report = ScanReport { dwell_secs, results: &results, proposals, systables };
        std::fs::write(path, serde_json::to_string_pretty(&report)?)?;
        println!("\nreport written to {}", path.display());
    }
    Ok(())
}

pub(crate) fn scan_group(
    sdr: &str,
    gain: Option<f64>,
    mode: Mode,
    rate: f64,
    center: u64,
    channels: &[u64],
    dwell_secs: u64,
) -> anyhow::Result<(Vec<ChannelResult>, Vec<serde_json::Value>)> {
    let (mut source, _) = crate::open_sdr(sdr, rate, center, gain)?;
    let cfg = SessionConfig {
        mode,
        center_hz: center,
        channels_hz: channels.to_vec(),
        station_ident: "XNG-SCAN".into(),
        sdr: None,
        receiver_pos: None,
        label_filter: Default::default(),
        demod_effort: runtime::DemodEffort::Live,
        outputs: runtime::OutputConfig {
            console: crate::outputs::console::ConsoleFormat::Pretty,
            jsonl: None,
            udp: vec![],
            asf2_grpc: None,
            asf2_quic: None,
            asf2_quic_trust: crate::outputs::asf2_quic::TrustMode::SystemRoots,
            metrics: None,
            sbs: None,
            beast: None,
            nmea_tcp: None,
            mqtt: None,
            mqtt_topic: "xng".into(),
        },
    };
    let decoders = runtime::build_decoders(rate, center, &cfg)?;
    let bus = MessageBus::new();
    let mut rx = bus.subscribe(); // counted via stats; drained for systables after the dwell
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
        None,
        Default::default(),
    )?;
    let _ = stop_thread.join();

    let mut systables = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        if let xng_types::MessageBody::Hfdl { kind, details } = &msg.body {
            if kind == "systable-complete" {
                systables.push(details.clone());
            }
        }
    }

    let levels = live.stats.lock().unwrap().clone();
    let results = stats
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
        .collect();
    Ok((results, systables))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_rate_prefers_smallest_divisible_advertised_rate() {
        // Airspy Mini advertises 6/3 Msps: VDL2 (50 kHz channels) and
        // ACARS (24 kHz) both divide 3 Msps, so the cheaper rate wins.
        assert_eq!(pick_auto_rate(&[6_000_000, 3_000_000], Mode::Vdl2, 2_400_000.0), 3_000_000.0);
        assert_eq!(pick_auto_rate(&[6_000_000, 3_000_000], Mode::AcarsPoa, 2_400_000.0), 3_000_000.0);
        // Device that advertises the plan rate keeps it.
        assert_eq!(pick_auto_rate(&[2_400_000], Mode::AcarsPoa, 2_400_000.0), 2_400_000.0);
        // Nothing probed (SoapySDR path): plan rate.
        assert_eq!(pick_auto_rate(&[], Mode::AcarsPoa, 2_400_000.0), 2_400_000.0);
    }

    #[test]
    fn auto_window_picks_densest_group_and_keeps_core_first() {
        // The ACARS plan spans ~2.7 MHz: at 2.4 MS/s it splits into two
        // windows, and the upper one (8 channels) must win.
        let (center, chans) = auto_window(Mode::AcarsPoa, 2_400_000.0, 64).unwrap();
        assert!(chans.len() >= 8, "{chans:?}");
        assert!(chans.contains(&131_550_000));
        assert!(center > 130_000_000 && center < 132_000_000);

        // A tight budget keeps the core channels in the window.
        let (_, capped) = auto_window(Mode::AcarsPoa, 2_400_000.0, 3).unwrap();
        assert_eq!(capped.len(), 3);
        assert!(capped.contains(&131_550_000), "core channel evicted: {capped:?}");
        assert!(capped.contains(&131_725_000), "core channel evicted: {capped:?}");
    }

    #[test]
    fn auto_window_handles_single_channel_modes() {
        let (center, chans) = auto_window(Mode::Adsb, 2_000_000.0, 8).unwrap();
        assert_eq!(chans, vec![1_090_000_000]);
        // Center is nudged off the channel to keep it away from DC.
        assert_ne!(center, 1_090_000_000);
    }

    #[test]
    fn auto_window_none_for_planless_modes() {
        assert!(auto_window(Mode::AeroL, 2_400_000.0, 8).is_none());
    }
}
