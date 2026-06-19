//! AERO-4: P-channel superframe-lock + DCD/AFC-lock state machine.
//!
//! Inmarsat Classic Aero transmits the P(acket)-channel as a continuous
//! stream of 1200-bit frames, each carrying a 16-bit header
//! ([`crate::frame::FrameHeader`]) whose frame counters advance by one per
//! frame inside a constant superframe id. A receiver that is genuinely
//! tracking the channel sees that counter run monotonically; a receiver
//! that is mistracking (false UW trigger, lost carrier, wrong frequency)
//! sees the counter jump around. JAERO models this with a
//! `FreqOffsetEstimateSlot` state machine that couples superframe sync,
//! the data-carrier-detect (DCD) flag, and the automatic-frequency-control
//! (AFC) hold; this module is the xng equivalent at the frame-header layer
//! (PROVENANCE.md "No AFC of the channel center / DCD interplay" is the
//! divergence this closes for the framing layer).
//!
//! The machine is intentionally a pure function of the header sequence: it
//! takes one [`FrameHeader`] per frame and returns the current lock state.
//! That makes it verifiable against a *synthetic header-counter sequence*
//! oracle (see the tests here and in `frame.rs`) rather than an off-air
//! capture — the hard rules explicitly permit synthetic frame-counter
//! sequences for state-machine logic.

use crate::frame::FrameHeader;

/// Consecutive in-sequence headers required to acquire superframe lock.
pub const LOCK_ACQUIRE_N: u32 = 3;
/// Consecutive out-of-sequence headers required to drop superframe lock.
pub const LOCK_LOSE_M: u32 = 4;

/// DCD/AFC indicator derived from the lock state.
///
/// - `Searching`: no lock; AFC is free to re-acquire (coarse correction on).
/// - `Acquiring`: in-sequence headers are accumulating toward lock; the
///   carrier looks present (DCD asserted) but lock is not yet confirmed.
/// - `Locked`: superframe lock held; AFC is held (frequency error is being
///   tracked, not searched) and DCD is asserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarrierState {
    Searching,
    Acquiring,
    Locked,
}

impl CarrierState {
    pub fn as_str(self) -> &'static str {
        match self {
            CarrierState::Searching => "searching",
            CarrierState::Acquiring => "acquiring",
            CarrierState::Locked => "locked",
        }
    }
}

/// Snapshot of the lock machine after processing one frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockStatus {
    /// True once `N` consecutive in-sequence headers have been seen and the
    /// machine has not yet lost lock.
    pub superframe_lock: bool,
    /// DCD asserted: a carrier carrying coherent frame headers is present
    /// (true while acquiring or locked).
    pub dcd: bool,
    /// AFC held: the frequency estimate is being tracked rather than
    /// searched (true only while locked).
    pub afc_locked: bool,
    /// Carrier/AFC state for surfacing in message details.
    pub carrier_state: CarrierState,
    /// Consecutive in-sequence headers in the current run.
    pub match_count: u32,
    /// Consecutive out-of-sequence headers since the last in-sequence one.
    pub miss_count: u32,
}

/// Tracks the P-channel frame-counter sequence across consecutive frames to
/// declare and maintain superframe lock, with a coupled DCD/AFC indicator.
///
/// Lock is acquired after `acquire_n` consecutive in-sequence headers and
/// lost after `lose_m` consecutive out-of-sequence headers. "In sequence"
/// means: same superframe id and frame counters that advance by exactly one
/// (mod 16) from the previous frame. The two redundant header counters must
/// also agree with each other (a corrupted header that survived the UW hunt
/// is rejected here even if its delta happens to look right).
#[derive(Debug, Clone)]
pub struct SuperframeLockStateMachine {
    acquire_n: u32,
    lose_m: u32,
    prev: Option<FrameHeader>,
    match_count: u32,
    miss_count: u32,
    locked: bool,
}

impl SuperframeLockStateMachine {
    /// Build with the default thresholds ([`LOCK_ACQUIRE_N`] /
    /// [`LOCK_LOSE_M`]).
    pub fn new() -> Self {
        Self::with_thresholds(LOCK_ACQUIRE_N, LOCK_LOSE_M)
    }

    /// Build with explicit acquire/lose thresholds. `acquire_n` is clamped
    /// to at least 1 (a single match cannot be a "no-op acquire").
    pub fn with_thresholds(acquire_n: u32, lose_m: u32) -> Self {
        Self {
            acquire_n: acquire_n.max(1),
            lose_m: lose_m.max(1),
            prev: None,
            match_count: 0,
            miss_count: 0,
            locked: false,
        }
    }

    /// True if the header's two redundant frame counters agree. A corrupted
    /// header that slipped past the UW hunt with a mismatched redundant copy
    /// must not count toward (or against) lock.
    fn header_self_consistent(h: &FrameHeader) -> bool {
        h.frame_counter1 == h.frame_counter2
    }

    /// True if `cur` is the in-sequence successor of `prev`: same superframe
    /// (or the superframe stepping by one as the counter wraps) and the
    /// frame counter advancing by exactly one mod 16.
    fn is_in_sequence(prev: &FrameHeader, cur: &FrameHeader) -> bool {
        if prev.format_id != cur.format_id {
            return false;
        }
        let expected = (prev.frame_counter1 + 1) & 0xF;
        if cur.frame_counter1 != expected {
            return false;
        }
        // The counter wrapped 15 -> 0: the superframe id is allowed to step
        // by one (mod 16); otherwise the superframe id must be unchanged.
        if expected == 0 {
            cur.superframe == ((prev.superframe + 1) & 0xF) || cur.superframe == prev.superframe
        } else {
            cur.superframe == prev.superframe
        }
    }

    /// Feed one decoded frame header; returns the updated lock status.
    pub fn update(&mut self, header: FrameHeader) -> LockStatus {
        let consistent = Self::header_self_consistent(&header);
        let in_seq = consistent
            && self
                .prev
                .as_ref()
                .map(|p| Self::is_in_sequence(p, &header))
                .unwrap_or(false);

        if in_seq {
            self.match_count = self.match_count.saturating_add(1);
            self.miss_count = 0;
            if !self.locked && self.match_count >= self.acquire_n {
                self.locked = true;
            }
        } else {
            self.miss_count = self.miss_count.saturating_add(1);
            self.match_count = 0;
            if self.locked && self.miss_count >= self.lose_m {
                self.locked = false;
            }
        }

        // Track the previous header only when self-consistent: a corrupted
        // header must not become the baseline that the next frame is
        // compared against (otherwise one good frame after garbage could
        // never re-establish a sequence).
        if consistent {
            self.prev = Some(header);
        }

        self.status()
    }

    /// Current status without advancing the machine.
    pub fn status(&self) -> LockStatus {
        let carrier_state = if self.locked {
            CarrierState::Locked
        } else if self.match_count > 0 {
            CarrierState::Acquiring
        } else {
            CarrierState::Searching
        };
        LockStatus {
            superframe_lock: self.locked,
            dcd: carrier_state != CarrierState::Searching,
            afc_locked: self.locked,
            carrier_state,
            match_count: self.match_count,
            miss_count: self.miss_count,
        }
    }

    /// Surface the current lock state as message-details JSON (enrichment;
    /// no shared-type change).
    pub fn details_json(&self) -> serde_json::Value {
        let s = self.status();
        serde_json::json!({
            "superframe_lock": s.superframe_lock,
            "dcd": s.dcd,
            "afc_locked": s.afc_locked,
            "carrier_state": s.carrier_state.as_str(),
            "match_count": s.match_count,
            "miss_count": s.miss_count,
        })
    }
}

impl Default for SuperframeLockStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a P-channel header (format id 1) with a given superframe and
    /// frame counter, redundant counters in agreement.
    fn hdr(superframe: u8, counter: u8) -> FrameHeader {
        FrameHeader {
            format_id: 1,
            superframe: superframe & 0xF,
            frame_counter1: counter & 0xF,
            frame_counter2: counter & 0xF,
        }
    }

    /// ORACLE (1): a synthetic run of in-sequence headers acquires lock at
    /// exactly N matches, not before.
    #[test]
    fn acquires_lock_after_n_matches() {
        let mut sm = SuperframeLockStateMachine::with_thresholds(3, 4);
        // First header has no predecessor: it is a miss, machine searching.
        let s0 = sm.update(hdr(0, 0));
        assert!(!s0.superframe_lock);
        assert_eq!(s0.carrier_state, CarrierState::Searching);

        // Header 1: first in-sequence delta (0 -> 1). match_count = 1.
        let s1 = sm.update(hdr(0, 1));
        assert!(!s1.superframe_lock);
        assert_eq!(s1.match_count, 1);
        assert_eq!(s1.carrier_state, CarrierState::Acquiring);
        assert!(s1.dcd, "carrier detected while acquiring");
        assert!(!s1.afc_locked);

        // Header 2: match_count = 2, still below N=3.
        let s2 = sm.update(hdr(0, 2));
        assert!(!s2.superframe_lock);
        assert_eq!(s2.match_count, 2);

        // Header 3: match_count = 3 == N -> lock acquired.
        let s3 = sm.update(hdr(0, 3));
        assert!(s3.superframe_lock, "lock acquired at N consecutive matches");
        assert_eq!(s3.carrier_state, CarrierState::Locked);
        assert!(s3.dcd);
        assert!(s3.afc_locked, "AFC held once locked");
    }

    /// ORACLE (2): once locked, a synthetic run of out-of-sequence (jumping)
    /// headers drops lock at exactly M misses, not before.
    #[test]
    fn loses_lock_after_m_misses() {
        let mut sm = SuperframeLockStateMachine::with_thresholds(3, 4);
        // Drive to lock with an in-sequence span.
        for c in 0..=4u8 {
            sm.update(hdr(0, c));
        }
        assert!(sm.status().superframe_lock);

        // Now feed jumping counters (each a miss). Lock must hold until the
        // M-th consecutive miss.
        let jumps = [9u8, 2, 13]; // 3 misses < M=4: still locked
        for &c in &jumps {
            let s = sm.update(hdr(0, c));
            assert!(
                s.superframe_lock,
                "lock holds through {} misses (M=4)",
                s.miss_count
            );
        }
        // 4th consecutive miss: lock lost, AFC released, but DCD may still be
        // searching/acquiring depending on count — here it is searching.
        let s = sm.update(hdr(0, 7));
        assert!(!s.superframe_lock, "lock lost at M consecutive misses");
        assert!(!s.afc_locked, "AFC released on loss of lock");
        assert_eq!(s.carrier_state, CarrierState::Searching);
    }

    /// A self-inconsistent header (redundant counters disagree) is a miss
    /// even if its primary delta looks correct — a corrupted header that
    /// slipped past the UW hunt must not count toward lock.
    #[test]
    fn inconsistent_header_is_a_miss() {
        let mut sm = SuperframeLockStateMachine::with_thresholds(3, 4);
        sm.update(hdr(0, 0));
        sm.update(hdr(0, 1));
        // counter would be 2 (in sequence) but redundant copy is garbage.
        let bad =
            FrameHeader { format_id: 1, superframe: 0, frame_counter1: 2, frame_counter2: 9 };
        let s = sm.update(bad);
        assert_eq!(s.match_count, 0, "inconsistent header resets the run");
        assert!(!s.superframe_lock);
    }

    /// The counter wrapping 15 -> 0 within a constant superframe stays in
    /// sequence (no spurious lock loss at the wrap).
    #[test]
    fn counter_wrap_stays_in_sequence() {
        let mut sm = SuperframeLockStateMachine::with_thresholds(2, 4);
        sm.update(hdr(0, 14));
        let s1 = sm.update(hdr(0, 15));
        assert_eq!(s1.match_count, 1);
        // 15 -> 0 wrap; superframe may stay or step, here it stays.
        let s2 = sm.update(hdr(0, 0));
        assert_eq!(s2.match_count, 2);
        assert!(s2.superframe_lock, "wrap does not break the sequence");
    }

    /// Re-acquisition: after losing lock, a fresh in-sequence span re-locks.
    #[test]
    fn reacquires_after_loss() {
        let mut sm = SuperframeLockStateMachine::with_thresholds(3, 4);
        for c in 0..=4u8 {
            sm.update(hdr(0, c));
        }
        assert!(sm.status().superframe_lock);
        // Lose it with M misses.
        for &c in &[9u8, 2, 13, 7] {
            sm.update(hdr(0, c));
        }
        assert!(!sm.status().superframe_lock);
        // Feed a clean in-sequence span from the last (consistent) header
        // baseline. The header 7 above set prev=7, so continue 8,9,10,11.
        let mut locked_again = false;
        for c in 8..=11u8 {
            locked_again = sm.update(hdr(0, c)).superframe_lock;
        }
        assert!(locked_again, "machine re-locks on a fresh in-sequence span");
    }

    #[test]
    fn details_json_shape() {
        let mut sm = SuperframeLockStateMachine::new();
        for c in 0..=4u8 {
            sm.update(hdr(0, c));
        }
        let v = sm.details_json();
        assert_eq!(v["superframe_lock"], true);
        assert_eq!(v["carrier_state"], "locked");
        assert_eq!(v["afc_locked"], true);
        assert_eq!(v["dcd"], true);
    }
}
