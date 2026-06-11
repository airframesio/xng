//! Multi-block ACARS reassembly, ported from MIT-licensed libacars
//! (reassembly.c + acars.c keying; see PROVENANCE.md).
//!
//! Long ACARS messages span blocks: each block carries a slice of the
//! text, intermediate blocks end with ETB (`more_to_come`), the final
//! with ETX. Downlinks sequence via the 4th character of the message
//! number (A, B, C ...) under a constant 3-char message id; uplinks
//! sequence via the block id itself (cycling A..W). Fragments are keyed
//! by (tail, label, msg_num) and joined when the final block arrives
//! with a contiguous sequence.

use std::collections::HashMap;
use xng_types::AcarsCore;

/// Outcome of offering one block to the reassembler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reasm {
    /// Single-block message (or uplink ACK quirk): nothing to do.
    Skipped,
    /// Fragment stored; message not yet complete.
    InProgress,
    /// This (sequence number, payload) was already seen.
    Duplicate,
    /// Final block arrived: the returned string is the full text.
    Complete(String),
    /// Final block arrived but the sequence has holes.
    Incomplete,
}

#[derive(Hash, PartialEq, Eq, Clone)]
struct Key {
    tail: String,
    label: String,
    /// 3-char message number (downlinks); empty for uplinks.
    msg_num: String,
}

struct Entry {
    frags: HashMap<i32, String>,
    deadline: f64,
}

/// Uplink block ids cycle A..W ('X'..'Z' are reserved for ACKs).
const UPLINK_SEQ_WRAP: i32 = ('X' as i32) - ('A' as i32);

pub struct Reassembler {
    entries: HashMap<Key, Entry>,
    timeout_secs: f64,
}

impl Reassembler {
    /// `timeout_secs`: fragment lifetime — libacars uses ~120 s on VHF
    /// and several minutes on satcom/HF bearers.
    pub fn new(timeout_secs: f64) -> Self {
        Self { entries: HashMap::new(), timeout_secs }
    }

    /// Offer a parsed block. `now_secs` is any monotonic seconds source.
    pub fn push(&mut self, core: &AcarsCore, now_secs: f64) -> Reasm {
        self.entries.retain(|_, e| e.deadline > now_secs);

        let Some(block_id) = core.block_id else { return Reasm::Skipped };
        let downlink = ('0'..='9').contains(&block_id);
        let (seq, msg_num, wrap) = if downlink {
            // Downlink message number "M01A": 3-char id + sequence char.
            let Some(num) = core.msg_num.as_deref() else { return Reasm::Skipped };
            if num.len() < 4 {
                return Reasm::Skipped;
            }
            let seq = num.as_bytes()[3] as i32 - 'A' as i32;
            (seq, num[..3].to_owned(), i32::MAX)
        } else {
            // Empty-text uplink ACKs use out-of-sequence block ids (X,Y,Z).
            if core.text.is_empty() {
                return Reasm::Skipped;
            }
            (block_id as i32 - 'A' as i32, String::new(), UPLINK_SEQ_WRAP)
        };
        if seq < 0 {
            return Reasm::Skipped;
        }
        let final_block = !core.more_to_come;

        let key = Key {
            tail: core.tail.clone().unwrap_or_default(),
            label: core.label.clone(),
            msg_num,
        };
        // The common case: a single-block message with no pending state.
        if final_block && !self.entries.contains_key(&key) && (seq == 0 || !downlink) {
            return Reasm::Skipped;
        }

        let entry = self.entries.entry(key.clone()).or_insert_with(|| Entry {
            frags: HashMap::new(),
            deadline: now_secs + self.timeout_secs,
        });
        if entry.frags.contains_key(&seq) {
            return Reasm::Duplicate;
        }
        entry.frags.insert(seq, core.text.clone());

        if !final_block {
            return Reasm::InProgress;
        }
        let entry = self.entries.remove(&key).expect("just inserted");
        // Join in sequence order; require contiguity from the first seen
        // sequence number (downlinks start at 0; uplink block ids may
        // wrap, handled by sorting modulo the wrap point).
        let mut seqs: Vec<i32> = entry.frags.keys().copied().collect();
        seqs.sort_unstable();
        let contiguous = if downlink {
            seqs[0] == 0 && seqs.windows(2).all(|w| w[1] == w[0] + 1)
        } else {
            // Uplinks: contiguous modulo wrap.
            seqs.windows(2).all(|w| w[1] == w[0] + 1 || (w[0] == wrap - 1 && w[1] == 0))
        };
        if !contiguous {
            return Reasm::Incomplete;
        }
        Some(())
            .map(|_| seqs.iter().map(|s| entry.frags[s].as_str()).collect::<String>())
            .map(Reasm::Complete)
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core(tail: &str, label: &str, block_id: char, msg_num: Option<&str>, text: &str, more: bool) -> AcarsCore {
        AcarsCore {
            mode: '2',
            tail: Some(tail.into()),
            label: label.into(),
            sublabel: None,
            mfi: None,
            block_id: Some(block_id),
            ack: None,
            flight: Some("UA0001".into()),
            msg_num: msg_num.map(|s| s.into()),
            text: text.into(),
            more_to_come: more,
            reassembled: false,
            app: None,
        }
    }

    #[test]
    fn single_block_is_skipped() {
        let mut r = Reassembler::new(120.0);
        let c = core("N12345", "H1", '2', Some("M01A"), "HELLO", false);
        assert_eq!(r.push(&c, 0.0), Reasm::Skipped);
    }

    #[test]
    fn downlink_two_blocks_reassemble() {
        let mut r = Reassembler::new(120.0);
        let a = core("N12345", "H1", '2', Some("M01A"), "FIRST-", true);
        let b = core("N12345", "H1", '3', Some("M01B"), "SECOND", false);
        assert_eq!(r.push(&a, 0.0), Reasm::InProgress);
        assert_eq!(r.push(&b, 5.0), Reasm::Complete("FIRST-SECOND".into()));
    }

    #[test]
    fn uplink_blocks_key_on_block_id() {
        let mut r = Reassembler::new(120.0);
        let a = core("N12345", "H1", 'A', None, "/O2.OHMAabcd", true);
        let b = core("N12345", "H1", 'B', None, "efgh", false);
        assert_eq!(r.push(&a, 0.0), Reasm::InProgress);
        assert_eq!(r.push(&b, 1.0), Reasm::Complete("/O2.OHMAabcdefgh".into()));
    }

    #[test]
    fn duplicate_fragment_detected() {
        let mut r = Reassembler::new(120.0);
        let a = core("N12345", "H1", '2', Some("M01A"), "X", true);
        assert_eq!(r.push(&a, 0.0), Reasm::InProgress);
        assert_eq!(r.push(&a, 1.0), Reasm::Duplicate);
    }

    #[test]
    fn timeout_drops_stale_fragments() {
        let mut r = Reassembler::new(120.0);
        let a = core("N12345", "H1", '2', Some("M01A"), "FIRST-", true);
        let b = core("N12345", "H1", '3', Some("M01B"), "SECOND", false);
        assert_eq!(r.push(&a, 0.0), Reasm::InProgress);
        // Final block arrives after the timeout: first fragment is gone,
        // sequence starts at B → holes → incomplete.
        assert_eq!(r.push(&b, 200.0), Reasm::Incomplete);
    }

    #[test]
    fn interleaved_aircraft_do_not_mix() {
        let mut r = Reassembler::new(120.0);
        let a1 = core("N11111", "H1", '2', Some("M01A"), "AAA", true);
        let b1 = core("N22222", "H1", '2', Some("M55A"), "BBB", true);
        let a2 = core("N11111", "H1", '3', Some("M01B"), "aaa", false);
        let b2 = core("N22222", "H1", '3', Some("M55B"), "bbb", false);
        r.push(&a1, 0.0);
        r.push(&b1, 0.0);
        assert_eq!(r.push(&a2, 1.0), Reasm::Complete("AAAaaa".into()));
        assert_eq!(r.push(&b2, 1.0), Reasm::Complete("BBBbbb".into()));
    }
}
