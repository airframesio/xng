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
use crate::airband::{AudioEvent, ChannelMeta, StreamId};
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
    let t: String =
        s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect();
    t.trim_matches('_').chars().take(48).collect()
}

/// One file being written.
enum Writer {
    Wav {
        file: File,
        path: PathBuf,
        samples: u64,
        since_header: u64,
    },
    #[cfg(feature = "opus")]
    Ogg {
        file: File,
        path: PathBuf,
        samples: u64,
        stream: crate::airband::opus_enc::OggOpus,
    },
}

/// Create `path` exclusively; when it already exists (a restart, or two
/// transmissions in the same second), use `<stem>_2.<ext>`, `_3`, ... so an
/// existing recording is never overwritten.
fn create_unique(path: PathBuf) -> std::io::Result<(File, PathBuf)> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut candidate = path.clone();
    for n in 2..1_000 {
        match OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(f) => return Ok((f, candidate)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                candidate = path.with_file_name(format!("{stem}_{n}.{ext}"));
            }
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other(format!("too many recordings named like {}", path.display())))
}

impl Writer {
    fn create(path: PathBuf, format: RecordFormat, title: &str) -> std::io::Result<Self> {
        let (mut file, path) = create_unique(path)?;
        match format {
            RecordFormat::Wav => {
                file.write_all(&wav_header(RATE, 0))?;
                Ok(Writer::Wav { file, path, samples: 0, since_header: 0 })
            }
            #[cfg(feature = "opus")]
            RecordFormat::Ogg => {
                let serial = rand_serial(&path);
                let (stream, head) =
                    crate::airband::opus_enc::OggOpus::new(24_000, serial, title).map_err(std::io::Error::other)?;
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
    #[allow(irrefutable_let_patterns)] // only WAV exists without `opus`
    fn refresh_header(&mut self) -> std::io::Result<()> {
        if let Writer::Wav { file, samples, since_header, .. } = self {
            let pos = file.stream_position()?;
            let header = wav_header(RATE, (*samples * 2).min(u32::MAX as u64 - 36) as u32);
            let wrote = file.seek(SeekFrom::Start(0)).and_then(|_| file.write_all(&header));
            // Always try to get back to the append position; if that fails
            // the caller must stop using this writer.
            file.seek(SeekFrom::Start(pos))?;
            wrote?;
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

fn split_path_for(
    dir: &Path,
    format: RecordFormat,
    chan: &ChannelMeta,
    stream: StreamId,
    start: DateTime<Utc>,
    part: u32,
) -> PathBuf {
    let mhz = if chan.freq_hz > 0 { format!("{:.3}", chan.freq_hz as f64 / 1e6) } else { stream.to_string() };
    let suffix = if part > 0 { format!("_part{part}") } else { String::new() };
    dir.join(channel_dir(chan, stream))
        .join(start.format("%Y-%m-%d").to_string())
        .join(format!("{}_{mhz}{suffix}.{}", start.format("%H%M%S"), ext(format)))
}

fn ext(format: RecordFormat) -> &'static str {
    match format {
        RecordFormat::Wav => "wav",
        RecordFormat::Ogg => "ogg",
    }
}

/// A transmission being recorded in split mode.
struct Split {
    /// The part being written; `None` after a rollover until more audio
    /// arrives, so an exact-length transmission leaves no empty part.
    writer: Option<Writer>,
    start: DateTime<Utc>,
    part: u32,
    /// Finished earlier parts of this transmission (each gets a sidecar).
    parts: Vec<PathBuf>,
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
    /// Sidecars of recent transmissions, so a transcript that arrives after
    /// the file is finished can be added to them.
    sidecars: std::collections::VecDeque<((StreamId, u64), Vec<PathBuf>)>,
}

/// Transmissions whose sidecars stay findable for late transcripts.
const SIDECARS_KEPT: usize = 256;

impl Recorder {
    pub fn new(o: &AirbandOutputs) -> anyhow::Result<Option<Self>> {
        let Some(dir) = o.record_dir.clone() else { return Ok(None) };
        fs::create_dir_all(&dir)?;
        Ok(Some(Self {
            dir,
            format: o.record_format,
            continuous: o.record_continuous,
            min_samples: o.record_min_ms as u64 * RATE as u64 / 1_000,
            max_samples: {
                let max = if o.record_max_s == 0 { u64::MAX } else { o.record_max_s as u64 * RATE as u64 };
                match o.record_format {
                    // Classic RIFF sizes are 32-bit: roll over before 4 GiB.
                    RecordFormat::Wav => max.min((u32::MAX as u64 - 36) / 2),
                    RecordFormat::Ogg => max,
                }
            },
            streams: crate::airband::config::parse_streams(&o.record_streams)?,
            split: HashMap::new(),
            hourly: HashMap::new(),
            sidecars: Default::default(),
        }))
    }

    fn split_path(&self, chan: &ChannelMeta, stream: StreamId, start: DateTime<Utc>, part: u32) -> PathBuf {
        split_path_for(&self.dir, self.format, chan, stream, start, part)
    }

    fn hour_path(&self, chan: &ChannelMeta, stream: StreamId, hour: DateTime<Utc>) -> PathBuf {
        // Scan/mix files are named by the stream, not whichever channel is
        // active, so the hour's file name is stable.
        let mhz = match stream {
            StreamId::Channel(f) => format!("{:.3}", f as f64 / 1e6),
            _ => stream.to_string(),
        };
        let _ = chan;
        self.dir.join(channel_dir(chan, stream)).join(hour.format("%Y-%m-%d").to_string()).join(format!(
            "{}0000_{mhz}.{}",
            hour.format("%H"),
            ext(self.format)
        ))
    }

    pub fn handle(&mut self, ev: &AudioEvent) -> std::io::Result<()> {
        if !selected(&self.streams, ev.stream()) {
            return Ok(());
        }
        if let AudioEvent::Transcript(t) = ev {
            self.add_transcript(t);
            return Ok(());
        }
        let r = if self.continuous { self.handle_continuous(ev) } else { self.handle_split(ev) };
        if r.is_err() {
            // A failed write leaves the file in an unknown state: stop using
            // it; the next audio starts a new file.
            let stream = ev.stream();
            self.split.remove(&stream);
            self.hourly.remove(&stream);
        }
        r
    }

    /// Split mode: add the text to the transmission's sidecar(s). Continuous
    /// mode: append a line to the hour's `.transcripts.jsonl` next to it.
    fn add_transcript(&mut self, t: &crate::airband::hub::Transcript) {
        if self.continuous {
            let path = self.hour_path(&t.chan, t.stream, top_of_hour(t.start)).with_extension("transcripts.jsonl");
            let line = serde_json::json!({
                "tx": t.tx, "start": t.start.to_rfc3339(), "end": t.end.to_rfc3339(),
                "text": t.text, "engine": t.engine, "model": t.model, "confidence": t.confidence,
            });
            let r = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes()));
            if let Err(e) = r {
                tracing::warn!("airband transcript {}: {e}", path.display());
            }
            return;
        }
        let Some((_, paths)) = self.sidecars.iter().find(|(k, _)| *k == (t.stream, t.tx)) else { return };
        for p in paths {
            let Ok(text) = fs::read_to_string(p) else { continue };
            let Ok(mut meta) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
            // A long transmission is transcribed in pieces; join them.
            let joined = match meta["transcript"].as_str() {
                Some(prev) if !prev.is_empty() => format!("{prev} {}", t.text),
                _ => t.text.clone(),
            };
            meta["transcript"] = serde_json::json!(joined);
            meta["transcript_engine"] = serde_json::json!(t.engine);
            meta["transcript_model"] = serde_json::json!(t.model);
            if let Err(e) = fs::write(p, format!("{meta:#}\n")) {
                tracing::warn!("airband transcript {}: {e}", p.display());
            }
        }
    }

    fn handle_split(&mut self, ev: &AudioEvent) -> std::io::Result<()> {
        match ev {
            AudioEvent::Open { stream, chan, at, .. } => {
                if let Some(old) = self.split.remove(stream) {
                    if let Some(w) = old.writer {
                        w.finish()?;
                    }
                }
                let path = self.split_path(chan, *stream, *at, 0);
                let writer = Writer::create(path, self.format, &chan.display())?;
                self.split.insert(*stream, Split { writer: Some(writer), start: *at, part: 0, parts: Vec::new() });
            }
            AudioEvent::Audio(f) => {
                if f.samples.is_empty() {
                    return Ok(());
                }
                let (max, format) = (self.max_samples, self.format);
                let next_path = match self.split.get(&f.stream) {
                    Some(s) if s.writer.is_none() => Some(self.split_path(&f.chan, f.stream, s.start, s.part)),
                    _ => None,
                };
                let Some(s) = self.split.get_mut(&f.stream) else { return Ok(()) };
                if let Some(path) = next_path {
                    s.writer = Some(Writer::create(path, format, &f.chan.display())?);
                }
                let mut rest: &[f32] = &f.samples;
                while !rest.is_empty() {
                    let Some(w) = &mut s.writer else { break };
                    let room = (max - w.samples()).min(rest.len() as u64) as usize;
                    w.write(&rest[..room])?;
                    rest = &rest[room..];
                    if w.samples() >= max {
                        // Roll over; the next part opens when audio continues.
                        let done = s.writer.take().expect("present").finish()?;
                        s.parts.push(done);
                        s.part += 1;
                        if !rest.is_empty() {
                            let path = split_path_for(&self.dir, format, &f.chan, f.stream, s.start, s.part);
                            s.writer = Some(Writer::create(path, format, &f.chan.display())?);
                        }
                    }
                }
            }
            AudioEvent::Close { stream, chan, tx, duration_s, peak_dbfs, noise_dbfs, .. } => {
                if let Some(s) = self.split.remove(stream) {
                    let mut parts = s.parts;
                    let short = parts.is_empty() && s.writer.as_ref().is_none_or(|w| w.samples() < self.min_samples);
                    if let Some(w) = s.writer {
                        parts.push(w.finish()?);
                    }
                    if short {
                        for p in &parts {
                            let _ = fs::remove_file(p);
                        }
                        return Ok(());
                    }
                    let n_parts = parts.len();
                    let mut written = Vec::with_capacity(n_parts);
                    for (i, path) in parts.iter().enumerate() {
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
                            "part": i,
                            "parts": n_parts,
                        });
                        let side = path.with_extension("json");
                        let _ = fs::write(&side, format!("{meta:#}\n"));
                        written.push(side);
                        tracing::debug!("airband recording {}", path.display());
                    }
                    self.sidecars.push_back(((*stream, *tx), written));
                    while self.sidecars.len() > SIDECARS_KEPT {
                        self.sidecars.pop_front();
                    }
                }
            }
            AudioEvent::Status(_) | AudioEvent::Transcript(_) => {}
        }
        Ok(())
    }

    fn handle_continuous(&mut self, ev: &AudioEvent) -> std::io::Result<()> {
        let AudioEvent::Audio(f) = ev else { return Ok(()) };
        // A frame that crosses the top of the hour is split between the two
        // hours' files; one ending exactly on it does not open the next file.
        let hour = top_of_hour(f.at);
        let next = hour + chrono::Duration::hours(1);
        let in_this_hour = ((next - f.at).num_microseconds().unwrap_or(i64::MAX).max(0) as u64 * RATE as u64
            / 1_000_000) as usize;
        if in_this_hour < f.samples.len() {
            let (a, b) = f.samples.split_at(in_this_hour);
            self.write_hourly(f.stream, &f.chan, f.at, hour, a)?;
            return self.write_hourly(f.stream, &f.chan, next, next, b);
        }
        self.write_hourly(f.stream, &f.chan, f.at, hour, &f.samples)
    }

    fn write_hourly(
        &mut self,
        stream: StreamId,
        chan: &ChannelMeta,
        at: DateTime<Utc>,
        hour: DateTime<Utc>,
        samples: &[f32],
    ) -> std::io::Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        if self.hourly.get(&stream).is_none_or(|h| h.hour != hour) {
            if let Some(old) = self.hourly.remove(&stream) {
                old.writer.finish()?;
            }
            let path = self.hour_path(chan, stream, hour);
            let writer = Writer::create(path, self.format, &chan.display())?;
            self.hourly.insert(stream, Hourly { writer, hour, cursor: hour });
        }
        let h = self.hourly.get_mut(&stream).expect("just inserted");
        // Pad the silence since the last write (bounded), so position in the
        // file is time since the top of the hour. Gaps up to 20 ms are left
        // to accumulate (the cursor only moves by audio written) and are
        // padded once they add up.
        let gap_ms = (at - h.cursor).num_milliseconds().clamp(0, MAX_GAP_S * 1_000);
        let gap = (gap_ms as u64 * RATE as u64 / 1_000) as usize;
        if gap > RATE as usize / 50 {
            const CHUNK: usize = 16_000;
            let zeros = [0.0f32; CHUNK];
            let mut left = gap;
            while left > 0 {
                let n = left.min(CHUNK);
                h.writer.write(&zeros[..n])?;
                h.cursor += samples_duration(n);
                left -= n;
            }
        }
        h.writer.write(samples)?;
        h.cursor += samples_duration(samples.len());
        #[allow(irrefutable_let_patterns)]
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
            if let Some(w) = s.writer {
                let _ = w.finish();
            }
        }
        for (_, h) in self.hourly.drain() {
            let _ = h.writer.finish();
        }
    }
}

fn top_of_hour(t: DateTime<Utc>) -> DateTime<Utc> {
    t.with_minute(0).and_then(|t| t.with_second(0)).and_then(|t| t.with_nanosecond(0)).unwrap_or(t)
}

fn samples_duration(n: usize) -> chrono::Duration {
    chrono::Duration::microseconds(n as i64 * 1_000_000 / RATE as i64)
}

fn finite(v: f32) -> Option<f32> {
    v.is_finite().then_some((v * 10.0).round() / 10.0)
}

/// Run the recorder until the session's bus closes.
/// `rx` is subscribed to the audio hub before decoding starts, so the first
/// audio is not missed.
pub async fn run(
    mut rx: tokio::sync::broadcast::Receiver<crate::airband::hub::AudioEvent>,
    bus_rx: broadcast::Receiver<Arc<Message>>,
    mut rec: Recorder,
) -> std::io::Result<()> {
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
    // The decoders have stopped; write what is still queued (including the
    // scanner's last events) before closing the files.
    let drain_until = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while tokio::time::Instant::now() < drain_until {
        match tokio::time::timeout(std::time::Duration::from_millis(250), rx.recv()).await {
            Ok(Ok(ev)) => {
                if let Err(e) = rec.handle(&ev) {
                    tracing::warn!("airband recording: {e}");
                }
            }
            Ok(Err(RecvError::Lagged(_))) => continue,
            _ => break,
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
        let side: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(wav.with_extension("json")).unwrap()).unwrap();
        assert_eq!(side["label"], "KSFO Tower");
        // A transcript that arrives later lands in the sidecar, pieces joined.
        for text in ["United 12 cleared to land", "runway 28 right"] {
            r.handle(&AudioEvent::Transcript(std::sync::Arc::new(crate::airband::hub::Transcript {
                stream: s,
                chan: meta(),
                freq_hz: 118_300_000,
                tx: 1,
                start: t0,
                end: t0,
                text: text.into(),
                engine: "whisper.cpp".into(),
            engine_version: Some("1.8.2".into()),
            model: "base.en".into(),
                language: None,
                confidence: None,
            })))
            .unwrap();
        }
        let side: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(wav.with_extension("json")).unwrap()).unwrap();
        assert_eq!(side["transcript"], "United 12 cleared to land runway 28 right");
        assert_eq!(side["transcript_model"], "base.en");
        assert_eq!(side["transcript_engine"], "whisper.cpp");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_drops_short_transmissions() {
        let dir = tmpdir("short");
        let o = AirbandOutputs { record_min_ms: 500, ..outputs(&dir) };
        let mut r = Recorder::new(&o).unwrap().unwrap();
        let t0 = Utc::now();
        let s = StreamId::Channel(118_300_000);
        r.handle(&AudioEvent::Open { stream: s, chan: meta(), tx: 1, at: t0, level_dbfs: 0.0, noise_dbfs: 0.0 })
            .unwrap();
        r.handle(&frame(t0, 1_600)).unwrap();
        r.handle(&AudioEvent::Close {
            stream: s,
            chan: meta(),
            tx: 1,
            at: t0,
            duration_s: 0.1,
            peak_dbfs: 0.0,
            noise_dbfs: 0.0,
        })
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

    fn open_close(r: &mut Recorder, t: DateTime<Utc>, n: usize) {
        let s = StreamId::Channel(118_300_000);
        r.handle(&AudioEvent::Open { stream: s, chan: meta(), tx: 1, at: t, level_dbfs: 0.0, noise_dbfs: 0.0 })
            .unwrap();
        r.handle(&frame(t, n)).unwrap();
        r.handle(&AudioEvent::Close {
            stream: s,
            chan: meta(),
            tx: 1,
            at: t,
            duration_s: 1.0,
            peak_dbfs: 0.0,
            noise_dbfs: 0.0,
        })
        .unwrap();
    }

    fn wavs(dir: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = walk(dir).into_iter().filter(|p| p.extension().is_some_and(|e| e == "wav")).collect();
        v.sort();
        v
    }

    #[test]
    fn same_second_transmissions_get_their_own_files() {
        let dir = tmpdir("samesec");
        let mut r = Recorder::new(&outputs(&dir)).unwrap().unwrap();
        let t0 = "2026-11-22T10:15:30Z".parse::<DateTime<Utc>>().unwrap();
        open_close(&mut r, t0, 16_000);
        open_close(&mut r, t0 + chrono::Duration::milliseconds(500), 8_000);
        let w = wavs(&dir);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w.iter().all(|p| p.with_extension("json").exists()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rollover_parts_each_get_a_sidecar_and_no_empty_part() {
        let dir = tmpdir("parts");
        let o = AirbandOutputs { record_max_s: 1, ..outputs(&dir) };
        let mut r = Recorder::new(&o).unwrap().unwrap();
        // Exactly two parts' worth of audio.
        open_close(&mut r, "2026-11-22T10:15:30Z".parse().unwrap(), 32_000);
        let w = wavs(&dir);
        assert_eq!(w.len(), 2, "{w:?}");
        for p in &w {
            assert_eq!(fs::read(p).unwrap().len(), 44 + 32_000);
            assert!(p.with_extension("json").exists());
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn continuous_restart_keeps_the_earlier_file() {
        let dir = tmpdir("restart");
        let o = AirbandOutputs { record_continuous: true, ..outputs(&dir) };
        let t0 = "2026-11-22T10:00:01Z".parse::<DateTime<Utc>>().unwrap();
        for k in 0..2 {
            let mut r = Recorder::new(&o).unwrap().unwrap();
            r.handle(&frame(t0 + chrono::Duration::seconds(10 * k), 16_000)).unwrap();
            r.finish();
        }
        assert_eq!(wavs(&dir).len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn continuous_splits_a_frame_at_the_hour() {
        let dir = tmpdir("hour");
        let o = AirbandOutputs { record_continuous: true, ..outputs(&dir) };
        let mut r = Recorder::new(&o).unwrap().unwrap();
        // 1 s of audio starting 0.5 s before 11:00.
        r.handle(&frame("2026-11-22T10:59:59.500Z".parse().unwrap(), 16_000)).unwrap();
        r.finish();
        let w = wavs(&dir);
        assert_eq!(w.len(), 2, "{w:?}");
        let next = fs::read(dir.join("KSFO_Tower_118.300/2026-11-22/110000_118.300.wav")).unwrap();
        assert_eq!((next.len() - 44) / 2, 8_000);
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
