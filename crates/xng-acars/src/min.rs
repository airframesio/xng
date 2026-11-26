//! ACARS downlink Message Identifier Number (MIN) handling, ported from
//! MIT-licensed libacars (`acars.c`; see PROVENANCE.md).
//!
//! A downlink block's text field begins with a 4-character MIN followed by
//! a 6-character flight id (libacars `acars.c`: `memcpy(msg->msg_num, ptr,
//! 3); msg->msg_num[3]='\0'; msg->msg_num_seq = ptr[3]`). libacars (and
//! acarsdec) surface the MIN as TWO fields — the 3-character message number
//! (`msg_num`) and the 4th character (`msg_num_seq`), the per-message
//! sequence character — and key reassembly on the sequence value
//! `msg_num_seq - 'A'` (`acars.c`: `.seq_num = down ? msg->msg_num_seq -
//! 'A' : ...`, with `.seq_num_first = 0`).
//!
//! xng's [`xng_types::AcarsCore`] carries the combined 4-character MIN in
//! its `msg_num` field; this module is the crate-local, libacars-faithful
//! place that splits it into the raw 3-character number plus the sequence
//! character and computes the downlink sequence index, including the 4th-
//! character edge cases.

use serde::Serialize;

/// libacars `IS_DOWNLINK_BLK(bid)`: a block id in `'0'..='9'` marks a
/// downlink; `'A'..='Z'` marks an uplink (`acars.c` macro).
pub fn is_downlink_block(block_id: char) -> bool {
    block_id.is_ascii_digit()
}

/// The split downlink MIN: the raw 3-character message number, the 4th
/// (sequence) character, and the zero-based sequence index libacars uses
/// for reassembly (`msg_num_seq - 'A'`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DownlinkMin {
    /// 3-character message number (libacars `msg_num`; acarsdec splits the
    /// `msgno` here too).
    pub msg_num: String,
    /// 4th character — the per-message sequence character (libacars
    /// `msg_num_seq`). The first block of a downlink message is `'A'`.
    pub msg_num_seq: char,
    /// Zero-based reassembly sequence index `msg_num_seq - 'A'` when the
    /// sequence character is a letter `'A'..='Z'`; `None` for the 4th-char
    /// edge cases where it is not (libacars would feed a negative/garbage
    /// `seq_num` that the reassembler rejects).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u8>,
}

/// Split a raw downlink MIN (the 4 characters at the start of a downlink
/// block's text, as carried in [`xng_types::AcarsCore::msg_num`]) into its
/// libacars components.
///
/// Returns `None` when the value is not at least a 4-character MIN
/// (libacars requires the downlink text field to be at least 10 bytes:
/// 4 MIN + 6 flight id — `acars.c`: `if(remaining < 10) goto fail`).
///
/// 4th-character downlink-rule edge cases handled here:
/// - the sequence character is the 4th byte regardless of its value;
/// - only `'A'..='Z'` yields a reassembly index (`seq`); other bytes
///   (digits, punctuation, the `'.'` libacars substitutes for embedded
///   NULs) leave `seq = None` rather than producing a bogus index.
pub fn split_downlink(raw_min: &str) -> Option<DownlinkMin> {
    let bytes = raw_min.as_bytes();
    if bytes.len() < 4 {
        return None;
    }
    let msg_num: String = raw_min.chars().take(3).collect();
    let msg_num_seq = bytes[3] as char;
    let seq = if msg_num_seq.is_ascii_uppercase() {
        Some(msg_num_seq as u8 - b'A')
    } else {
        None
    };
    Some(DownlinkMin { msg_num, msg_num_seq, seq })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_id_class_matches_libacars_macro() {
        // libacars IS_DOWNLINK_BLK: digits are downlink, letters uplink.
        for c in '0'..='9' {
            assert!(is_downlink_block(c), "{c} should be downlink");
        }
        for c in 'A'..='Z' {
            assert!(!is_downlink_block(c), "{c} should be uplink");
        }
    }

    #[test]
    fn splits_min_like_libacars() {
        // libacars acars.c: msg_num = first 3 chars, msg_num_seq = 4th
        // char; the first block of a message uses 'A' (seq 0).
        let m = split_downlink("M01A").unwrap();
        assert_eq!(m.msg_num, "M01");
        assert_eq!(m.msg_num_seq, 'A');
        assert_eq!(m.seq, Some(0));

        // A later fragment: 4th char 'C' -> seq 2.
        let m = split_downlink("M07C").unwrap();
        assert_eq!(m.msg_num, "M07");
        assert_eq!(m.msg_num_seq, 'C');
        assert_eq!(m.seq, Some(2));
    }

    #[test]
    fn fourth_char_edge_cases() {
        // A non-letter 4th char (here a digit) is preserved as the
        // sequence character but yields no reassembly index — libacars
        // would compute a negative/garbage seq_num that the reassembler
        // discards, so we surface None rather than a bogus value.
        let m = split_downlink("D5R2").unwrap();
        assert_eq!(m.msg_num, "D5R");
        assert_eq!(m.msg_num_seq, '2');
        assert_eq!(m.seq, None);

        // libacars replaces embedded NULs in the text with '.', so a '.'
        // can appear as the 4th char; it is not a valid sequence letter.
        let m = split_downlink("S01.").unwrap();
        assert_eq!(m.msg_num, "S01");
        assert_eq!(m.msg_num_seq, '.');
        assert_eq!(m.seq, None);
    }

    #[test]
    fn rejects_short_min() {
        assert!(split_downlink("M0").is_none());
        assert!(split_downlink("").is_none());
    }

    #[test]
    fn seq_indices_span_the_alphabet() {
        // The downlink sequence character runs A..Z -> 0..25, matching the
        // libacars `msg_num_seq - 'A'` reassembly index.
        assert_eq!(split_downlink("AAAA").unwrap().seq, Some(0));
        assert_eq!(split_downlink("AAAZ").unwrap().seq, Some(25));
    }
}
