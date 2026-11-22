//! Scanner selection: which of several monitored channels to listen to.
//!
//! This is the decision logic of a hardware scanner applied to channels that
//! are all being received at once (a multichannel capture): the "scan" audio
//! stream follows whichever channel this controller selects.
//!
//! Rules, in order:
//! 1. A pinned (sticky) channel is always selected.
//! 2. Locked-out channels are never selected.
//! 3. While the selected channel is open it is kept, unless a channel with a
//!    higher priority opens and priority preemption is on.
//! 4. After the selected channel closes it stays selected for `hold_ms`, so a
//!    reply on the same frequency is not lost to another channel; a higher
//!    priority channel can still take over during the hold.
//! 5. Otherwise the highest-priority open channel wins, then the one that
//!    opened first.

use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct ScanConfig {
    /// Channels in priority order, highest first. Unlisted channels rank
    /// below every listed one, equal to each other.
    pub priority: Vec<u64>,
    /// How long to stay on a channel after it closes (ms).
    pub hold_ms: u64,
    /// Let a higher-priority channel interrupt an open lower-priority one.
    pub preempt: bool,
    /// Channels never selected.
    pub lockout: Vec<u64>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self { priority: Vec::new(), hold_ms: 2_000, preempt: true, lockout: Vec::new() }
    }
}

pub struct ScanController {
    cfg: ScanConfig,
    lockout: HashSet<u64>,
    pinned: Option<u64>,
    current: Option<u64>,
    /// When the current channel closed (ms), while in hold.
    closed_at: Option<u64>,
    /// Open channels and when they opened (ms).
    open: HashMap<u64, u64>,
}

impl ScanController {
    pub fn new(cfg: ScanConfig) -> Self {
        let lockout = cfg.lockout.iter().copied().collect();
        Self { cfg, lockout, pinned: None, current: None, closed_at: None, open: HashMap::new() }
    }

    fn rank(&self, f: u64) -> usize {
        self.cfg.priority.iter().position(|&p| p == f).unwrap_or(usize::MAX)
    }

    /// Pin the scanner to one channel (sticky mode); `None` resumes scanning.
    pub fn pin(&mut self, freq: Option<u64>) {
        self.pinned = freq;
    }

    pub fn pinned(&self) -> Option<u64> {
        self.pinned
    }

    pub fn set_lockout(&mut self, freq: u64, locked: bool) {
        if locked {
            self.lockout.insert(freq);
            if self.current == Some(freq) {
                self.current = None;
                self.closed_at = None;
            }
        } else {
            self.lockout.remove(&freq);
        }
    }

    pub fn on_open(&mut self, freq: u64, now_ms: u64) {
        self.open.entry(freq).or_insert(now_ms);
        if self.current == Some(freq) {
            self.closed_at = None;
        }
    }

    pub fn on_close(&mut self, freq: u64, now_ms: u64) {
        self.open.remove(&freq);
        if self.current == Some(freq) {
            self.closed_at = Some(now_ms);
        }
    }

    pub fn is_open(&self, freq: u64) -> bool {
        self.open.contains_key(&freq)
    }

    /// Best open, not-locked-out channel other than `except`.
    fn best_open(&self, except: Option<u64>) -> Option<u64> {
        self.open
            .iter()
            .filter(|(f, _)| Some(**f) != except && !self.lockout.contains(f))
            .min_by_key(|(f, opened)| (self.rank(**f), **opened, **f))
            .map(|(f, _)| *f)
    }

    /// Re-evaluate and return the selected channel (if any).
    pub fn select(&mut self, now_ms: u64) -> Option<u64> {
        if let Some(p) = self.pinned {
            return Some(p);
        }
        if let Some(c) = self.current {
            let challenger = self.best_open(Some(c)).filter(|&o| self.rank(o) < self.rank(c));
            if self.open.contains_key(&c) {
                if self.cfg.preempt {
                    if let Some(o) = challenger {
                        self.switch_to(o);
                    }
                }
                return self.current;
            }
            // Holding after close.
            let holding = self.closed_at.is_some_and(|t| now_ms.saturating_sub(t) < self.cfg.hold_ms);
            if holding {
                if let Some(o) = challenger {
                    self.switch_to(o);
                }
                return self.current;
            }
            self.current = None;
            self.closed_at = None;
        }
        if let Some(o) = self.best_open(None) {
            self.switch_to(o);
        }
        self.current
    }

    fn switch_to(&mut self, f: u64) {
        self.current = Some(f);
        self.closed_at = if self.open.contains_key(&f) { None } else { self.closed_at };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u64 = 118_300_000;
    const B: u64 = 119_100_000;
    const C: u64 = 121_500_000;

    fn ctl(priority: Vec<u64>) -> ScanController {
        ScanController::new(ScanConfig { priority, hold_ms: 1_000, preempt: true, lockout: vec![] })
    }

    #[test]
    fn first_open_wins_and_is_kept() {
        let mut s = ctl(vec![]);
        assert_eq!(s.select(0), None);
        s.on_open(A, 10);
        s.on_open(B, 20);
        assert_eq!(s.select(20), Some(A));
        assert_eq!(s.select(30), Some(A));
    }

    #[test]
    fn hold_keeps_channel_for_the_reply() {
        let mut s = ctl(vec![]);
        s.on_open(A, 0);
        assert_eq!(s.select(0), Some(A));
        s.on_open(B, 100);
        s.on_close(A, 200);
        // Within the hold, A stays selected even though B is talking.
        assert_eq!(s.select(500), Some(A));
        // Reply on A during hold.
        s.on_open(A, 600);
        assert_eq!(s.select(600), Some(A));
        s.on_close(A, 700);
        // Hold expires; B is still open and takes over.
        assert_eq!(s.select(1_800), Some(B));
    }

    #[test]
    fn priority_preempts() {
        let mut s = ctl(vec![C, A]);
        s.on_open(B, 0);
        assert_eq!(s.select(0), Some(B));
        s.on_open(A, 10);
        assert_eq!(s.select(10), Some(A));
        s.on_open(C, 20);
        assert_eq!(s.select(20), Some(C));
    }

    #[test]
    fn no_preempt_when_disabled() {
        let mut s = ScanController::new(ScanConfig { priority: vec![C], hold_ms: 0, preempt: false, lockout: vec![] });
        s.on_open(B, 0);
        assert_eq!(s.select(0), Some(B));
        s.on_open(C, 10);
        assert_eq!(s.select(10), Some(B));
        s.on_close(B, 20);
        assert_eq!(s.select(20), Some(C));
    }

    #[test]
    fn pin_and_lockout() {
        let mut s = ctl(vec![]);
        s.pin(Some(B));
        s.on_open(A, 0);
        assert_eq!(s.select(0), Some(B));
        s.pin(None);
        assert_eq!(s.select(1), Some(A));
        s.set_lockout(A, true);
        assert_eq!(s.select(2), None);
        s.on_open(B, 3);
        assert_eq!(s.select(3), Some(B));
    }

    #[test]
    fn empty_after_everything_closes() {
        let mut s = ctl(vec![]);
        s.on_open(A, 0);
        s.select(0);
        s.on_close(A, 10);
        assert_eq!(s.select(500), Some(A));
        assert_eq!(s.select(1_011), None);
    }
}
