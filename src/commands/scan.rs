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
        // UAT 978 MHz is wideband like ADS-B (whole capture, single center).
        Mode::Uat => (2_400_000.0, k(&[978_000])),
        // COSPAS-SARSAT 406 MHz beacon channels.
        Mode::Sarsat => (2_400_000.0, k(&[406_025, 406_028, 406_037])),
        // DSC MF/HF distress + calling (the implemented 100 Bd FSK path).
        Mode::Dsc => (768_000.0, vec![2_187_500, 4_207_500, 6_312_000, 8_414_500, 12_577_000, 16_804_500]),
        // NAVTEX international (518), national (490), HF (4209.5 kHz).
        Mode::Navtex => (768_000.0, vec![518_000, 490_000, 4_209_500]),
        // Radiosonde common RS41 frequencies (400–406 MHz; sondes also hop).
        Mode::Sonde => (2_400_000.0, k(&[402_700, 404_000, 405_000])),
        // ADS-L on the EASA SRD860 868 MHz band.
        Mode::AdsL => (2_000_000.0, k(&[868_200])),
        // ATCS rail data radio (900 MHz band, representative channels).
        Mode::Atcs => (2_400_000.0, k(&[896_000, 900_000])),
        // APRS / AX.25 packet — the whole 2-meter channel cluster fits one
        // 2.4 MHz window: 144.390 (NA/SA), 144.575 (NZ), 144.640 (CN/TW),
        // 144.660 (JP), 144.800 (EU/RU), 144.990 (NA event), 145.175 (AU), and
        // 145.825 (ISS / satellite digipeat). 70cm 446.100 + HF 300-baud APRS
        // (10.1476 / 14.1030 / 29.250 MHz) are separate bands/modulation.
        Mode::Aprs => (
            2_400_000.0,
            k(&[144_390, 144_575, 144_640, 144_660, 144_800, 144_990, 145_175, 145_825]),
        ),
        // POCSAG paging: representative US (929/931) + EU (466) channels.
        Mode::Pocsag => (2_400_000.0, k(&[929_000, 931_000, 466_075])),
        // Rail EOT/HOT telemetry: EOT→HOT 457.9375, HOT→EOT 452.9375 MHz.
        Mode::Eot => (2_400_000.0, k(&[457_937, 452_937])),
        // FLEX paging: representative US 929/931 MHz channels.
        Mode::Flex => (2_400_000.0, k(&[929_000, 931_000])),
        // VDES ASM 1/2 (the former AIS 27/28 region): 161.950 / 162.000 MHz.
        Mode::Vdes => (2_400_000.0, k(&[161_950, 162_000])),
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
        Mode::Uat => k(&[978_000]),
        Mode::Sarsat => k(&[406_025]),
        Mode::Dsc => vec![2_187_500],
        Mode::Navtex => vec![518_000],
        Mode::AdsL => k(&[868_200]),
        Mode::Aprs => k(&[144_390]),
        Mode::Pocsag => k(&[929_000]),
        Mode::Eot => k(&[457_937]),
        Mode::Flex => k(&[929_000]),
        Mode::Vdes => k(&[161_950]),
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
        Mode::Adsb | Mode::Uat => 0.0,
        Mode::Sarsat => xng_mode_sarsat::CHANNEL_PASSBAND_HZ,
        Mode::Dsc => xng_mode_dsc::CHANNEL_PASSBAND_HZ,
        Mode::Navtex => xng_mode_navtex::CHANNEL_PASSBAND_HZ,
        Mode::Sonde => xng_mode_sonde::CHANNEL_PASSBAND_HZ,
        Mode::AdsL => xng_mode_adsl::CHANNEL_PASSBAND_HZ,
        Mode::Atcs => xng_mode_atcs::CHANNEL_PASSBAND_HZ,
        Mode::Aprs => xng_mode_aprs::CHANNEL_PASSBAND_HZ,
        Mode::Pocsag => xng_mode_pocsag::CHANNEL_PASSBAND_HZ,
        Mode::Eot => xng_mode_eot::CHANNEL_PASSBAND_HZ,
        Mode::Flex => xng_mode_flex::CHANNEL_PASSBAND_HZ,
        Mode::Vdes => xng_mode_vdes::CHANNEL_PASSBAND_HZ,
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
        // Consumes the whole capture at its native rate (wideband).
        Mode::Adsb | Mode::Uat => 1.0,
        Mode::Sarsat => xng_mode_sarsat::CHANNEL_RATE,
        Mode::Dsc => xng_mode_dsc::CHANNEL_RATE,
        Mode::Navtex => xng_mode_navtex::CHANNEL_RATE,
        Mode::Sonde => xng_mode_sonde::CHANNEL_RATE,
        Mode::AdsL => xng_mode_adsl::CHANNEL_RATE,
        Mode::Atcs => xng_mode_atcs::CHANNEL_RATE,
        Mode::Aprs => xng_mode_aprs::CHANNEL_RATE,
        Mode::Pocsag => xng_mode_pocsag::CHANNEL_RATE,
        Mode::Eot => xng_mode_eot::CHANNEL_RATE,
        Mode::Flex => xng_mode_flex::CHANNEL_RATE,
        Mode::Vdes => xng_mode_vdes::CHANNEL_RATE,
        _ => xng_mode_acars::CHANNEL_RATE,
    }
}

/// What an SDR can do for sample rate. Native backends (airspy/airspyhf)
/// report an exact discrete set; SoapySDR devices (RTL-SDR, HackRF, SDRplay…)
/// report inclusive ranges; `Unknown` when we couldn't ask — callers then
/// trust the mode's plan rate.
#[derive(Debug, Clone)]
pub(crate) enum RateCaps {
    Discrete(Vec<u32>),
    /// `(min, max, step)` Hz; `step == 0.0` means continuous.
    Ranges(Vec<(f64, f64, f64)>),
    Unknown,
}

/// Pick a capture rate the device supports for `mode`, given what it can do.
/// Prefers the mode's plan rate; otherwise the smallest supported rate that is
/// **at least** the plan rate and (for DDC modes) a clean integer multiple of
/// the per-channel rate. The "at least" floor keeps a device with a wide rate
/// menu (e.g. RTL-SDR's 0.25–3.2 MS/s) from stranding a mode at a too-narrow
/// capture. Falls back to the plan rate when nothing fits or support is
/// unknown.
pub(crate) fn choose_rate(caps: &RateCaps, mode: Mode, plan_rate: f64) -> f64 {
    match caps {
        RateCaps::Unknown => plan_rate,
        RateCaps::Discrete(rates) => pick_auto_rate(rates, mode, plan_rate),
        RateCaps::Ranges(ranges) => {
            pick_auto_rate(&candidates_from_ranges(ranges, mode), mode, plan_rate)
        }
    }
}

/// Expand advertised rate ranges into discrete candidate rates aligned to the
/// mode's per-channel rate (so each is a clean DDC multiple needing no
/// resampling). Whole-capture modes (channel rate 1.0: Mode S) have no DDC
/// grid, so a coarse 250 kHz step enumerates sensible widths. The range
/// endpoints are added too: if a (narrow or discrete-point) device offers no
/// aligned rate, the DDC can still resample from an endpoint.
fn candidates_from_ranges(ranges: &[(f64, f64, f64)], mode: Mode) -> Vec<u32> {
    let ch = channel_rate(mode);
    let unit = if ch > 1.0 { ch } else { 250_000.0 };
    let mut out: Vec<u32> = Vec::new();
    for &(lo, hi, _step) in ranges {
        if hi < lo {
            continue;
        }
        // First multiple of `unit` at or above the range floor, then step up.
        let mut r = (lo / unit).ceil() * unit;
        let mut n = 0;
        while r <= hi + 1e-6 && n < 4096 {
            out.push(r as u32);
            r += unit;
            n += 1;
        }
        // Endpoints as resampling fallbacks (also captures min==max discrete
        // points some SoapySDR drivers report as zero-width ranges).
        out.push(lo.round() as u32);
        out.push(hi.round() as u32);
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Mode S decodes from the raw capture by pulse position, so an **even
/// integer** number of samples per µs (2, 4, 6, 10 MS/s …) takes the fast,
/// higher-recall integer demod path; odd or fractional rates (2.5, 3 MS/s)
/// fall to a slower fractional path. Other modes channelize through a DDC and
/// only care that the rate is an integer multiple of their channel rate, so
/// this preference applies to Mode S alone.
pub(crate) fn prefers_even_integer_rate(mode: Mode) -> bool {
    matches!(mode, Mode::Adsb | Mode::Uat)
}

/// True when `rate` gives an even integer number of samples per µs.
fn is_even_integer_rate(rate: f64) -> bool {
    let spu = rate / 1e6;
    (spu - spu.round()).abs() < 1e-9 && (spu.round() as i64) % 2 == 0
}

/// Choose a capture rate from a discrete advertised set (see [`choose_rate`]).
pub(crate) fn pick_auto_rate(advertised: &[u32], mode: Mode, plan_rate: f64) -> f64 {
    if advertised.is_empty() {
        return plan_rate;
    }
    let ch = channel_rate(mode);
    let divides = |r: f64| (r / ch).fract().abs() < 1e-9;
    let mut rates: Vec<f64> = advertised.iter().map(|&r| r as f64).collect();
    rates.sort_by(|a, b| a.partial_cmp(b).unwrap());
    // The plan rate is authoritative when the device offers it exactly (the
    // mode plans are even-integer rates, so this already takes the fast path).
    if rates.iter().any(|&r| (r - plan_rate).abs() < 1e-9) {
        return plan_rate;
    }
    // Mode S: prefer the smallest even-integer-samples/µs rate at or above the
    // plan (the fast integer demod path) over a merely-smaller fractional one.
    // On an Airspy this picks 10 MS/s (R2) / 6 MS/s (Mini) instead of the
    // 2.5/3 MS/s fractional rate — the whole point of an Airspy for ADS-B.
    // Pass an explicit -r to override (e.g. -r 2500000 for lighter CPU).
    if prefers_even_integer_rate(mode) {
        if let Some(&r) = rates
            .iter()
            .find(|&&r| r >= plan_rate - 1e-9 && divides(r) && is_even_integer_rate(r))
        {
            return r;
        }
    }
    // Prefer a clean integer-ratio rate at or above the plan: the DDC
    // integer-decimates, no resampling, and never narrower than the mode needs.
    if let Some(&r) = rates.iter().find(|&&r| r >= plan_rate - 1e-9 && divides(r)) {
        return r;
    }
    // No clean multiple at or above the plan: take the smallest supported rate
    // there and let the DDC resample to the channel rate. This is what makes,
    // e.g., a full multi-mode station work on an Airspy R2 — its 10/2.5 MS/s
    // are integer multiples of neither the 24 kHz (ACARS) nor 48 kHz (AIS)
    // channel rate, so without resampling those modes had no usable rate.
    if let Some(&r) = rates.iter().find(|&&r| r >= plan_rate - 1e-9) {
        return r;
    }
    // Nothing at or above the plan: the widest rate offered (DDC resamples),
    // preferring a clean multiple if one exists.
    rates
        .iter()
        .rev()
        .copied()
        .find(|&r| divides(r))
        .or_else(|| rates.last().copied())
        .unwrap_or(plan_rate)
}

/// A one-line hint to show after auto-selecting a rate, when the user might
/// reasonably prefer a different one. Currently: Mode S on a device whose
/// fast integer-path rate sits well above the plan (an Airspy), where a
/// lighter fractional capture is a sensible alternative via an explicit `-r`.
pub(crate) fn rate_choice_hint(mode: Mode, chosen: f64, plan_rate: f64) -> Option<String> {
    if prefers_even_integer_rate(mode) && chosen > plan_rate * 1.5 {
        Some(format!(
            "ADS-B at {:.1} MS/s uses the high-resolution integer demod path; \
             pass an explicit -r for a lighter capture (more CPU headroom, \
             slightly lower recall)",
            chosen / 1e6
        ))
    } else {
        None
    }
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
            // Avoid a channel landing exactly at DC — except whole-capture
            // modes (passband 0: Mode S), whose core consumes the entire
            // capture and requires the channel AT the center. A 25 kHz nudge
            // there makes the offset non-zero and the decoder rejects it.
            let center = if passband > 0.0 && g.iter().any(|&c| c == center) {
                center + 25_000
            } else {
                center
            };
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
    // Probe once: pick each mode's rate from what this device actually
    // supports (Airspy's fixed list, a SoapySDR device's ranges), preferring
    // the plan rate.
    let caps = crate::probe_rate_caps(sdr);
    for &mode in modes {
        let (plan_rate, channels) = plan(mode);
        if channels.is_empty() {
            tracing::warn!("no built-in frequency plan for mode {mode}; skipping");
            continue;
        }
        let rate = choose_rate(&caps, mode, plan_rate);
        if rate != plan_rate {
            tracing::info!("{mode}: using {} S/s (device does not offer the plan's {} S/s)", rate as u64, plan_rate as u64);
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
        let rate = choose_rate(&caps, mode, plan(mode).0);
        let freqs: Vec<u64> = active.iter().map(|r| r.frequency_hz).collect();
        let center = (freqs.iter().min().unwrap() + freqs.iter().max().unwrap()) / 2;
        // Whole-capture modes (passband 0) must keep the channel at center.
        let center = if passband(mode) > 0.0 && freqs.contains(&center) {
            center + 25_000
        } else {
            center
        };
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
        ais_filter: Default::default(),
        demod_effort: runtime::DemodEffort::Live,
        max_ppm: None,
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
            nmea_udp: None,
            nmea_tag_blocks: false,
            gsmtap: None,
            iridium_satmap: None,
            http: None,
            mqtt: None,
            mqtt_topic: "xng".into(),
            airframes: None,
            own_ship_mmsi: None,
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
        // Nothing probed: plan rate.
        assert_eq!(pick_auto_rate(&[], Mode::AcarsPoa, 2_400_000.0), 2_400_000.0);
    }

    #[test]
    fn choose_rate_from_rtl_ranges() {
        // RTL-SDR advertises ~0.225–0.3 and ~0.9–3.2 MS/s (two continuous
        // ranges with a gap), reported as ranges, not a discrete list.
        let rtl = RateCaps::Ranges(vec![
            (225_001.0, 300_000.0, 0.0),
            (900_001.0, 3_200_000.0, 0.0),
        ]);
        // ADS-B (whole capture): plan 2.0 MS/s is in range → kept exactly so
        // the channel stays at the capture center.
        assert_eq!(choose_rate(&rtl, Mode::Adsb, 2_000_000.0), 2_000_000.0);
        // ACARS: 2.4 MS/s is supported and a clean 24 kHz multiple → kept.
        assert_eq!(choose_rate(&rtl, Mode::AcarsPoa, 2_400_000.0), 2_400_000.0);
        // HFDL: plan 768 kS/s lands in the RTL gap; pick the smallest
        // supported 12 kHz-multiple at or above it (never narrower).
        let r = choose_rate(&rtl, Mode::Hfdl, 768_000.0);
        assert!((r / 12_000.0).fract().abs() < 1e-9, "not a 12 kHz multiple: {r}");
        assert!(r >= 900_001.0, "must land in the supported high range: {r}");
    }

    #[test]
    fn choose_rate_unknown_keeps_plan() {
        assert_eq!(choose_rate(&RateCaps::Unknown, Mode::AcarsPoa, 2_400_000.0), 2_400_000.0);
    }

    #[test]
    fn choose_rate_floor_avoids_too_narrow_capture() {
        // A device that also offers sub-plan rates must not strand the mode
        // at a too-narrow capture just because the small rate "fits".
        let caps = RateCaps::Discrete(vec![250_000, 1_000_000, 2_000_000, 2_400_000]);
        assert_eq!(choose_rate(&caps, Mode::Adsb, 2_000_000.0), 2_000_000.0);
        assert_eq!(choose_rate(&caps, Mode::AcarsPoa, 2_400_000.0), 2_400_000.0);
    }

    #[test]
    fn adsb_prefers_integer_path_rate_on_airspy() {
        // Airspy R2 (10 / 2.5 MS/s): 2.5 gives fractional samples/µs, 10 is
        // even-integer → ADS-B takes 10 for the fast integer demod path.
        let r2 = RateCaps::Discrete(vec![10_000_000, 2_500_000]);
        assert_eq!(choose_rate(&r2, Mode::Adsb, 2_000_000.0), 10_000_000.0);
        // Airspy Mini (6 / 3 MS/s): 3 is odd (fractional), 6 is even → 6.
        let mini = RateCaps::Discrete(vec![6_000_000, 3_000_000]);
        assert_eq!(choose_rate(&mini, Mode::Adsb, 2_000_000.0), 6_000_000.0);
        // RTL-SDR keeps 2.0 MS/s: even-integer and already the plan rate.
        let rtl = RateCaps::Ranges(vec![
            (225_001.0, 300_000.0, 0.0),
            (900_001.0, 3_200_000.0, 0.0),
        ]);
        assert_eq!(choose_rate(&rtl, Mode::Adsb, 2_000_000.0), 2_000_000.0);
    }

    #[test]
    fn integer_path_preference_is_adsb_only() {
        // Iridium on an Airspy R2 still takes the smallest clean 250 kHz
        // multiple (2.5 MS/s) — the even-integer preference is Mode S only.
        let r2 = RateCaps::Discrete(vec![10_000_000, 2_500_000]);
        assert_eq!(choose_rate(&r2, Mode::Iridium, 2_400_000.0), 2_500_000.0);
    }

    #[test]
    fn airspy_r2_runs_all_modes_via_resampling() {
        // R2 (10 / 2.5 MS/s) divides neither 24 kHz (ACARS), 48 kHz (AIS) nor
        // 12 kHz (StdC); without resampling these had no usable rate. Now the
        // picker returns the smallest rate >= plan and the DDC resamples.
        let r2 = RateCaps::Discrete(vec![10_000_000, 2_500_000]);
        assert_eq!(choose_rate(&r2, Mode::AcarsPoa, 2_400_000.0), 2_500_000.0);
        assert_eq!(choose_rate(&r2, Mode::Ais, 2_400_000.0), 2_500_000.0);
        assert_eq!(choose_rate(&r2, Mode::StdC, 2_400_000.0), 2_500_000.0);
        // VDL2 (50 kHz) and Iridium (250 kHz) DO divide 2.5 MS/s → clean
        // integer path, same rate, no resampling.
        assert_eq!(choose_rate(&r2, Mode::Vdl2, 2_400_000.0), 2_500_000.0);
        assert_eq!(choose_rate(&r2, Mode::Iridium, 2_400_000.0), 2_500_000.0);
        // ADS-B takes the even-integer 10 MS/s (integer demod path).
        assert_eq!(choose_rate(&r2, Mode::Adsb, 2_000_000.0), 10_000_000.0);
    }

    #[test]
    fn ranges_resample_when_no_aligned_rate_fits() {
        // A device whose only band is a narrow window containing no 24 kHz
        // multiple still yields a usable (resampled) rate from an endpoint.
        let narrow = RateCaps::Ranges(vec![(2_450_000.0, 2_460_000.0, 0.0)]);
        let r = choose_rate(&narrow, Mode::AcarsPoa, 2_400_000.0);
        assert!((2_450_000.0..=2_460_000.0).contains(&r), "got {r}");
    }

    #[test]
    fn rate_hint_only_for_elevated_adsb() {
        assert!(rate_choice_hint(Mode::Adsb, 10_000_000.0, 2_000_000.0).is_some());
        // Not elevated above the plan → no hint.
        assert!(rate_choice_hint(Mode::Adsb, 2_000_000.0, 2_000_000.0).is_none());
        // Not Mode S → no hint.
        assert!(rate_choice_hint(Mode::AcarsPoa, 6_000_000.0, 2_400_000.0).is_none());
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
        // Mode S consumes the whole capture: the channel must sit AT the
        // center (a DC nudge would make the offset non-zero and the core
        // rejects it — "Mode S uses the whole capture").
        assert_eq!(center, 1_090_000_000);
    }

    #[test]
    fn group_channels_keeps_whole_capture_channel_at_center() {
        // ADS-B (passband 0): the single 1090 MHz channel must land exactly
        // at the capture center, not nudged 25 kHz off DC.
        let groups = group_channels(&[1_090_000_000], 2_000_000.0, passband(Mode::Adsb));
        assert_eq!(groups, vec![(1_090_000_000, vec![1_090_000_000])]);
        // A DDC mode whose only channel is the midpoint still nudges off DC.
        let groups = group_channels(&[131_550_000], 2_400_000.0, passband(Mode::AcarsPoa));
        assert_eq!(groups[0].0, 131_575_000);
    }

    #[test]
    fn auto_window_none_for_planless_modes() {
        assert!(auto_window(Mode::AeroL, 2_400_000.0, 8).is_none());
    }
}
