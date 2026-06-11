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
use xng_mode_aero::{AeroBurstDecoder, AeroChannelDecoder};
use xng_mode_ais::AisChannelDecoder;
use xng_mode_hfdl::HfdlChannelDecoder;
use xng_mode_iridium::{IridiumChannelDecoder, IridiumWidebandDecoder};
use xng_mode_stdc::StdcChannelDecoder;
use xng_mode_vdl2::Vdl2ChannelDecoder;
use xng_sdr::{IqSource, SdrError};
use xng_types::{AppInfo, ChannelInfo, Message, MessageBody, Mode, Provenance, SdrInfo, StationIdentity};

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
    /// Prometheus metrics listen address (host:port).
    pub metrics: Option<String>,
    /// SBS-1 (BaseStation, dump1090 port-30003 style) TCP server address.
    pub sbs: Option<String>,
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

/// Live session state shared with the TUI.
pub struct LiveState {
    /// Per channel: (freq_hz, frames, crc_ok, level_dbfs).
    pub stats: std::sync::Mutex<Vec<(u64, u64, u64, f32)>>,
    pub spectrum: std::sync::Mutex<Option<SpectrumFrame>>,
    pub samples: std::sync::atomic::AtomicU64,
}

pub struct SpectrumFrame {
    pub bins_db: Vec<f32>,
    #[allow(dead_code)]
    pub center_hz: u64,
    #[allow(dead_code)]
    pub span_hz: f64,
}

impl LiveState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            stats: std::sync::Mutex::new(Vec::new()),
            spectrum: std::sync::Mutex::new(None),
            samples: std::sync::atomic::AtomicU64::new(0),
        })
    }
}

/// One mode-specific per-channel decoder.
pub(crate) enum ModeChannel {
    Acars(AcarsChannelDecoder),
    Ais(AisChannelDecoder),
    Adsb(AdsbDecoder),
    Vdl2(Vdl2ChannelDecoder),
    Aero(AeroChannelDecoder),
    AeroBurst(AeroBurstDecoder),
    StdC(StdcChannelDecoder),
    Hfdl(HfdlChannelDecoder),
    Iridium(IridiumChannelDecoder),
    IridiumWide(IridiumWidebandDecoder),
}

impl ModeChannel {
    fn new(mode: Mode, sample_rate: f64, offset: f64, freq: u64) -> Result<Self, String> {
        match mode {
            Mode::AcarsPoa => Ok(Self::Acars(AcarsChannelDecoder::new(sample_rate, offset)?)),
            Mode::Ais => Ok(Self::Ais(AisChannelDecoder::new(sample_rate, offset, freq)?)),
            Mode::Vdl2 => Ok(Self::Vdl2(Vdl2ChannelDecoder::new(sample_rate, offset)?)),
            Mode::AeroL => Ok(Self::Aero(AeroChannelDecoder::new(sample_rate, offset)?)),
            Mode::AeroC => Ok(Self::AeroBurst(AeroBurstDecoder::new(sample_rate, offset)?)),
            Mode::StdC => Ok(Self::StdC(StdcChannelDecoder::new(sample_rate, offset)?)),
            Mode::Hfdl => Ok(Self::Hfdl(HfdlChannelDecoder::new(sample_rate, offset)?)),
            Mode::Iridium => {
                // At zero offset (channel == capture center) the wideband
                // burst hunter consumes the whole capture — needed for
                // SBD/ACARS traffic, which hops across duplex channels.
                if offset.abs() < 1e-6 {
                    Ok(Self::IridiumWide(IridiumWidebandDecoder::new(sample_rate)?))
                } else {
                    Ok(Self::Iridium(IridiumChannelDecoder::new(sample_rate, offset)?))
                }
            }
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
            Mode::AeroL | Mode::AeroC => xng_mode_aero::CHANNEL_PASSBAND_HZ,
            Mode::StdC => xng_mode_stdc::CHANNEL_PASSBAND_HZ,
            Mode::Hfdl => xng_mode_hfdl::CHANNEL_PASSBAND_HZ,
            Mode::Iridium => xng_mode_iridium::CHANNEL_PASSBAND_HZ,
            Mode::Adsb => 0.0, // wideband: offset must be 0, no DDC
            _ => xng_mode_acars::CHANNEL_PASSBAND_HZ,
        }
    }

    fn channel_rate(&self) -> f64 {
        match self {
            Self::Acars(_) => xng_mode_acars::CHANNEL_RATE,
            Self::Ais(_) => xng_mode_ais::CHANNEL_RATE,
            Self::Vdl2(_) => xng_mode_vdl2::CHANNEL_RATE,
            Self::Aero(_) | Self::AeroBurst(_) => xng_mode_aero::CHANNEL_RATE,
            Self::StdC(_) => xng_mode_stdc::CHANNEL_RATE,
            Self::Hfdl(_) => xng_mode_hfdl::CHANNEL_RATE,
            Self::Iridium(_) => xng_mode_iridium::CHANNEL_RATE,
            Self::IridiumWide(_) => xng_mode_iridium::CHANNEL_RATE,
            Self::Adsb(_) => 2_000_000.0,
        }
    }

    fn level(&self) -> f32 {
        match self {
            Self::Acars(d) => d.level_dbfs(),
            Self::Ais(d) => d.level_dbfs(),
            Self::Adsb(d) => d.level_dbfs(),
            Self::Vdl2(d) => d.level_dbfs(),
            Self::Aero(d) => d.level_dbfs(),
            Self::AeroBurst(d) => d.level_dbfs(),
            Self::StdC(d) => d.level_dbfs(),
            Self::Hfdl(d) => d.level_dbfs(),
            Self::Iridium(d) => d.level_dbfs(),
            Self::IridiumWide(d) => d.level_dbfs(),
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
            Self::AeroBurst(dec) => {
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
            Self::StdC(dec) => {
                let packets = dec.process(iq);
                let seen = packets.len() as u64;
                let level = dec.level_dbfs();
                let ok = packets.iter().filter(|p| p.checksum_ok).count() as u64;
                let msgs = packets
                    .iter()
                    .map(|p| xng_mode_stdc::to_message(p, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, ok)
            }
            Self::Iridium(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let level = dec.level_dbfs();
                let msgs = frames
                    .iter()
                    .map(|f| xng_mode_iridium::to_message(f, freq, level, prov.clone()))
                    .collect();
                (msgs, seen, seen)
            }
            Self::IridiumWide(dec) => {
                let frames = dec.process(iq);
                let seen = frames.len() as u64;
                let level = dec.level_dbfs();
                let msgs = frames
                    .iter()
                    .map(|(off, f)| {
                        let f_hz = (freq as i64 + off.round() as i64).max(0) as u64;
                        xng_mode_iridium::to_message(f, f_hz, level, prov.clone())
                    })
                    .collect();
                (msgs, seen, seen)
            }
            Self::Hfdl(dec) => {
                let events = dec.process(iq);
                let seen = events.len() as u64;
                let level = dec.level_dbfs();
                let ok = events
                    .iter()
                    .filter(|e| e.acars.as_ref().map(|a| a.crc_ok).unwrap_or(true))
                    .count() as u64;
                let msgs = events
                    .iter()
                    .map(|e| xng_mode_hfdl::to_message(e, freq, level, prov.clone()))
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
    let live = LiveState::new();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let bus = MessageBus::new();
        if let Some(addr) = cfg.outputs.metrics.clone() {
            let live = live.clone();
            let mode = cfg.mode.as_str().to_string();
            tokio::spawn(async move {
                if let Err(e) = crate::outputs::metrics::serve(addr, live, mode).await {
                    tracing::warn!("metrics endpoint failed: {e}");
                }
            });
        }
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
        if let Some(addr) = cfg.outputs.sbs.clone() {
            let rx = bus.subscribe();
            output_tasks.push(tokio::spawn(crate::outputs::sbs::run(rx, addr)));
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
            {
                let live = live.clone();
                // Satellite/HF ACARS blocks arrive minutes apart; VHF
                // bearers are quick (libacars timeout profiles).
                let reasm_timeout = match cfg.mode {
                    Mode::AcarsPoa | Mode::Vdl2 => 120.0,
                    _ => 660.0,
                };
                move || {
                    let mut reasm = xng_acars::reasm::Reassembler::new(reasm_timeout);
                    decode_loop(
                        &mut *source,
                        decoders,
                        station,
                        cfg.sdr,
                        bus,
                        stop,
                        Some((live, capture_center, sample_rate)),
                        Some(&mut reasm),
                    )
                }
            }
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

pub(crate) fn decode_loop(
    source: &mut dyn IqSource,
    mut decoders: Vec<(u64, ModeChannel)>,
    station: StationIdentity,
    sdr: Option<SdrInfo>,
    bus: MessageBus,
    stop: Arc<AtomicBool>,
    live: Option<(Arc<LiveState>, u64, f64)>,
    mut reasm: Option<&mut xng_acars::reasm::Reassembler>,
) -> anyhow::Result<Vec<(u64, u64, u64)>> {
    use std::sync::atomic::Ordering as AtomOrd;
    let mut spectrum_fft: Option<std::sync::Arc<dyn rustfft::Fft<f32>>> = None;
    let mut chunk_count: u64 = 0;
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
        if let Some((state, center_hz, sample_rate)) = &live {
            state.samples.fetch_add(n as u64, AtomOrd::Relaxed);
            chunk_count += 1;
            if chunk_count % 4 == 1 && n >= 512 {
                let fft = spectrum_fft
                    .get_or_insert_with(|| {
                        rustfft::FftPlanner::new().plan_fft_forward(512)
                    })
                    .clone();
                let mut fbuf: Vec<Complex<f32>> = buf[..512]
                    .iter()
                    .enumerate()
                    .map(|(k, &x)| {
                        let w = 0.54
                            - 0.46
                                * (std::f32::consts::TAU * k as f32 / 511.0).cos();
                        x * w
                    })
                    .collect();
                fft.process(&mut fbuf);
                // FFT-shift and convert to dB.
                let bins_db: Vec<f32> = (0..512)
                    .map(|k| {
                        let idx = (k + 256) % 512;
                        10.0 * (fbuf[idx].norm_sqr() / (512.0 * 512.0)).max(1e-12).log10()
                    })
                    .collect();
                *state.spectrum.lock().unwrap() = Some(SpectrumFrame {
                    bins_db,
                    center_hz: *center_hz,
                    span_hz: *sample_rate,
                });
            }
        }
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
            for mut msg in msgs {
                if let Some(r) = reasm.as_deref_mut() {
                    apply_reassembly(&mut msg, r);
                }
                bus.publish(msg);
            }
            if let Some((state, _, _)) = &live {
                let mut ls = state.stats.lock().unwrap();
                if ls.len() <= i {
                    ls.resize(decoders_len_hint(i), (0, 0, 0, 0.0));
                }
                ls[i] = (stats[i].0, stats[i].1, stats[i].2, dec_level_after(dec));
            }
        }
    }
    Ok(stats)
}

/// Offer ACARS bodies to the multi-block reassembler; when a message
/// completes, replace the text with the full assembly and re-run the
/// application layer over it (long CPDLC/OHMA/MIAM payloads only decode
/// from complete text).
fn apply_reassembly(msg: &mut Message, r: &mut xng_acars::reasm::Reassembler) {
    use xng_acars::reasm::Reasm;
    let MessageBody::Acars(core) = &mut msg.body else { return };
    if !msg.decode.crc_ok {
        return;
    }
    let now = msg.timestamp.timestamp_millis() as f64 / 1e3;
    if let Reasm::Complete(full) = r.push(core, now) {
        let downlink = core.block_id.is_some_and(|b| b.is_ascii_digit());
        let dec = xng_acars::decode(&core.label, &full, downlink);
        core.text = full;
        core.reassembled = true;
        if let Some(app) = dec.app {
            core.app = serde_json::to_value(&app).ok();
        }
    }
}

fn decoders_len_hint(i: usize) -> usize {
    i + 1
}

fn dec_level_after(dec: &ModeChannel) -> f32 {
    dec.level()
}

/// Build the per-channel decoders for a session config (shared between
/// the console runtime and the TUI).
pub(crate) fn build_decoders(
    sample_rate: f64,
    capture_center: u64,
    cfg: &SessionConfig,
) -> anyhow::Result<Vec<(u64, ModeChannel)>> {
    let mut decoders = Vec::new();
    for &freq in &cfg.channels_hz {
        let offset = freq as f64 - capture_center as f64;
        if 2.0 * (offset.abs() + ModeChannel::passband_hz(cfg.mode)) > sample_rate {
            anyhow::bail!(
                "channel {:.3} MHz is outside the capture",
                freq as f64 / 1e6
            );
        }
        let dec = ModeChannel::new(cfg.mode, sample_rate, offset, freq)
            .map_err(|e| anyhow::anyhow!("channel {:.3} MHz: {e}", freq as f64 / 1e6))?;
        decoders.push((freq, dec));
    }
    Ok(decoders)
}
