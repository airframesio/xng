//! Decode session runtime: drives an IQ source through per-channel decoders
//! on a blocking thread, publishes normalized messages to the bus, and fans
//! out to the configured outputs on the tokio runtime.

use crate::bus::MessageBus;
use crate::outputs::console::{self, ConsoleFormat};
use crate::outputs::{acarsdec_json, jsonl};
use num_complex::Complex;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use xng_mode_acars::AcarsChannelDecoder;
use xng_sdr::{IqSource, SdrError};
use xng_types::{AppInfo, ChannelInfo, Message, Provenance, SdrInfo, StationIdentity};

pub struct OutputConfig {
    /// Console format (always on).
    pub console: ConsoleFormat,
    pub jsonl: Option<PathBuf>,
    /// acarsdec-JSON UDP targets (host:port).
    pub udp: Vec<String>,
}

pub struct SessionConfig {
    pub center_hz: u64,
    pub channels_hz: Vec<u64>,
    pub station_ident: String,
    pub sdr: Option<SdrInfo>,
    pub outputs: OutputConfig,
}

const READ_CHUNK: usize = 65_536;

/// Run an ACARS decode session until the source ends or `stop` is set.
pub fn run_acars_session(
    mut source: Box<dyn IqSource>,
    cfg: SessionConfig,
) -> anyhow::Result<()> {
    let sample_rate = source.sample_rate();
    let capture_center = if cfg.center_hz > 0 { cfg.center_hz } else { source.center_freq_hz() };

    // Build one decoder per channel up front so config errors surface early.
    let mut decoders = Vec::new();
    for &freq in &cfg.channels_hz {
        let offset = freq as f64 - capture_center as f64;
        let dec = AcarsChannelDecoder::new(sample_rate, offset)
            .map_err(|e| anyhow::anyhow!("channel {:.3} MHz: {e}", freq as f64 / 1e6))?;
        if 2.0 * (offset.abs() + xng_mode_acars::CHANNEL_PASSBAND_HZ) > sample_rate {
            anyhow::bail!(
                "channel {:.3} MHz is outside the capture (center {:.3} MHz, rate {} S/s)",
                freq as f64 / 1e6,
                capture_center as f64 / 1e6,
                sample_rate
            );
        }
        decoders.push((freq, dec));
    }
    tracing::info!(
        "acars session: {} channel(s) from a {:.0} S/s capture centered at {:.3} MHz",
        decoders.len(),
        sample_rate,
        capture_center as f64 / 1e6
    );

    let station = StationIdentity::new(cfg.station_ident.clone());
    let stop = Arc::new(AtomicBool::new(false));

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let bus = MessageBus::new();
        let mut output_tasks = Vec::new();
        output_tasks.push(tokio::spawn({
            let rx = bus.subscribe();
            let fmt = cfg.outputs.console;
            async move {
                console::run(rx, fmt).await;
                Ok::<(), std::io::Error>(())
            }
        }));
        if let Some(path) = cfg.outputs.jsonl.clone() {
            let rx = bus.subscribe();
            output_tasks.push(tokio::spawn(async move { jsonl::run(rx, &path).await }));
        }
        for target in cfg.outputs.udp.clone() {
            let rx = bus.subscribe();
            output_tasks.push(tokio::spawn(acarsdec_json::run(rx, target)));
        }

        // Ctrl-C → graceful stop.
        tokio::spawn({
            let stop = stop.clone();
            async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    tracing::info!("interrupt received, stopping session");
                    stop.store(true, Ordering::Relaxed);
                }
            }
        });

        // DSP loop on a blocking thread.
        let decode = tokio::task::spawn_blocking({
            let bus = bus.clone();
            let stop = stop.clone();
            move || decode_loop(&mut *source, decoders, station, cfg.sdr, bus, stop)
        });
        let stats = decode.await??;

        drop(bus); // close the channel so outputs drain and exit
        for t in output_tasks {
            if let Err(e) = t.await? {
                tracing::warn!("output error: {e}");
            }
        }

        for (freq, count, crc_ok) in &stats {
            tracing::info!(
                "channel {:.3} MHz: {count} frame(s), {crc_ok} with valid CRC",
                *freq as f64 / 1e6
            );
        }
        let total: u64 = stats.iter().map(|s| s.1).sum();
        tracing::info!("session complete: {total} frame(s) decoded");
        Ok(())
    })
}

fn decode_loop(
    source: &mut dyn IqSource,
    mut decoders: Vec<(u64, AcarsChannelDecoder)>,
    station: StationIdentity,
    sdr: Option<SdrInfo>,
    bus: MessageBus,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<Vec<(u64, u64, u64)>> {
    let mut buf = vec![Complex::new(0.0f32, 0.0f32); READ_CHUNK];
    let mut stats: Vec<(u64, u64, u64)> = decoders.iter().map(|(f, _)| (*f, 0, 0)).collect();

    while !stop.load(Ordering::Relaxed) {
        let n = match source.read(&mut buf) {
            Ok(n) => n,
            Err(SdrError::EndOfStream) => break,
            Err(e) => return Err(e.into()),
        };
        for (i, (freq, dec)) in decoders.iter_mut().enumerate() {
            for frame in dec.process(&buf[..n]) {
                stats[i].1 += 1;
                if frame.crc_ok {
                    stats[i].2 += 1;
                }
                let msg: Message = xng_mode_acars::to_message(
                    &frame,
                    *freq,
                    dec.level_dbfs(),
                    Provenance {
                        station: station.clone(),
                        app: AppInfo::xng(),
                        sdr: sdr.clone(),
                        channel: Some(ChannelInfo {
                            index: i as u32,
                            frequency_hz: *freq,
                            sample_rate: xng_mode_acars::CHANNEL_RATE,
                        }),
                    },
                );
                bus.publish(msg);
            }
        }
    }
    Ok(stats)
}
