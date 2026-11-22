//! The `scan` stream: one audio stream that follows whichever channel the
//! scanner selects (priority, hold, sticky pin, lockout). It is built from
//! the per-channel streams on the hub, so it spans every session (several
//! SDRs scan as one receiver).

use super::hub::{hub, AudioEvent, AudioFrame, ScanState, StreamId};
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use xng_mode_airband::scan::{ScanConfig, ScanController};

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Install the scanner (before outputs that pick the scan stream start).
pub fn install(cfg: ScanConfig) {
    let lockout = cfg.lockout.clone();
    *hub().scan.lock().unwrap() =
        Some(ScanState { ctl: ScanController::new(cfg), selected: None, locked_out: lockout });
}

/// Run the scan stream. The hub never closes, so the caller ends this task
/// together with the session.
pub async fn run(mut rx: tokio::sync::broadcast::Receiver<crate::airband::hub::AudioEvent>) {
    let mut follower = Follower::default();
    loop {
        match rx.recv().await {
            Ok(ev) => {
                for out in follower.step(&ev, now_ms()) {
                    hub().publish(out);
                }
            }
            Err(RecvError::Lagged(n)) => {
                tracing::warn!("airband scan stream lagged, dropped {n} events; resyncing");
                for out in follower.resync(now_ms()) {
                    hub().publish(out);
                }
            }
            Err(RecvError::Closed) => break,
        }
    }
}

/// Pin the scanner to one channel, or resume scanning with `None`.
pub fn pin(freq: Option<u64>) -> bool {
    match hub().scan.lock().unwrap().as_mut() {
        Some(s) => {
            s.ctl.pin(freq);
            true
        }
        None => false,
    }
}

/// Lock a channel out of (or back into) the scan.
pub fn lockout(freq: u64, locked: bool) -> bool {
    match hub().scan.lock().unwrap().as_mut() {
        Some(s) => {
            s.ctl.set_lockout(freq, locked);
            s.locked_out.retain(|&f| f != freq);
            if locked {
                s.locked_out.push(freq);
            }
            true
        }
        None => false,
    }
}

/// Builds the scan stream's events from channel events.
#[derive(Default)]
struct Follower {
    /// Channel the scan stream is currently carrying a transmission from.
    source: Option<u64>,
    tx: u64,
}

impl Follower {
    fn step(&mut self, ev: &AudioEvent, now: u64) -> Vec<AudioEvent> {
        let mut out = Vec::new();
        let StreamId::Channel(freq) = ev.stream() else { return out };
        let mut guard = hub().scan.lock().unwrap();
        let Some(state) = guard.as_mut() else { return out };
        match ev {
            AudioEvent::Open { .. } => state.ctl.on_open(freq, now),
            AudioEvent::Close { .. } => state.ctl.on_close(freq, now),
            _ => {}
        }
        let sel = state.ctl.select(now);
        state.selected = sel;
        drop(guard);

        // Selection moved away from the channel we were carrying.
        if let Some(src) = self.source {
            if sel != Some(src) {
                out.push(self.close(ev, src));
            }
        }
        match ev {
            AudioEvent::Audio(f) if sel == Some(freq) => {
                if self.source.is_none() {
                    self.tx += 1;
                    self.source = Some(freq);
                    out.push(AudioEvent::Open {
                        stream: StreamId::Scan,
                        chan: f.chan.clone(),
                        tx: self.tx,
                        at: f.at,
                        level_dbfs: f32::NEG_INFINITY,
                        noise_dbfs: f32::NEG_INFINITY,
                    });
                }
                out.push(AudioEvent::Audio(Arc::new(AudioFrame {
                    stream: StreamId::Scan,
                    chan: f.chan.clone(),
                    tx: self.tx,
                    at: f.at,
                    samples: f.samples.clone(),
                    discontinuity: f.discontinuity,
                })));
            }
            AudioEvent::Close { .. } if self.source == Some(freq) => {
                out.push(self.close(ev, freq));
            }
            _ => {}
        }
        out
    }

    /// After dropped events: take which channels are open from the hub's
    /// latest status instead of the lost Open/Close events, reselect, and
    /// close the scan transmission if its channel is no longer selected.
    fn resync(&mut self, now: u64) -> Vec<AudioEvent> {
        let status = hub().status();
        let mut guard = hub().scan.lock().unwrap();
        let Some(state) = guard.as_mut() else { return Vec::new() };
        for st in &status {
            match (st.open, state.ctl.is_open(st.freq_hz)) {
                (true, false) => state.ctl.on_open(st.freq_hz, now),
                (false, true) => state.ctl.on_close(st.freq_hz, now),
                _ => {}
            }
        }
        let sel = state.ctl.select(now);
        state.selected = sel;
        drop(guard);
        match self.source {
            Some(src) if sel != Some(src) || !status.iter().any(|st| st.freq_hz == src && st.open) => {
                self.source = None;
                let Some(chan) = hub().channel(src) else { return Vec::new() };
                vec![AudioEvent::Close {
                    stream: StreamId::Scan,
                    chan,
                    tx: self.tx,
                    at: chrono::Utc::now(),
                    duration_s: 0.0,
                    peak_dbfs: f32::NEG_INFINITY,
                    noise_dbfs: f32::NEG_INFINITY,
                }]
            }
            _ => Vec::new(),
        }
    }

    fn close(&mut self, ev: &AudioEvent, src: u64) -> AudioEvent {
        self.source = None;
        let (chan, at, duration_s) = match ev {
            AudioEvent::Close { chan, at, duration_s, .. } if chan.freq_hz == src => (chan.clone(), *at, *duration_s),
            AudioEvent::Audio(f) => (hub().channel(src).unwrap_or_else(|| f.chan.clone()), f.at, 0.0),
            AudioEvent::Open { at, chan, .. } | AudioEvent::Close { at, chan, .. } => {
                (hub().channel(src).unwrap_or_else(|| chan.clone()), *at, 0.0)
            }
            AudioEvent::Status(_) | AudioEvent::Transcript(_) => {
                let chan = hub().channel(src).expect("scan source is a registered channel");
                (chan, chrono::Utc::now(), 0.0)
            }
        };
        AudioEvent::Close {
            stream: StreamId::Scan,
            chan,
            tx: self.tx,
            at,
            duration_s,
            peak_dbfs: f32::NEG_INFINITY,
            noise_dbfs: f32::NEG_INFINITY,
        }
    }
}
