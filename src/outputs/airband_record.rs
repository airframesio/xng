//! Airband recordings.
//!
//! Split mode (default): one file per transmission,
//! `<dir>/<channel>/<YYYY-MM-DD>/<HHMMSS>_<MHz>.wav` (UTC), with a JSON
//! sidecar holding the transmission's metadata. Transmissions shorter than
//! `record_min_ms` are discarded; one longer than `record_max_s` continues in
//! a new file.
//!
//! Continuous mode: one file per channel per UTC hour,
//! `<dir>/<channel>/<YYYY-MM-DD>/<HH>0000_<MHz>.wav`, with the gaps between
//! transmissions filled with silence so the file's timeline is wall-clock
//! time. The WAV header is refreshed every few seconds, so a file being
//! written is always playable.
//!
//! File writes are small appends (about 1 KB per channel every few tens of
//! milliseconds), done inline on the output task.

use crate::airband::config::{selected, AirbandOutputs, RecordFormat};
use crate::airband::{hub, AudioEvent, ChannelMeta, StreamId};
use chrono::{DateTime, Timelike, Utc};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use xng_mode_airband::codec::{i16_le_bytes, to_i16, wav_header};
use xng_types::Message;

const RATE: u32 = crate::airband::hub::AUDIO_RATE;
/// Refresh continuous-file headers this often (samples).
const HEADER_REFRESH: u64 = RATE as u64 * 5;
/// Never pad more than this much silence in one go (a long outage).
const MAX_GAP_S: i64 = 3_600;

/// Folder-safe name for a channel: its label, else its frequency.
fn channel_dir(chan: &ChannelMeta, stream: StreamId) -> String {
    match stream {
        StreamId::Mix => "mix".to_string(),
        StreamId::Scan => "scan".to_string(),
        StreamId::Channel(f) => match &chan.label {
            Some(l) => format!("{}_{:.3}", sanitize(l), f as f64 / 1e6),
            None => format!("{:.3}", f as f64 / 1e6),
        },
    }
}

fn sanitize(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .collect();
    t.trim_matches('_').chars().take(48).collect()
}

/// One file being written.
enum Writer {
    Wav { file: File, path: PathBuf, samples: u64, since_header: u64 },
    #[cfg(feature = "opus")]
    Ogg { file: File, path: PathBuf, samples: u64, stream: crate::airband::opus_enc::OggOpus },
}

impl Writer {
    fn create(path: PathBuf, format: RecordFormat, title: &str) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new().create(true).write(true).truncate(true).open(&path)?;
        match format {
            RecordFormat::Wav => {
                file.write_all(&wav_header(RATE, 0))?;
                Ok(Writer::Wav { file, path, samples: 0, since_header: 0 })
            }
            #[cfg(feature = "opus")]
            RecordFormat::Ogg => {
                let serial = rand_serial(&path);
                let (stream, head) = crate::airband::opus_enc::OggOpus::new(24_000, serial, title)
                    .map_err(std::io::Error::other)?;
                file.write_all(&head)?;
                Ok(Writer::Ogg { file, path, samples: 0, stream })
            }
            #[cfg(not(feature = "opus"))]
            RecordFormat::Ogg => {
                let _ = title;
                Err(std::io::Error::other("ogg recordings need xng built with the `opus` feature"))
            }
        }
    }

    fn write(&mut self, samples: &[f32]) -> std::io::Result<()> {
        match self {
            Writer::Wav { file, samples: n, since_header, .. } => {
                file.write_all(&i16_le_bytes(&to_i16(samples)))?;
                *n += samples.len() as u64;
                *since_header += samples.len() as u64;
                Ok(())
            }
            #[cfg(feature = "opus")]
            Writer::Ogg { file, samples: n, stream, .. } => {
                *n += samples.len() as u64;
                file.write_all(&stream.push(samples))
            }
        }
    }

    fn samples(&self) -> u64 {
        match self {
            Writer::Wav { samples, .. } => *samples,
            #[cfg(feature = "opus")]
            Writer::Ogg { samples, .. } => *samples,
        }
    }

    fn path(&self) -> &Path {
        match self {
            Writer::Wav { path, .. } => path,
            #[cfg(feature = "opus")]
            Writer::Ogg { path, .. } => path,
        }
    }

    /// Rewrite the WAV sizes so the file is valid as written so far.
    fn refresh_header(&mut self) -> std::io::Result<()> {
        if let Writer::Wav { file, samples, since_header, .. } = self {
            let pos = file.stream_position()?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&wav_header(RATE, (*samples * 2).min(u32::MAX as u64 - 36) as u32))?;
            file.seek(SeekFrom::Start(pos))?;
            *since_header = 0;
        }
        Ok(())
    }

    fn finish(mut self) -> std::io::Result<PathBuf> {
        self.refresh_header()?;
        #[cfg(feature = "opus")]
        if let Writer::Ogg { file, stream, .. } = &mut self {
            file.write_all(&stream.finish())?;
        }
        Ok(self.path().to_path_buf())
    }
}

#[cfg(feature = "opus")]
fn rand_serial(path: &Path) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    Utc::now().timestamp_nanos_opt().hash(&mut h);
    h.finish() as u32
}

fn ext(format: RecordFormat) -> &'static str {
    match format {
        RecordFormat::Wav => "wav",
        RecordFormat::Ogg => "ogg",
    }
}

/// A transmission being recorded in split mode.
struct Split {
    writer: Writer,
    start: DateTime<Utc>,
    part: u32,
}

/// A continuous hourly file.
struct Hourly {
    writer: Writer,
    hour: DateTime<Utc>,
    /// Wall-clock time just after the last written sample.
    cursor: DateTime<Utc>,
}

pub struct Recorder {
    dir: PathBuf,
    format: RecordFormat,
    continuous: bool,
    min_samples: u64,
    max_samples: u64,
    streams: Vec<StreamId>,
    split: HashMap<StreamId, Split>,
    hourly: HashMap<StreamId, Hourly>,
}

impl Recorder {
    pub fn new(o: &AirbandOutputs) -> anyhow::Result<Option<Self>> {
        let Some(dir) = o.record_dir.clone() else { return Ok(None) };
        fs::create_dir_all(&dir)?;
        Ok(Some(Self {
            dir,
            format: o.record_format,
            continuous: o.record_continuous,
            min_samples: o.record_min_ms as u64 * RATE as u64 / 1_000,
            max_samples: if o.record_max_s == 0 { u64::MAX } else { o.record_max_s as u64 * RATE as u64 },
            streams: crate::airband::config::parse_streams(&o.record_streams)?,
            split: HashMap::new(),
            hourly: HashMap::new(),
        }))
    }

    fn split_path(&self, chan: &ChannelMeta, stream: StreamId, start: DateTime<Utc>, part: u32) -> PathBuf {
        let mhz = if chan.freq_hz > 0 { format!("{:.3}", chan.freq_hz as f64 / 1e6) } else { stream.to_string() };
        let suffix = if part > 0 { format!("_part{part}") } else { String::new() };
        self.dir
            .join(channel_dir(chan, stream))
            .join(start.format("%Y-%m-%d").to_string())
            .join(format!("{}_{mhz}{suffix}.{}", start.format("%H%M%S"), ext(self.format)))
    }

    fn hour_path(&self, chan: &ChannelMeta, stream: StreamId, hour: DateTime<Utc>) -> PathBuf {
        let mhz = if chan.freq_hz > 0 { format!("{:.3}", chan.freq_hz as f64 / 1e6) } else { stream.to_string() };
        self.dir
            .join(channel_dir(chan, stream))
            .join(hour.format("%Y-%m-%d").to_string())
            .join(format!("{}0000_{mhz}.{}", hour.format("%H"), ext(self.format)))
    }

    pub fn handle(&mut self, ev: &AudioEvent) -> std::io::Result<()> {
        if !selected(&self.streams, ev.stream()) {
            return Ok(());
        }
        if self.continuous {
            self.handle_continuous(ev)
        } else {
            self.handle_split(ev)
        }
    }

    fn handle_split(&mut self, ev: &AudioEvent) -> std::io::Result<()> {
        match ev {
            AudioEvent::Open { stream, chan, at, .. } => {
                if let Some(old) = self.split.remove(stream) {
                    old.writer.finish()?;
                }
                let path = self.split_path(chan, *stream, *at, 0);
                let writer = Writer::create(path, self.format, &chan.display())?;
                self.split.insert(*stream, Split { writer, start: *at, part: 0 });
            }
            AudioEvent::Audio(f) => {
                let max = self.max_samples;
                let rollover = match self.split.get_mut(&f.stream) {
                    Some(s) => {
                        s.writer.write(&f.samples)?;
                        s.writer.samples() >= max
                    }
                    None => false,
                };
                if rollover {
                    let s = self.split.remove(&f.stream).expect("present");
                    let part = s.part + 1;
                    let start = s.start;
                    s.writer.finish()?;
                    let path = self.split_path(&f.chan, f.stream, start, part);
                    let writer = Writer::create(path, self.format, &f.chan.display())?;
                    self.split.insert(f.stream, Split { writer, start, part });
                }
            }
            AudioEvent::Close { stream, chan, tx, duration_s, peak_dbfs, noise_dbfs, .. } => {
                if let Some(s) = self.split.remove(stream) {
                    let short = s.part == 0 && s.writer.samples() < self.min_samples;
                    let path = s.writer.finish()?;
                    if short {
                        let _ = fs::remove_file(&path);
                    } else {
                        let meta = serde_json::json!({
                            "stream": stream.to_string(),
                            "freq_hz": chan.freq_hz,
                            "label": chan.label,
                            "atc_frequency": chan.atc_frequency,
                            "modulation": chan.modulation,
                            "tx": tx,
                            "start": s.start.to_rfc3339(),
                            "duration_s": duration_s,
                            "peak_dbfs": finite(*peak_dbfs),
                            "noise_dbfs": finite(*noise_dbfs),
                            "part": s.part,
                        });
                        let _ = fs::write(path.with_extension("json"), format!("{meta:#}\n"));
                        tracing::debug!("airband recording {}", path.display());
                    }
                }
            }
            AudioEvent::Status(_) => {}
        }
        Ok(())
    }

    fn handle_continuous(&mut self, ev: &AudioEvent) -> std::io::Result<()> {
        let AudioEvent::Audio(f) = ev else { return Ok(()) };
        let hour = f
            .at
            .with_minute(0)
            .and_then(|t| t.with_second(0))
            .and_then(|t| t.with_nanosecond(0))
            .unwrap_or(f.at);
        let rotate = self.hourly.get(&f.stream).is_none_or(|h| h.hour != hour);
        if rotate {
            if let Some(old) = self.hourly.remove(&f.stream) {
                old.writer.finish()?;
            }
            let path = self.hour_path(&f.chan, f.stream, hour);
            let writer = Writer::create(path, self.format, &f.chan.display())?;
            self.hourly.insert(f.stream, Hourly { writer, hour, cursor: hour });
        }
        let h = self.hourly.get_mut(&f.stream).expect("just inserted");
        // Pad the silence since the last write (bounded), so position in the
        // file is time since the top of the hour.
        let gap_ms = (f.at - h.cursor).num_milliseconds().clamp(0, MAX_GAP_S * 1_000);
        let gap = (gap_ms as u64 * RATE as u64 / 1_000) as usize;
        if gap > RATE as usize / 50 {
            const CHUNK: usize = 16_000;
            let zeros = [0.0f32; CHUNK];
            let mut left = gap;
            while left > 0 {
                let n = left.min(CHUNK);
                h.writer.write(&zeros[..n])?;
                left -= n;
            }
        }
        h.writer.write(&f.samples)?;
        h.cursor = f.at + chrono::Duration::microseconds((f.samples.len() as i64) * 1_000_000 / RATE as i64);
        if let Writer::Wav { since_header, .. } = &h.writer {
            if *since_header >= HEADER_REFRESH {
                h.writer.refresh_header()?;
            }
        }
        Ok(())
    }

    /// Close every open file (end of session).
    pub fn finish(&mut self) {
        for (_, s) in self.split.drain() {
            let _ = s.writer.finish();
        }
        for (_, h) in self.hourly.drain() {
            let _ = h.writer.finish();
        }
    }
}

fn finite(v: f32) -> Option<f32> {
    v.is_finite().then_some((v * 10.0).round() / 10.0)
}

/// Run the recorder until the session's bus closes.
pub async fn run(bus_rx: broadcast::Receiver<Arc<Message>>, mut rec: Recorder) -> std::io::Result<()> {
    let mut rx = hub().subscribe();
    let closed = crate::airband::bus_closed(bus_rx);
    tokio::pin!(closed);
    tracing::info!("airband recordings under {}", rec.dir.display());
    loop {
        tokio::select! {
            _ = &mut closed => break,
            ev = rx.recv() => match ev {
                Ok(ev) => {
                    if let Err(e) = rec.handle(&ev) {
                        tracing::warn!("airband recording: {e}");
                    }
                }
                Err(RecvError::Lagged(n)) => tracing::warn!("airband recorder lagged, dropped {n} events"),
                Err(RecvError::Closed) => break,
            },
        }
    }
    rec.finish();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::airband::AudioFrame;
    use xng_mode_airband::squelch::SquelchMode;
    use xng_mode_airband::Modulation;

    fn meta() -> Arc<ChannelMeta> {
        Arc::new(ChannelMeta {
            freq_hz: 118_300_000,
            label: Some("KSFO Tower".into()),
            key: "k".into(),
            atc_frequency: None,
            modulation: Modulation::Am,
            bandwidth_hz: 8_000.0,
            squelch: SquelchMode::default(),
            sdr: None,
        })
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xng-rec-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn frame(at: DateTime<Utc>, n: usize) -> AudioEvent {
        AudioEvent::Audio(Arc::new(AudioFrame {
            stream: StreamId::Channel(118_300_000),
            chan: meta(),
            tx: 1,
            at,
            samples: vec![0.25f32; n].into(),
            discontinuity: false,
        }))
    }

    fn outputs(dir: &Path) -> AirbandOutputs {
        AirbandOutputs { record_dir: Some(dir.to_path_buf()), ..Default::default() }
    }

    #[test]
    fn split_writes_wav_and_sidecar() {
        let dir = tmpdir("split");
        let mut r = Recorder::new(&outputs(&dir)).unwrap().unwrap();
        let t0 = "2026-11-22T10:15:30Z".parse::<DateTime<Utc>>().unwrap();
        let s = StreamId::Channel(118_300_000);
        r.handle(&AudioEvent::Open { stream: s, chan: meta(), tx: 1, at: t0, level_dbfs: -20.0, noise_dbfs: -50.0 })
            .unwrap();
        r.handle(&frame(t0, 16_000)).unwrap();
        r.handle(&AudioEvent::Close {
            stream: s,
            chan: meta(),
            tx: 1,
            at: t0,
            duration_s: 1.0,
            peak_dbfs: -18.0,
            noise_dbfs: -50.0,
        })
        .unwrap();
        let wav = dir.join("KSFO_Tower_118.300/2026-11-22/101530_118.300.wav");
        let bytes = fs::read(&wav).unwrap();
        assert_eq!(bytes.len(), 44 + 32_000);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 32_000);
        let side: serde_json::Value = serde_json::from_str(&fs::read_to_string(wav.with_extension("json")).unwrap()).unwrap();
        assert_eq!(side["label"], "KSFO Tower");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_drops_short_transmissions() {
        let dir = tmpdir("short");
        let o = AirbandOutputs { record_min_ms: 500, ..outputs(&dir) };
        let mut r = Recorder::new(&o).unwrap().unwrap();
        let t0 = Utc::now();
        let s = StreamId::Channel(118_300_000);
        r.handle(&AudioEvent::Open { stream: s, chan: meta(), tx: 1, at: t0, level_dbfs: 0.0, noise_dbfs: 0.0 }).unwrap();
        r.handle(&frame(t0, 1_600)).unwrap();
        r.handle(&AudioEvent::Close { stream: s, chan: meta(), tx: 1, at: t0, duration_s: 0.1, peak_dbfs: 0.0, noise_dbfs: 0.0 })
            .unwrap();
        let n = walk(&dir).iter().filter(|p| p.extension().is_some_and(|e| e == "wav")).count();
        assert_eq!(n, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn continuous_pads_gaps_with_silence() {
        let dir = tmpdir("cont");
        let o = AirbandOutputs { record_continuous: true, ..outputs(&dir) };
        let mut r = Recorder::new(&o).unwrap().unwrap();
        let t0 = "2026-11-22T10:00:01Z".parse::<DateTime<Utc>>().unwrap();
        r.handle(&frame(t0, 16_000)).unwrap();
        // Next transmission 3 s after the first ended.
        r.handle(&frame(t0 + chrono::Duration::seconds(4), 16_000)).unwrap();
        r.finish();
        let wav = dir.join("KSFO_Tower_118.300/2026-11-22/100000_118.300.wav");
        let bytes = fs::read(&wav).unwrap();
        // 1 s lead-in silence + 1 s + 3 s gap + 1 s = 6 s.
        assert_eq!((bytes.len() - 44) / 2, 6 * 16_000);
        let _ = fs::remove_dir_all(&dir);
    }

    fn walk(d: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    out.extend(walk(&p));
                } else {
                    out.push(p);
                }
            }
        }
        out
    }
}
