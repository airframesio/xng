//! Share one [`IqSource`] between several consumers.
//!
//! A USB SDR can be opened by only one process (and one handle), so two decode
//! sessions on the same device — ACARS and VDL2 off one wideband capture, say —
//! cannot each open it. [`tee`] opens nothing itself: it takes the one open
//! source, runs a reader thread that pulls fixed-size blocks from it, and hands
//! every consumer the same `Arc`'d block (no per-consumer copy of the IQ).
//! Each consumer is itself an [`IqSource`], so decode loops are unchanged and
//! each session still runs on its own thread.
//!
//! When a consumer falls behind, [`Overflow`] decides what happens: live
//! hardware must never stall (that just moves the overflow into the device and
//! starves the *other* sessions), so `Drop` discards the block for that consumer
//! only and counts it; file replay has no deadline, so `Block` applies
//! backpressure and every consumer sees every sample.

use crate::{IqSource, SdrError};
use num_complex::Complex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Duration;

/// Samples per block read from the shared source.
const BLOCK: usize = 65_536;
/// Blocks queued per consumer: ~0.4 s at 10 MS/s, ~2.7 s at 2.4 MS/s.
const DEPTH: usize = 64;
/// Consecutive read errors after which the shared source is given up on
/// (matches the decode loop's own budget for a directly-owned source).
const MAX_CONSECUTIVE_ERRORS: u32 = 10;

/// What to do when a consumer's queue is full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overflow {
    /// Discard the block for the lagging consumer only (live hardware).
    Drop,
    /// Wait for the lagging consumer (file replay).
    Block,
}

type Block = Arc<[Complex<f32>]>;
/// What the reader thread sends each consumer: a block, or why it stopped.
type Message = Result<Block, String>;
/// A consumer's queue and its count of blocks dropped for falling behind.
type Slot = Option<(SyncSender<Message>, Arc<AtomicU64>)>;

/// Split `source` into `n` consumers that each see the same sample stream.
///
/// The reader thread stops when the source ends, fails persistently, or every
/// consumer has been dropped; the source (and so the device) is dropped with
/// it.
pub fn tee(mut source: Box<dyn IqSource>, n: usize, overflow: Overflow) -> Vec<TeeSource> {
    assert!(n > 0, "tee needs at least one consumer");
    let sample_rate = source.sample_rate();
    let center_freq_hz = source.center_freq_hz();
    let mut txs: Vec<Slot> = Vec::new();
    let mut outs = Vec::new();
    for _ in 0..n {
        let (tx, rx) = sync_channel(DEPTH);
        let dropped = Arc::new(AtomicU64::new(0));
        txs.push(Some((tx, dropped.clone())));
        outs.push(TeeSource {
            rx,
            pending: None,
            pos: 0,
            sample_rate,
            center_freq_hz,
            dropped,
            reported: 0,
            wait: overflow == Overflow::Block,
        });
    }

    std::thread::Builder::new()
        .name("iq-tee".into())
        .spawn(move || {
            let mut buf = vec![Complex::new(0.0f32, 0.0f32); BLOCK];
            let mut consecutive_errors = 0u32;
            loop {
                // A consumer that was dropped leaves this thread the only
                // holder of its loss counter. Checked every pass, so the
                // source is released even while reads come back empty.
                for slot in txs.iter_mut() {
                    if slot.as_ref().is_some_and(|(_, d)| Arc::strong_count(d) == 1) {
                        *slot = None;
                    }
                }
                if txs.iter().all(Option::is_none) {
                    break;
                }
                let msg = match source.read(&mut buf) {
                    Ok(0) => continue,
                    Ok(n) => {
                        consecutive_errors = 0;
                        Ok(Block::from(&buf[..n]))
                    }
                    Err(SdrError::EndOfStream) => break,
                    Err(e) => {
                        consecutive_errors += 1;
                        if consecutive_errors < MAX_CONSECUTIVE_ERRORS {
                            continue;
                        }
                        Err(format!(
                            "giving up after {consecutive_errors} consecutive read errors: {e}"
                        ))
                    }
                };
                let fatal = msg.is_err();
                for slot in txs.iter_mut() {
                    let Some((tx, dropped)) = slot else { continue };
                    let gone = match overflow {
                        Overflow::Drop => match tx.try_send(msg.clone()) {
                            Ok(()) => false,
                            Err(TrySendError::Full(_)) => {
                                dropped.fetch_add(1, Ordering::Relaxed);
                                false
                            }
                            Err(TrySendError::Disconnected(_)) => true,
                        },
                        Overflow::Block => tx.send(msg.clone()).is_err(),
                    };
                    if gone {
                        *slot = None;
                    }
                }
                if fatal {
                    break;
                }
            }
            // Dropping the senders ends every consumer with EndOfStream;
            // dropping `source` here stops and releases the device.
        })
        .expect("spawn iq-tee thread");

    outs
}

/// One consumer of a [`tee`]d source.
pub struct TeeSource {
    rx: Receiver<Message>,
    pending: Option<Block>,
    pos: usize,
    sample_rate: f64,
    center_freq_hz: u64,
    dropped: Arc<AtomicU64>,
    reported: u64,
    /// `Overflow::Block`: the reader may be waiting on a slower sibling, so
    /// a quiet spell is not a stalled source.
    wait: bool,
}

impl TeeSource {
    /// Blocks this consumer has lost by falling behind (`Overflow::Drop`).
    pub fn dropped_blocks(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl IqSource for TeeSource {
    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn center_freq_hz(&self) -> u64 {
        self.center_freq_hz
    }

    fn read(&mut self, buf: &mut [Complex<f32>]) -> Result<usize, SdrError> {
        if self.pending.as_ref().is_none_or(|b| self.pos >= b.len()) {
            let block = match self.rx.recv_timeout(Duration::from_secs(2)) {
                Ok(Ok(b)) => b,
                Ok(Err(e)) => return Err(SdrError::Device(e)),
                Err(RecvTimeoutError::Timeout) if self.wait => return Ok(0),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(SdrError::Device(
                        "no samples from shared source for 2 s".into(),
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => return Err(SdrError::EndOfStream),
            };
            // Surface new losses once per block boundary; silent when healthy.
            let dropped = self.dropped_blocks();
            if dropped > self.reported {
                eprintln!(
                    "shared SDR WARNING: a session fell behind and lost {} block(s) of {BLOCK} samples ({} total)",
                    dropped - self.reported,
                    dropped
                );
                self.reported = dropped;
            }
            self.pending = Some(block);
            self.pos = 0;
        }
        let block = self.pending.as_ref().expect("filled above");
        let n = buf.len().min(block.len() - self.pos);
        buf[..n].copy_from_slice(&block[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A finite ramp source: sample k has re = k.
    struct Ramp {
        next: usize,
        len: usize,
    }

    impl IqSource for Ramp {
        fn sample_rate(&self) -> f64 {
            1_000_000.0
        }
        fn center_freq_hz(&self) -> u64 {
            100_000_000
        }
        fn read(&mut self, buf: &mut [Complex<f32>]) -> Result<usize, SdrError> {
            if self.next >= self.len {
                return Err(SdrError::EndOfStream);
            }
            let n = buf.len().min(self.len - self.next).min(1000);
            for (i, s) in buf[..n].iter_mut().enumerate() {
                *s = Complex::new((self.next + i) as f32, 0.0);
            }
            self.next += n;
            Ok(n)
        }
    }

    fn drain(mut s: TeeSource) -> Vec<f32> {
        let mut out = Vec::new();
        let mut buf = vec![Complex::new(0.0, 0.0); 777];
        loop {
            match s.read(&mut buf) {
                Ok(n) => out.extend(buf[..n].iter().map(|c| c.re)),
                Err(SdrError::EndOfStream) => return out,
                Err(e) => panic!("{e}"),
            }
        }
    }

    #[test]
    fn every_consumer_sees_every_sample_with_block() {
        let len = 250_000;
        let outs = tee(Box::new(Ramp { next: 0, len }), 3, Overflow::Block);
        assert!(outs.iter().all(|s| s.sample_rate() == 1_000_000.0));
        assert!(outs.iter().all(|s| s.center_freq_hz() == 100_000_000));
        let handles: Vec<_> = outs
            .into_iter()
            .map(|s| std::thread::spawn(move || drain(s)))
            .collect();
        let want: Vec<f32> = (0..len).map(|k| k as f32).collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), want);
        }
    }

    #[test]
    fn a_dropped_consumer_does_not_stall_the_others() {
        let len = 200_000;
        let mut outs = tee(Box::new(Ramp { next: 0, len }), 2, Overflow::Block);
        drop(outs.pop());
        let got = drain(outs.pop().unwrap());
        assert_eq!(got.len(), len);
    }

    #[test]
    fn drop_mode_never_blocks_the_reader_on_a_stalled_consumer() {
        // One consumer never reads; the other must still get the whole
        // stream promptly and the stalled one must account its losses.
        let len = BLOCK * (DEPTH + 20);
        let mut outs = tee(Box::new(Ramp { next: 0, len }), 2, Overflow::Drop);
        let stalled = outs.pop().unwrap();
        let reader = outs.pop().unwrap();
        let got = drain(reader);
        assert!(got.len() <= len);
        assert!(stalled.dropped_blocks() > 0);
    }

    /// A source that never has samples, and says when it is released.
    struct Empty(Arc<std::sync::atomic::AtomicBool>);

    impl IqSource for Empty {
        fn sample_rate(&self) -> f64 {
            1_000_000.0
        }
        fn center_freq_hz(&self) -> u64 {
            100_000_000
        }
        fn read(&mut self, _: &mut [Complex<f32>]) -> Result<usize, SdrError> {
            std::thread::sleep(Duration::from_millis(1));
            Ok(0)
        }
    }

    impl Drop for Empty {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    /// Once every consumer is gone the source is released, even when its
    /// reads only ever come back empty.
    #[test]
    fn source_is_released_when_every_consumer_is_gone() {
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        drop(tee(Box::new(Empty(released.clone())), 2, Overflow::Drop));
        let start = std::time::Instant::now();
        while !released.load(Ordering::Relaxed) {
            assert!(start.elapsed() < Duration::from_secs(5), "source never released");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// With `Block`, a consumer waiting on a slower sibling keeps waiting
    /// instead of reporting a stalled source, and still gets every sample.
    #[test]
    fn block_mode_waits_out_a_slow_sibling() {
        let len = (DEPTH + 8) * BLOCK;
        let mut outs = tee(Box::new(Ramp { next: 0, len }), 2, Overflow::Block);
        let slow = outs.pop().unwrap();
        let fast = std::thread::spawn({
            let s = outs.pop().unwrap();
            move || drain(s)
        });
        std::thread::sleep(Duration::from_secs(3));
        let slow = std::thread::spawn(move || drain(slow));
        let want: Vec<f32> = (0..len).map(|k| k as f32).collect();
        assert_eq!(fast.join().unwrap(), want);
        assert_eq!(slow.join().unwrap(), want);
    }
}
