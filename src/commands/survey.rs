//! `xng survey` — official soak test for one mode: monitor and decode every
//! channel that fits the SDR's bandwidth for a sustained period, then report
//! per-channel statistics (frames, CRC, levels, rates) plus gain/SDR advice.
//!
//! Where `xng scan` answers "what's receivable here?" with a short dwell per
//! group, `survey` answers "how well does this site/configuration actually
//! perform?" — long dwells, full-plan coverage (rotating capture windows when
//! the plan exceeds the bandwidth), interim progress tables, and an optional
//! gain sweep that picks the best setting empirically.

use crate::bus::MessageBus;
use crate::commands::scan;
use crate::outputs::console::{ConsoleFormat, format_message};
use crate::runtime::{self, SessionConfig};
use serde::Serialize;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use xng_types::{Mode, StationIdentity};

pub struct SurveyOpts {
    pub sdr: String,
    pub gain: Option<f64>,
    pub mode: Mode,
    pub sample_rate: Option<f64>,
    /// Explicit channels (Hz); full built-in plan when empty.
    pub channels: Vec<u64>,
    pub duration_secs: u64,
    pub interim_secs: u64,
    /// Run a scan pre-pass and survey only active/core channels.
    pub scan_first: bool,
    pub scan_dwell_secs: u64,
    /// Sweep gain settings empirically before the survey proper.
    pub tune_gain: bool,
    pub tune_dwell_secs: u64,
    /// Per-visit dwell when rotating between capture windows.
    pub rotate_dwell_secs: u64,
    pub show_messages: bool,
    pub jsonl: Option<std::path::PathBuf>,
    pub out_json: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
struct SurveyChannel {
    frequency_hz: u64,
    listen_secs: f64,
    frames: u64,
    crc_ok: u64,
    crc_bad: u64,
    ok_pct: f64,
    frames_per_min: f64,
    level_dbfs: f32,
    verdict: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct SweepRow {
    gain_db: f64,
    frames: u64,
    crc_ok: u64,
    mean_level_dbfs: f32,
}

#[derive(Serialize)]
struct SurveyReport {
    mode: String,
    duration_secs: f64,
    sample_rate: f64,
    gain: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    sweep: Vec<SweepRow>,
    channels: Vec<SurveyChannel>,
    advice: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    proposal: Option<String>,
}

/// Accumulated per-channel state across rotation visits.
#[derive(Default, Clone)]
struct ChanAcc {
    frames: u64,
    crc_ok: u64,
    listen_secs: f64,
    level_dbfs: f32,
}

pub fn run(opts: SurveyOpts) -> anyhow::Result<()> {
    let mode = opts.mode;
    let (plan_rate, plan_channels) = scan::plan(mode);
    let rate = match opts.sample_rate {
        Some(r) => r,
        None => {
            let caps = crate::probe_rate_caps(&opts.sdr);
            let r = scan::choose_rate(&caps, mode, plan_rate);
            if r != plan_rate {
                println!("using {} S/s (device does not offer the plan's {} S/s)", r as u64, plan_rate as u64);
            }
            if let Some(hint) = scan::rate_choice_hint(mode, r, plan_rate) {
                println!("note: {hint}");
            }
            r
        }
    };
    let passband = scan::passband(mode);

    // Ctrl-C ends the survey early but still produces the report. A second
    // Ctrl-C forces an immediate exit: if a device open or stream read is
    // wedged and never observes `abort`, the first signal can't get us out,
    // and tokio swallows further signals once nothing is awaiting them — so
    // we keep awaiting and hard-exit on the second.
    let abort = Arc::new(AtomicBool::new(false));
    {
        let abort = abort.clone();
        std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_io().build() else {
                return;
            };
            rt.block_on(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    eprintln!("\ninterrupted — finishing up and reporting (Ctrl-C again to quit now)");
                    abort.store(true, Ordering::Relaxed);
                }
                if tokio::signal::ctrl_c().await.is_ok() {
                    eprintln!("\nforced quit");
                    std::process::exit(130);
                }
            });
        });
    }

    // ── Channel set ────────────────────────────────────────────────────
    let channels: Vec<u64> = if !opts.channels.is_empty() {
        opts.channels.clone()
    } else if opts.scan_first {
        scan_prepass(&opts, mode, rate, &plan_channels, passband, &abort)?
    } else {
        plan_channels.clone()
    };
    anyhow::ensure!(
        !channels.is_empty(),
        "no channels to survey: mode {mode} has no built-in plan; pass --channels"
    );
    let groups = scan::group_channels(&channels, rate, passband);
    println!(
        "surveying {} channel(s) in {} capture window(s), {}s total",
        channels.len(),
        groups.len(),
        opts.duration_secs
    );

    // Whole-command clock: --duration bounds the TOTAL run (gain sweep +
    // survey loop), so the command never overruns what the user asked for.
    // (Previously the sweep ran *before* this clock started, so a tune-gain
    // survey always took duration + sweep time.)
    let started = Instant::now();
    let deadline = started + Duration::from_secs(opts.duration_secs);

    // ── Gain ───────────────────────────────────────────────────────────
    let mut sweep_rows = Vec::new();
    let (gain, gain_desc) = if opts.tune_gain && !abort.load(Ordering::Relaxed) {
        // A failed, empty, or interrupted sweep must NOT sink the survey or
        // its report — fall back to the requested gain / AGC and carry on.
        match sweep_gain(&opts, mode, rate, &groups, &abort, deadline) {
            Ok((g, rows)) => {
                sweep_rows = rows;
                (Some(g), format!("{g} dB (swept)"))
            }
            Err(e) => {
                println!("gain sweep did not complete ({e}); continuing");
                match opts.gain {
                    Some(g) => (Some(g), format!("{g} dB (fixed)")),
                    None => (None, "hardware AGC".to_string()),
                }
            }
        }
    } else {
        match opts.gain {
            Some(g) => (Some(g), format!("{g} dB (fixed)")),
            None => (None, "hardware AGC".to_string()),
        }
    };

    // ── Survey loop: rotate capture windows until the time is spent ────
    let bus = MessageBus::new();
    let mut rx = bus.subscribe();
    let mut jsonl = match &opts.jsonl {
        Some(p) => Some(std::io::BufWriter::new(
            std::fs::OpenOptions::new().create(true).append(true).open(p)?,
        )),
        None => None,
    };

    let mut acc: std::collections::BTreeMap<u64, ChanAcc> = Default::default();
    let mut next_interim = started + Duration::from_secs(opts.interim_secs);
    let mut gi = 0usize;
    let mut ever_ok = false;
    let mut consec_fail = 0usize;
    while Instant::now() < deadline && !abort.load(Ordering::Relaxed) {
        let (center, group) = &groups[gi % groups.len()];
        gi += 1;
        let remaining = deadline.saturating_duration_since(Instant::now()).as_secs();
        // A single window has nothing to rotate to: dwell straight through
        // to the next interim boundary instead of churning the device.
        let visit = if groups.len() == 1 {
            remaining.min(opts.interim_secs)
        } else {
            remaining.min(opts.rotate_dwell_secs)
        };
        if visit == 0 {
            break;
        }
        // Settle between reopen cycles (multi-window rotation reuses the
        // same device back-to-back).
        if gi > 1 {
            std::thread::sleep(Duration::from_millis(500));
        }
        let t0 = Instant::now();
        match dwell(&opts.sdr, gain, mode, rate, *center, group, visit, &abort, &bus) {
            Ok(stats) => {
                ever_ok = true;
                consec_fail = 0;
                let secs = t0.elapsed().as_secs_f64();
                for (freq, frames, ok, level) in stats {
                    let a = acc.entry(freq).or_default();
                    a.frames += frames;
                    a.crc_ok += ok;
                    a.listen_secs += secs;
                    a.level_dbfs = level;
                }
            }
            Err(e) => {
                consec_fail += 1;
                // Before any window has ever worked, a full rotation of
                // failures means the configuration itself is bad (wrong
                // sample rate, missing device): retrying forever just
                // spams the same error.
                if !ever_ok && consec_fail >= groups.len().max(2) {
                    anyhow::bail!("every capture window is failing (last error: {e})");
                }
                println!("window @ {:.3} MHz skipped: {e}", *center as f64 / 1e6);
                std::thread::sleep(Duration::from_secs(2));
            }
        }
        drain_messages(&mut rx, opts.show_messages, jsonl.as_mut());
        if Instant::now() >= next_interim && Instant::now() < deadline {
            let elapsed = started.elapsed().as_secs();
            println!("\n── interim @ {elapsed}s ──");
            print_table(&summarize(&acc));
            next_interim += Duration::from_secs(opts.interim_secs);
        }
    }
    if let Some(w) = jsonl.as_mut() {
        let _ = w.flush();
    }

    // ── Report ─────────────────────────────────────────────────────────
    let results = summarize(&acc);
    let actual = started.elapsed().as_secs_f64();
    println!("\n══ survey report: {mode}, {:.0}s, gain {gain_desc} ══", actual);
    if !sweep_rows.is_empty() {
        println!("gain sweep:");
        for r in &sweep_rows {
            println!(
                "  {:>5.1} dB: {:>4} frames, {:>4} ok, mean level {:>6.1} dBFS",
                r.gain_db, r.frames, r.crc_ok, r.mean_level_dbfs
            );
        }
    }
    print_table(&results);

    let advice = advise(opts.gain.is_none() && !opts.tune_gain, &results);
    if !advice.is_empty() {
        println!("\nsuggestions:");
        for a in &advice {
            println!("  • {a}");
        }
    }

    let proposal = proposal_command(&opts.sdr, mode, rate, gain, &results);
    if let Some(p) = &proposal {
        println!("\nproposed configuration:\n  {p}");
    }

    if let Some(path) = &opts.out_json {
        let report = SurveyReport {
            mode: mode.as_str().to_string(),
            duration_secs: actual,
            sample_rate: rate,
            gain: gain_desc,
            sweep: sweep_rows,
            channels: results,
            advice,
            proposal,
        };
        std::fs::write(path, serde_json::to_string_pretty(&report)?)?;
        println!("\nreport written to {}", path.display());
    }
    Ok(())
}

/// Scan pre-pass: short dwell over the full plan, then survey the channels
/// that showed life **plus** the mode's core tier — the worldwide/primary
/// frequencies a short dwell routinely undersells.
fn scan_prepass(
    opts: &SurveyOpts,
    mode: Mode,
    rate: f64,
    plan_channels: &[u64],
    passband: f64,
    abort: &Arc<AtomicBool>,
) -> anyhow::Result<Vec<u64>> {
    let groups = scan::group_channels(plan_channels, rate, passband);
    println!(
        "scan pre-pass: {} group(s), {}s dwell each",
        groups.len(),
        opts.scan_dwell_secs
    );
    let mut results = Vec::new();
    for (center, group) in &groups {
        if abort.load(Ordering::Relaxed) {
            break;
        }
        println!("→ {:.3} MHz", *center as f64 / 1e6);
        match scan::scan_group(&opts.sdr, opts.gain, mode, rate, *center, group, opts.scan_dwell_secs)
        {
            Ok((mut r, _)) => results.append(&mut r),
            Err(e) => println!("  group skipped: {e}"),
        }
    }
    let mut levels: Vec<f32> = results.iter().map(|r| r.level_dbfs).collect();
    levels.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = levels.get(levels.len() / 2).copied().unwrap_or(-120.0);

    let mut keep: std::collections::BTreeSet<u64> = results
        .iter()
        .filter(|r| r.frames > 0 || r.level_dbfs > median + 6.0)
        .map(|r| r.frequency_hz)
        .collect();
    let active = keep.len();
    let core: Vec<u64> = scan::core_channels(mode)
        .into_iter()
        .filter(|f| plan_channels.contains(f) && keep.insert(*f))
        .collect();
    if !core.is_empty() {
        let list: Vec<String> = core.iter().map(|&f| format!("{:.3}", f as f64 / 1e6)).collect();
        println!("including {} quiet-but-core channel(s) anyway: {}", core.len(), list.join(", "));
    }
    println!("pre-pass kept {} channel(s) ({active} with signal + {} core)", keep.len(), core.len());
    Ok(keep.into_iter().collect())
}

/// Step a generic dB ladder on the busiest window, score each setting, and
/// pick the winner: most CRC-verified frames, then most frames, then the
/// healthiest mean level (closest to −35 dBFS — strong but far from
/// clipping).
fn sweep_gain(
    opts: &SurveyOpts,
    mode: Mode,
    rate: f64,
    groups: &[(u64, Vec<u64>)],
    abort: &Arc<AtomicBool>,
    deadline: Instant,
) -> anyhow::Result<(f64, Vec<SweepRow>)> {
    const LADDER: [f64; 5] = [12.0, 21.0, 30.0, 39.0, 48.0];
    let (center, group) =
        groups.iter().max_by_key(|(_, g)| g.len()).expect("at least one group");
    println!(
        "gain sweep on {:.3} MHz window: {:?} dB, up to {}s each",
        *center as f64 / 1e6,
        LADDER,
        opts.tune_dwell_secs
    );
    let bus = MessageBus::new();
    let mut rx = bus.subscribe();
    let mut rows = Vec::new();
    for &g in &LADDER {
        if abort.load(Ordering::Relaxed) {
            break;
        }
        // Keep the whole command within --duration: cap each step to the
        // time left and stop the sweep once the budget is spent.
        let remaining = deadline.saturating_duration_since(Instant::now()).as_secs();
        let dwell_secs = opts.tune_dwell_secs.min(remaining);
        if dwell_secs == 0 {
            break;
        }
        // Let the device settle between rapid reopen cycles.
        std::thread::sleep(Duration::from_millis(500));
        match dwell(&opts.sdr, Some(g), mode, rate, *center, group, dwell_secs, abort, &bus)
        {
            Ok(stats) => {
                let frames: u64 = stats.iter().map(|s| s.1).sum();
                let ok: u64 = stats.iter().map(|s| s.2).sum();
                let mean = stats.iter().map(|s| s.3).sum::<f32>() / stats.len().max(1) as f32;
                println!("  {g:>5.1} dB: {frames} frames, {ok} ok, mean {mean:.1} dBFS");
                rows.push(SweepRow { gain_db: g, frames, crc_ok: ok, mean_level_dbfs: mean });
            }
            Err(e) => println!("  {g:>5.1} dB: failed ({e})"),
        }
        drain_messages(&mut rx, false, None);
    }
    anyhow::ensure!(!rows.is_empty(), "gain sweep collected no data");
    let best = pick_gain(&rows);
    println!("→ using {best} dB");
    Ok((best, rows))
}

/// Decode counts only outrank level health once there's enough traffic to
/// be statistics rather than luck — bursty modes can hand any gain setting
/// a frame or two in a short dwell (observed live: a 15 s dwell crowned
/// 12 dB on two lucky frames, leaving every channel at −66 dBFS). Below
/// the threshold, prefer the mean level closest to −35 dBFS: strong ADC
/// loading with ample clipping headroom.
fn pick_gain(rows: &[SweepRow]) -> f64 {
    const MIN_FRAMES_FOR_DECODE_SCORING: u64 = 5;
    rows.iter()
        .max_by(|a, b| {
            let key = |r: &SweepRow| {
                let trusted = r.frames >= MIN_FRAMES_FOR_DECODE_SCORING;
                (
                    if trusted { r.crc_ok } else { 0 },
                    if trusted { r.frames } else { 0 },
                    -((r.mean_level_dbfs + 35.0).abs() * 10.0) as i64,
                )
            };
            key(a).cmp(&key(b))
        })
        .map(|r| r.gain_db)
        .expect("non-empty rows")
}

/// One capture-window visit: open, decode for `secs` (or until abort), and
/// return per-channel (freq, frames, crc_ok, level_dbfs).
#[allow(clippy::too_many_arguments)]
fn dwell(
    sdr: &str,
    gain: Option<f64>,
    mode: Mode,
    rate: f64,
    center: u64,
    channels: &[u64],
    secs: u64,
    abort: &Arc<AtomicBool>,
    bus: &MessageBus,
) -> anyhow::Result<Vec<(u64, u64, u64, f32)>> {
    let (mut source, _) = crate::open_sdr(sdr, rate, center, gain)?;
    let cfg = SessionConfig {
        mode,
        center_hz: center,
        channels_hz: channels.to_vec(),
        station_ident: "XNG-SURVEY".into(),
        sdr: None,
        receiver_pos: None,
        label_filter: Default::default(),
        ais_filter: Default::default(),
        demod_effort: runtime::DemodEffort::Live,
        max_ppm: None,
        outputs: runtime::OutputConfig {
            console: ConsoleFormat::Pretty,
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
        },
    };
    let decoders = runtime::build_decoders(rate, center, &cfg)?;
    let live = runtime::LiveState::new();
    let stop = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(secs);
    let stop_thread = {
        let stop = stop.clone();
        let abort = abort.clone();
        std::thread::spawn(move || {
            while Instant::now() < deadline && !abort.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
            }
            stop.store(true, Ordering::Relaxed);
        })
    };
    let stats = runtime::decode_loop(
        &mut *source,
        decoders,
        StationIdentity::new("XNG-SURVEY"),
        None,
        bus.clone(),
        stop,
        Some((live.clone(), center, rate)),
        None,
        Default::default(),
        Default::default(),
    )?;
    let _ = stop_thread.join();
    let levels = live.stats.lock().unwrap().clone();
    Ok(stats
        .iter()
        .enumerate()
        .map(|(i, &(freq, frames, ok))| {
            (freq, frames, ok, levels.get(i).map(|l| l.3).unwrap_or(-120.0))
        })
        .collect())
}

fn drain_messages(
    rx: &mut tokio::sync::broadcast::Receiver<Arc<xng_types::Message>>,
    show: bool,
    mut jsonl: Option<&mut std::io::BufWriter<std::fs::File>>,
) {
    use tokio::sync::broadcast::error::TryRecvError;
    loop {
        match rx.try_recv() {
            Ok(msg) => {
                if show {
                    println!("{}", format_message(&msg, ConsoleFormat::Pretty));
                }
                if let Some(w) = jsonl.as_deref_mut() {
                    if let Ok(line) = serde_json::to_string(&*msg) {
                        let _ = writeln!(w, "{line}");
                    }
                }
            }
            Err(TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
}

fn summarize(acc: &std::collections::BTreeMap<u64, ChanAcc>) -> Vec<SurveyChannel> {
    let mut levels: Vec<f32> = acc.values().map(|a| a.level_dbfs).collect();
    levels.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = levels.get(levels.len() / 2).copied().unwrap_or(-120.0);
    acc.iter()
        .map(|(&freq, a)| {
            let bad = a.frames - a.crc_ok;
            let mins = (a.listen_secs / 60.0).max(1e-9);
            SurveyChannel {
                frequency_hz: freq,
                listen_secs: a.listen_secs,
                frames: a.frames,
                crc_ok: a.crc_ok,
                crc_bad: bad,
                ok_pct: if a.frames > 0 {
                    a.crc_ok as f64 / a.frames as f64 * 100.0
                } else {
                    0.0
                },
                frames_per_min: a.frames as f64 / mins,
                level_dbfs: a.level_dbfs,
                verdict: if a.crc_ok > 0 {
                    "ACTIVE"
                } else if a.frames > 0 {
                    "partial"
                } else if a.level_dbfs > median + 6.0 {
                    "signal?"
                } else {
                    "quiet"
                },
            }
        })
        .collect()
}

fn print_table(results: &[SurveyChannel]) {
    println!(
        "{:<10} {:>8} {:>7} {:>6} {:>6} {:>6} {:>7} {:>8}  verdict",
        "freq MHz", "listen s", "frames", "ok", "bad", "ok%", "fr/min", "dBFS"
    );
    for r in results {
        println!(
            "{:<10.3} {:>8.0} {:>7} {:>6} {:>6} {:>6.0} {:>7.2} {:>8.1}  {}",
            r.frequency_hz as f64 / 1e6,
            r.listen_secs,
            r.frames,
            r.crc_ok,
            r.crc_bad,
            r.ok_pct,
            r.frames_per_min,
            r.level_dbfs,
            r.verdict
        );
    }
    let frames: u64 = results.iter().map(|r| r.frames).sum();
    let ok: u64 = results.iter().map(|r| r.crc_ok).sum();
    println!(
        "{:<10} {:>8} {:>7} {:>6} {:>6} {:>6.0}",
        "total",
        "",
        frames,
        ok,
        frames - ok,
        if frames > 0 { ok as f64 / frames as f64 * 100.0 } else { 0.0 }
    );
}

/// Reception advice from the final statistics. Pure so it's testable.
fn advise(used_agc: bool, results: &[SurveyChannel]) -> Vec<String> {
    let mut advice = Vec::new();
    if results.is_empty() {
        return advice;
    }
    let max_level = results.iter().map(|r| r.level_dbfs).fold(f32::MIN, f32::max);
    let frames: u64 = results.iter().map(|r| r.frames).sum();
    let ok: u64 = results.iter().map(|r| r.crc_ok).sum();

    if max_level < -55.0 {
        advice.push(format!(
            "every channel sits below \u{2212}55 dBFS (max {max_level:.1}): weak signals \u{2014} \
             increase gain, or improve the antenna / add an LNA"
        ));
    }
    if max_level > -12.0 {
        advice.push(format!(
            "strongest channel is at {max_level:.1} dBFS, close to full scale: if CRC failures \
             are high, reduce gain to avoid intermodulation"
        ));
    }
    for r in results {
        if r.frames >= 5 && r.crc_ok == 0 && r.level_dbfs > -50.0 {
            advice.push(format!(
                "{:.3} MHz triggers the demodulator ({} frames) but nothing passes CRC at a \
                 healthy level: likely interference or a non-{} signal on that frequency",
                r.frequency_hz as f64 / 1e6,
                r.frames,
                "matching"
            ));
        }
    }
    if frames >= 20 && ok as f64 / frames as f64 * 100.0 < 50.0 {
        advice.push(format!(
            "overall CRC pass rate is {:.0}% \u{2014} marginal SNR; small gain changes \
             (\u{00b1}3\u{2013}6 dB) or antenna placement usually dominate here",
            ok as f64 / frames as f64 * 100.0
        ));
    }
    if used_agc {
        advice.push(
            "gain was hardware AGC: run again with --tune-gain to pick a fixed gain empirically"
                .to_string(),
        );
    }
    advice
}

fn proposal_command(
    sdr: &str,
    mode: Mode,
    rate: f64,
    gain: Option<f64>,
    results: &[SurveyChannel],
) -> Option<String> {
    let active: Vec<u64> = results
        .iter()
        .filter(|r| r.verdict == "ACTIVE")
        .map(|r| r.frequency_hz)
        .collect();
    if active.is_empty() {
        return None;
    }
    let center = (active.iter().min().unwrap() + active.iter().max().unwrap()) / 2;
    // Whole-capture modes (passband 0: Mode S) keep the channel at center.
    let center = if scan::passband(mode) > 0.0 && active.contains(&center) {
        center + 25_000
    } else {
        center
    };
    let chans: Vec<String> = active.iter().map(|&f| format!("{:.3}", f as f64 / 1e6)).collect();
    let gain_arg = gain.map(|g| format!(" --gain {g}")).unwrap_or_default();
    Some(format!(
        "xng listen --sdr '{sdr}'{gain_arg} --mode {mode} -r {} -c {:.3}M --channels {}",
        rate as u64,
        center as f64 / 1e6,
        chans.join(",")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chan(freq: u64, frames: u64, ok: u64, level: f32) -> SurveyChannel {
        SurveyChannel {
            frequency_hz: freq,
            listen_secs: 600.0,
            frames,
            crc_ok: ok,
            crc_bad: frames - ok,
            ok_pct: if frames > 0 { ok as f64 / frames as f64 * 100.0 } else { 0.0 },
            frames_per_min: frames as f64 / 10.0,
            level_dbfs: level,
            verdict: if ok > 0 { "ACTIVE" } else { "quiet" },
        }
    }

    #[test]
    fn weak_site_advice() {
        let advice = advise(false, &[chan(131_550_000, 2, 1, -62.0), chan(130_025_000, 0, 0, -70.0)]);
        assert!(advice.iter().any(|a| a.contains("weak signals")), "{advice:?}");
    }

    #[test]
    fn clipping_advice() {
        let advice = advise(false, &[chan(131_550_000, 50, 10, -6.0)]);
        assert!(advice.iter().any(|a| a.contains("full scale")), "{advice:?}");
    }

    #[test]
    fn interference_and_agc_advice() {
        let advice = advise(true, &[chan(131_550_000, 9, 0, -40.0)]);
        assert!(advice.iter().any(|a| a.contains("nothing passes CRC")), "{advice:?}");
        assert!(advice.iter().any(|a| a.contains("--tune-gain")), "{advice:?}");
    }

    #[test]
    fn healthy_site_gets_no_scolding() {
        let advice = advise(false, &[chan(131_550_000, 40, 36, -38.0)]);
        assert!(advice.is_empty(), "{advice:?}");
    }

    #[test]
    fn sweep_prefers_decodes_then_level() {
        let rows = vec![
            SweepRow { gain_db: 12.0, frames: 3, crc_ok: 1, mean_level_dbfs: -60.0 },
            SweepRow { gain_db: 30.0, frames: 8, crc_ok: 6, mean_level_dbfs: -40.0 },
            SweepRow { gain_db: 48.0, frames: 9, crc_ok: 6, mean_level_dbfs: -8.0 },
        ];
        // 30 and 48 dB tie on decodes; 48 has more frames so it wins the
        // second criterion before level is consulted.
        assert_eq!(pick_gain(&rows), 48.0);
        let rows2 = vec![
            SweepRow { gain_db: 30.0, frames: 8, crc_ok: 6, mean_level_dbfs: -40.0 },
            SweepRow { gain_db: 48.0, frames: 8, crc_ok: 6, mean_level_dbfs: -8.0 },
        ];
        // Full tie on decodes and frames: the healthier level decides.
        assert_eq!(pick_gain(&rows2), 30.0);
    }

    #[test]
    fn sweep_ignores_lucky_frames_on_sparse_traffic() {
        // The live-observed failure: 2 lucky decodes at low gain must not
        // outrank healthy ADC loading when traffic is too sparse to score.
        let rows = vec![
            SweepRow { gain_db: 12.0, frames: 3, crc_ok: 2, mean_level_dbfs: -66.0 },
            SweepRow { gain_db: 21.0, frames: 1, crc_ok: 1, mean_level_dbfs: -59.0 },
            SweepRow { gain_db: 48.0, frames: 1, crc_ok: 1, mean_level_dbfs: -35.0 },
        ];
        assert_eq!(pick_gain(&rows), 48.0);
    }

    #[test]
    fn proposal_only_from_active_channels() {
        let results =
            vec![chan(131_550_000, 40, 36, -38.0), chan(130_025_000, 0, 0, -60.0)];
        let p = proposal_command("driver=rtlsdr", Mode::AcarsPoa, 2_400_000.0, Some(28.0), &results)
            .unwrap();
        assert!(p.contains("--channels 131.550"), "{p}");
        assert!(!p.contains("130.025"), "{p}");
        assert!(p.contains("--gain 28"), "{p}");
    }
}
