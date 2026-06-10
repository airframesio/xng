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
use xng_mode_adsb::AdsbDecoder;
use xng_mode_aero::AeroChannelDecoder;
use xng_mode_ais::AisChannelDecoder;
use xng_mode_vdl2::Vdl2ChannelDecoder;
use xng_sdr::{IqSource, SdrError};
use xng_types::{AppInfo, ChannelInfo, Message, Mode, Provenance, SdrInfo, StationIdentity};

pub struct OutputConfig {
    /// Console format (always on).
    pub console: ConsoleFormat,
    pub jsonl: Option<PathBuf>,
    /// acarsdec-JSON UDP targets (host:port).
    pub udp: Vec<String>,
    /// asf-2.0 gRPC ingest URL.
    pub asf2_grpc: Option<String>,
    /// asf-2.0 QUIC ingest host:port.
    pub asf2_quic: Option<String>,
    /// Certificate trust for the QUIC output.
    pub asf2_quic_trust: crate::outputs::asf2_quic::TrustMode,
}

pub struct SessionConfig {
    pub mode: Mode,
    pub center_hz: u64,
    pub channels_hz: Vec<u64>,
    pub station_ident: String,
    pub sdr: Option<SdrInfo>,
    pub outputs: OutputConfig,
}

const READ_CHUNK: usize = 65_536;

/// One mode-specific per-channel decoder.
enum ModeChannel {
    Acars(AcarsChannelDecoder),
    Ais(AisChannelDecoder),
    Adsb(AdsbDecoder),
    Vdl2(Vdl2ChannelDecoder),
    Aero(AeroChannelDecoder),
}

impl ModeChannel {
    fn new(mode: Mode, sample_rate: f64, offset: f64, freq: u64) -> Result<Self, String> {
        match mode {
            Mode::AcarsPoa => Ok(Self::Acars(AcarsChannelDecoder::new(sample_rate, offset)?)),
            Mode::Ais => Ok(Self::Ais(AisChannelDecoder::new(sample_rate, offset, freq)?)),
            Mode::Vdl2 => Ok(Self::Vdl2(Vdl2ChannelDecoder::new(sample_rate, offset)?)),
            Mode::AeroL => Ok(Self::Aero(AeroChannelDecoder::new(sample_rate, offset)?)),
            Mode::Adsb => {
                if offset.abs() > 1e-6 {
                    return Err("Mode S uses the whole capture: tune -c to 1090.000M and pass --channels 1090".into());
                }
                Ok(Self::Adsb(AdsbDecoder::new(sample_rate)?))
            }
            other => Err(format!("mode {other} has no native core yet")),
        }
    }

    fn passband_hz(mode: Mode) -> f64 {
        match mode {
            Mode::Ais => xng_mode_ais::CHANNEL_PASSBAND_HZ,
            Mode::Vdl2 => xng_mode_vdl2::CHANNEL_PASSBAND_HZ,
            Mode::AeroL => xng_mode_aero::CHANNEL_PASSBAND_HZ,
            Mode::Adsb => 0.0, // wideband: offset must be 0, no DDC
            _ => xng_mode_acars::CHANNEL_PASSBAND_HZ,
        }
    }

    fn channel_rate(&self) -> f64 {
        match self {
            Self::Acars(_) => xng_mode_acars::CHANNEL_RATE,
            Self::Ais(_) => xng_mode_ais::CHANNEL_RATE,
            Self::Vdl2(_) => xng_mode_vdl2::CHANNEL_RATE,
            Self::Aero(_) => xng_mode_aero::CHANNEL_RATE,
            Self::Adsb(_) => 2_000_000.0,
        }
    }

    /// Decode a capture chunk into normalized messages.
    /// Returns (messages, frames_seen, frames_crc_ok).
    fn process(&mut self, iq: &[Complex<f32>], freq: u64, prov: &Provenance) -> (Vec<Message>, u64, u64) {
        match self {
            Self::Acars(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let ok = frames.iter().filter(|f| f.crc_ok).count() as u64;
                let level = dec.level_dbfs();
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_acars::to_message(f, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, ok)
            }
            Self::Ais(dec) => {
                let found = dec.process(iq);
                let seen = found.len() as u64;
                let level = dec.level_dbfs();
                let msgs = found
                    .into_iter()
                    .map(|(f, nmea)| xng_mode_ais::to_message(&f, nmea, freq, level, prov.clone()))
                    .collect();
                // The AIS deframer only surfaces CRC-valid frames.
                (msgs, seen, seen)
            }
            Self::Vdl2(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let level = dec.level_dbfs();
                let ok = frames
                    .iter()
                    .filter(|f| f.acars.as_ref().map(|a| a.crc_ok).unwrap_or(true))
                    .count() as u64;
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_vdl2::to_message(f, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, ok)
            }
            Self::Aero(dec) => {
                let events = dec.process(iq);
                let seen = events.len() as u64;
                let level = dec.level_dbfs();
                let ok = events
                    .iter()
                    .filter(|e| e.acars.as_ref().map(|a| a.crc_ok).unwrap_or(true))
                    .count() as u64;
                let msgs = events
                    .iter()
                    .map(|e| xng_mode_aero::to_message(e, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, ok)
            }
            Self::Adsb(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_adsb::to_message(f, freq, prov.clone()))
                    .collect();
                // The validator only surfaces parity-valid frames.
                (msgs, seen, seen)
            }
        }
    }
}

/// Run a decode session until the source ends or `stop` is set.
pub fn run_session(mut source: Box<dyn IqSource>, cfg: SessionConfig) -> anyhow::Result<()> {
    let sample_rate = source.sample_rate();
    let capture_center = if cfg.center_hz > 0 { cfg.center_hz } else { source.center_freq_hz() };

    // Build one decoder per channel up front so config errors surface early.
    let mut decoders = Vec::new();
    for &freq in &cfg.channels_hz {
        let offset = freq as f64 - capture_center as f64;
        if 2.0 * (offset.abs() + ModeChannel::passband_hz(cfg.mode)) > sample_rate {
            anyhow::bail!(
                "channel {:.3} MHz is outside the capture (center {:.3} MHz, rate {} S/s)",
                freq as f64 / 1e6,
                capture_center as f64 / 1e6,
                sample_rate
            );
        }
        let dec = ModeChannel::new(cfg.mode, sample_rate, offset, freq)
            .map_err(|e| anyhow::anyhow!("channel {:.3} MHz: {e}", freq as f64 / 1e6))?;
        decoders.push((freq, dec));
    }
    tracing::info!(
        "{} session: {} channel(s) from a {:.0} S/s capture centered at {:.3} MHz",
        cfg.mode,
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
        if let Some(url) = cfg.outputs.asf2_grpc.clone() {
            let rx = bus.subscribe();
            let (id, ident) = (station.id.to_string(), station.ident.clone());
            output_tasks.push(tokio::spawn(crate::outputs::asf2_grpc::run(rx, url, id, ident)));
        }
        if let Some(target) = cfg.outputs.asf2_quic.clone() {
            let rx = bus.subscribe();
            let trust = cfg.outputs.asf2_quic_trust.clone();
            let (id, ident) = (station.id.to_string(), station.ident.clone());
            output_tasks.push(tokio::spawn(crate::outputs::asf2_quic::run(rx, target, trust, id, ident)));
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
    mut decoders: Vec<(u64, ModeChannel)>,
    station: StationIdentity,
    sdr: Option<SdrInfo>,
    bus: MessageBus,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<Vec<(u64, u64, u64)>> {
    let mut buf = vec![Complex::new(0.0f32, 0.0f32); READ_CHUNK];
    let mut stats: Vec<(u64, u64, u64)> = decoders.iter().map(|(f, _)| (*f, 0, 0)).collect();
    let mut consecutive_errors: u32 = 0;

    while !stop.load(Ordering::Relaxed) {
        let n = match source.read(&mut buf) {
            Ok(n) => {
                consecutive_errors = 0;
                n
            }
            Err(SdrError::EndOfStream) => break,
            Err(e) => {
                // Transient device hiccups (overflows, timeouts) are routine
                // on live streams; only give up if they persist.
                consecutive_errors += 1;
                if consecutive_errors >= 10 {
                    return Err(anyhow::anyhow!("giving up after {consecutive_errors} consecutive read errors: {e}"));
                }
                tracing::warn!("read error ({consecutive_errors}/10): {e}");
                continue;
            }
        };
        for (i, (freq, dec)) in decoders.iter_mut().enumerate() {
            let prov = Provenance {
                station: station.clone(),
                app: AppInfo::xng(),
                sdr: sdr.clone(),
                channel: Some(ChannelInfo {
                    index: i as u32,
                    frequency_hz: *freq,
                    sample_rate: dec.channel_rate(),
                }),
            };
            let (msgs, seen, ok) = dec.process(&buf[..n], *freq, &prov);
            stats[i].1 += seen;
            stats[i].2 += ok;
            for msg in msgs {
                bus.publish(msg);
            }
        }
    }
    Ok(stats)
}
