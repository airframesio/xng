//! The airband channels of one session: the multichannel decoder plus the
//! glue that turns its events into hub audio and bus messages.

use super::config::AirbandConfig;
use super::hub::{hub, AudioEvent, AudioFrame, ChannelMeta, ChannelStatus, StreamId};
use chrono::{DateTime, Duration, Utc};
use num_complex::Complex;
use std::sync::Arc;
use xng_mode_airband::{AirbandEvent, AirbandMultiChannelDecoder, TransmissionInfo, CHANNEL_RATE};
use xng_types::{Message, Provenance};

/// Status cadence in channel samples (250 ms).
const STATUS_EVERY: u64 = (CHANNEL_RATE / 4.0) as u64;
/// Re-anchor the sample clock when it falls this far behind the wall clock
/// (the SDR dropped samples).
const CLOCK_SLIP_S: f64 = 1.5;

/// Maps channel sample positions to wall-clock capture times.
struct Clock {
    anchor: Option<(u64, DateTime<Utc>)>,
    slipped: bool,
}

impl Clock {
    fn at(&self, pos: u64) -> DateTime<Utc> {
        match self.anchor {
            Some((p0, t0)) => t0 + Duration::microseconds(((pos as f64 - p0 as f64) * 1e6 / CHANNEL_RATE) as i64),
            None => Utc::now(),
        }
    }

    /// Called once per chunk with the position at its end. A file replay runs
    /// ahead of the wall clock, which is fine; only falling behind (dropped
    /// samples) re-anchors.
    fn tick(&mut self, end_pos: u64) {
        let now = Utc::now();
        match self.anchor {
            None => self.anchor = Some((end_pos, now)),
            Some(_) => {
                let behind = (now - self.at(end_pos)).num_milliseconds() as f64 / 1e3;
                if behind > CLOCK_SLIP_S {
                    tracing::debug!("airband clock re-anchored ({behind:.1} s behind)");
                    self.anchor = Some((end_pos, now));
                    self.slipped = true;
                }
            }
        }
    }
}

struct Chan {
    meta: Arc<ChannelMeta>,
    /// Transmission counter (the id of the open or last transmission).
    tx: u64,
    open_at: Option<DateTime<Utc>>,
    open_level: f32,
}

/// Per-channel result of one chunk, in the shape the decode loop's shared
/// path consumes: `(index, freq, level, messages, seen, ok)`.
pub type ChannelResult = (usize, u64, f32, Vec<Message>, u64, u64);

pub struct AirbandSet {
    dec: AirbandMultiChannelDecoder,
    chans: Vec<Chan>,
    freqs: Vec<u64>,
    clock: Clock,
    last_status: u64,
    // mix stream
    mix: Option<Arc<ChannelMeta>>,
    mix_tx: u64,
    mix_open: bool,
    mix_open_at: Option<DateTime<Utc>>,
    open_count: usize,
}

impl AirbandSet {
    pub fn new(
        sample_rate: f64,
        capture_center: u64,
        freqs: &[u64],
        cfg: &AirbandConfig,
        sdr: Option<String>,
    ) -> Result<Self, String> {
        cfg.validate().map_err(|e| e.to_string())?;
        let offsets: Vec<f64> = freqs.iter().map(|&f| f as f64 - capture_center as f64).collect();
        let mut cfgs = Vec::with_capacity(freqs.len());
        let mut chans = Vec::with_capacity(freqs.len());
        for &f in freqs {
            let (c, ch) = cfg.channel(f);
            let key =
                ch.and_then(|c| c.key.clone()).unwrap_or_else(|| format!("{}:{}", sdr.as_deref().unwrap_or("sdr"), f));
            let meta = Arc::new(ChannelMeta {
                freq_hz: f,
                label: ch.and_then(|c| c.label.clone()),
                key,
                atc_frequency: ch.and_then(|c| c.atc_frequency.clone()),
                modulation: c.modulation,
                bandwidth_hz: c.bandwidth(),
                squelch: c.squelch,
                sdr: sdr.clone(),
            });
            hub().register(meta.clone());
            chans.push(Chan { meta, tx: 0, open_at: None, open_level: f32::NEG_INFINITY });
            cfgs.push(c);
        }
        let dec = AirbandMultiChannelDecoder::new(sample_rate, &offsets, cfgs)?;
        Ok(Self {
            dec,
            chans,
            freqs: freqs.to_vec(),
            clock: Clock { anchor: None, slipped: false },
            last_status: 0,
            mix: cfg.mix.then(|| {
                hub().enable_mix();
                Arc::new(ChannelMeta::mix())
            }),
            mix_tx: 0,
            mix_open: false,
            mix_open_at: None,
            open_count: 0,
        })
    }

    pub fn freqs(&self) -> &[u64] {
        &self.freqs
    }

    pub fn level_dbfs(&self, i: usize) -> f32 {
        self.dec.channel(i).level_dbfs()
    }

    /// Decode one capture chunk. `prov(i, freq)` builds the provenance for
    /// channel `i`.
    pub fn process(&mut self, iq: &[Complex<f32>], prov: impl Fn(usize, u64) -> Provenance) -> Vec<ChannelResult> {
        let per = self.dec.process(iq);
        let end_pos = self.dec.channel(0).position();
        self.clock.tick(end_pos);
        let h = hub();
        let mut out = Vec::with_capacity(per.len());
        // Mix accumulator for this chunk: (start position, summed samples).
        let mut mix_buf: Option<(u64, Vec<f32>)> = None;
        let discontinuity = std::mem::take(&mut self.clock.slipped);

        for (i, events) in per.into_iter().enumerate() {
            let freq = self.freqs[i];
            let mut msgs = Vec::new();
            let mut ended = 0u64;
            for ev in events {
                match ev {
                    AirbandEvent::Start { sample, level_dbfs, noise_dbfs } => {
                        let at = self.clock.at(sample);
                        let c = &mut self.chans[i];
                        c.tx += 1;
                        c.open_at = Some(at);
                        c.open_level = level_dbfs;
                        h.publish(AudioEvent::Open {
                            stream: StreamId::Channel(freq),
                            chan: c.meta.clone(),
                            tx: c.tx,
                            at,
                            level_dbfs,
                            noise_dbfs,
                        });
                        let info = self.info(i, at, None, level_dbfs, noise_dbfs);
                        msgs.push(xng_mode_airband::to_message("tx_start", &info, freq, prov(i, freq)));
                        self.open_count += 1;
                        if self.open_count == 1 {
                            self.mix_start(at);
                        }
                    }
                    AirbandEvent::Audio { sample, samples } => {
                        let at = self.clock.at(sample);
                        if self.mix.is_some() {
                            accumulate(&mut mix_buf, sample, &samples);
                        }
                        let c = &self.chans[i];
                        h.publish(AudioEvent::Audio(Arc::new(AudioFrame {
                            stream: StreamId::Channel(freq),
                            chan: c.meta.clone(),
                            tx: c.tx,
                            at,
                            samples: samples.into(),
                            discontinuity,
                        })));
                    }
                    AirbandEvent::End { sample, duration_s, peak_dbfs, mean_dbfs: _, noise_dbfs } => {
                        let at = self.clock.at(sample);
                        let c = &mut self.chans[i];
                        let start = c.open_at.take().unwrap_or(at);
                        h.publish(AudioEvent::Close {
                            stream: StreamId::Channel(freq),
                            chan: c.meta.clone(),
                            tx: c.tx,
                            at,
                            duration_s,
                            peak_dbfs,
                            noise_dbfs,
                        });
                        let mut info = self.info(i, start, Some(at), peak_dbfs, noise_dbfs);
                        info.duration_s = Some((duration_s * 1_000.0).round() / 1_000.0);
                        msgs.push(xng_mode_airband::to_message("tx_end", &info, freq, prov(i, freq)));
                        ended += 1;
                        self.open_count = self.open_count.saturating_sub(1);
                        if self.open_count == 0 {
                            // Mixed audio of this transmission goes out before its Close.
                            if let Some((start, samples)) = mix_buf.take() {
                                self.publish_mix(start, &samples, discontinuity);
                            }
                            self.mix_end(at);
                        }
                    }
                }
            }
            out.push((i, freq, self.dec.channel(i).level_dbfs(), msgs, ended, ended));
        }

        if let Some((start, samples)) = mix_buf {
            self.publish_mix(start, &samples, discontinuity);
        }

        if end_pos >= self.last_status + STATUS_EVERY {
            self.last_status = end_pos;
            for (i, c) in self.chans.iter().enumerate() {
                let d = self.dec.channel(i);
                h.publish(AudioEvent::Status(Arc::new(ChannelStatus {
                    freq_hz: c.meta.freq_hz,
                    level_dbfs: d.level_dbfs(),
                    noise_dbfs: d.noise_dbfs(),
                    squelch_dbfs: d.squelch_dbfs(),
                    open: d.is_open(),
                    transmissions: d.transmissions(),
                    airtime_s: d.airtime_s(),
                    afc_hz: d.afc_offset_hz(),
                })));
            }
        }
        out
    }

    fn info(
        &self,
        i: usize,
        start: DateTime<Utc>,
        end: Option<DateTime<Utc>>,
        level: f32,
        noise: f32,
    ) -> TransmissionInfo {
        let c = &self.chans[i];
        TransmissionInfo {
            tx: c.tx,
            label: c.meta.label.clone(),
            modulation: c.meta.modulation,
            start: start.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            end: end.map(|e| e.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            duration_s: None,
            level_dbfs: round1(level),
            noise_dbfs: round1(noise),
            atc_frequency: c.meta.atc_frequency.clone(),
        }
    }

    fn mix_start(&mut self, at: DateTime<Utc>) {
        let Some(meta) = &self.mix else { return };
        if self.mix_open {
            return;
        }
        self.mix_open = true;
        self.mix_tx += 1;
        self.mix_open_at = Some(at);
        hub().publish(AudioEvent::Open {
            stream: StreamId::Mix,
            chan: meta.clone(),
            tx: self.mix_tx,
            at,
            level_dbfs: f32::NEG_INFINITY,
            noise_dbfs: f32::NEG_INFINITY,
        });
    }

    fn publish_mix(&self, start: u64, samples: &[f32], discontinuity: bool) {
        let Some(meta) = &self.mix else { return };
        if samples.is_empty() {
            return;
        }
        hub().publish(AudioEvent::Audio(Arc::new(AudioFrame {
            stream: StreamId::Mix,
            chan: meta.clone(),
            tx: self.mix_tx,
            at: self.clock.at(start),
            samples: samples.iter().map(|v| v.clamp(-1.0, 1.0)).collect::<Vec<_>>().into(),
            discontinuity,
        })));
    }

    fn mix_end(&mut self, at: DateTime<Utc>) {
        let Some(meta) = &self.mix else { return };
        if !self.mix_open {
            return;
        }
        self.mix_open = false;
        let start = self.mix_open_at.take().unwrap_or(at);
        hub().publish(AudioEvent::Close {
            stream: StreamId::Mix,
            chan: meta.clone(),
            tx: self.mix_tx,
            at,
            duration_s: (at - start).num_milliseconds() as f64 / 1e3,
            peak_dbfs: f32::NEG_INFINITY,
            noise_dbfs: f32::NEG_INFINITY,
        });
    }
}

fn round1(v: f32) -> f32 {
    if v.is_finite() {
        (v * 10.0).round() / 10.0
    } else {
        v
    }
}

/// Sum `samples` (starting at channel position `pos`) into the chunk's mix
/// buffer. Channels of one session share a sample clock, so positions line
/// up; gaps between transmissions stay silent.
fn accumulate(buf: &mut Option<(u64, Vec<f32>)>, pos: u64, samples: &[f32]) {
    let (start, mix) = buf.get_or_insert_with(|| (pos, Vec::new()));
    if pos < *start {
        // Prepend room (a pre-roll block that starts before the chunk's
        // first contribution).
        let shift = (*start - pos) as usize;
        let mut v = vec![0.0; shift];
        v.extend_from_slice(mix);
        *mix = v;
        *start = pos;
    }
    let off = (pos - *start) as usize;
    if mix.len() < off + samples.len() {
        mix.resize(off + samples.len(), 0.0);
    }
    for (m, s) in mix[off..].iter_mut().zip(samples) {
        *m += s;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_accumulates_and_aligns() {
        let mut b = None;
        accumulate(&mut b, 100, &[0.5, 0.5]);
        accumulate(&mut b, 98, &[0.1, 0.1, 0.1]);
        let (start, m) = b.unwrap();
        assert_eq!(start, 98);
        assert_eq!(m.len(), 4);
        assert!((m[0] - 0.1).abs() < 1e-6);
        assert!((m[2] - 0.6).abs() < 1e-6);
        assert_eq!(m[3], 0.5);
    }
}
