//! Bounded channel between a vendor USB callback thread and [`crate::IqSource::read`].
//!
//! Both Airspy libraries deliver samples on their own thread via a C callback.
//! The callback converts each transfer into an owned `Vec` and `try_send`s it;
//! a full channel drops the transfer (slow consumer) rather than stalling the
//! USB thread, which would overflow the device's own buffers anyway.

use num_complex::Complex;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::Duration;

use crate::SdrError;

/// ~1 s of headroom at typical transfer sizes (65536 samples each).
const CHANNEL_DEPTH: usize = 64;

pub(crate) fn sample_channel() -> (SyncSender<Vec<Complex<f32>>>, SamplePump) {
    let (tx, rx) = sync_channel(CHANNEL_DEPTH);
    (tx, SamplePump { rx, pending: Vec::new(), pos: 0 })
}

pub(crate) struct SamplePump {
    rx: Receiver<Vec<Complex<f32>>>,
    pending: Vec<Complex<f32>>,
    pos: usize,
}

impl SamplePump {
    pub(crate) fn read(&mut self, buf: &mut [Complex<f32>]) -> Result<usize, SdrError> {
        if self.pos >= self.pending.len() {
            self.pending = match self.rx.recv_timeout(Duration::from_secs(2)) {
                Ok(v) => v,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(SdrError::Device("no samples from device for 2 s".into()));
                }
                Err(RecvTimeoutError::Disconnected) => return Err(SdrError::EndOfStream),
            };
            self.pos = 0;
        }
        let n = buf.len().min(self.pending.len() - self.pos);
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}
