//! NAVTEX message framing: phasing → `ZCZC B1B2B3B4` header → text body →
//! `NNNN` end, emitted as structured JSON.
//!
//! Frame structure (IMO NAVTEX Manual, MSC.1/Circ.1403; mirrored by
//! fldigi `ccir_message`):
//!
//! ```text
//!   ZCZC B1 B2 B3 B4 <CR><LF> ...message text... <CR><LF> NNNN
//! ```
//!
//! - `ZCZC` then a space is the start-of-message phasing/header marker.
//! - `B1`   station identifier (single letter, the transmitter).
//! - `B2`   subject indicator (single letter, message category A–Z).
//! - `B3B4` two-digit message serial number (`00` = never suppressed).
//! - `NNNN` marks end of message.
//!
//! The subject-indicator letter → category mapping is the IMO table as
//! transcribed in fldigi `ccir_message::msg_type`.

use serde::{Deserialize, Serialize};

/// IMO subject-indicator categories (B2 character), per the IMO NAVTEX
/// Manual; text matches fldigi `ccir_message::msg_type`.
pub fn subject_category(b2: char) -> &'static str {
    match b2.to_ascii_uppercase() {
        'A' => "Navigational warning",
        'B' => "Meteorological warning",
        'C' => "Ice report",
        'D' => "Search & rescue information, pirate warnings",
        'E' => "Meteorological forecast",
        'F' => "Pilot service message",
        'G' => "AIS message",
        'H' => "LORAN message",
        'I' => "Not used",
        'J' => "SATNAV messages",
        'K' => "Other electronic navaid messages",
        'L' => "Navigational warnings (additional)",
        'T' => "Test transmissions (UK only)",
        'V' => "Notice to fishermen (U.S. only)",
        'W' => "Environmental (U.S. only)",
        'X' => "Special services - allocation by IMO NAVTEX Panel",
        'Y' => "Special services - allocation by IMO NAVTEX Panel",
        'Z' => "No message on hand",
        _ => "Unknown / invalid subject",
    }
}

/// A decoded NAVTEX message ready to emit as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavtexMessage {
    /// Station identifier (B1) — the transmitting station letter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub station: Option<char>,
    /// Subject-indicator letter (B2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<char>,
    /// Human-readable subject category for `subject`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_category: Option<String>,
    /// Message serial number (B3B4), 0..=99.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_number: Option<u8>,
    /// Message text body (between the header CR/LF and `NNNN`), trimmed.
    pub text: String,
    /// True when a complete `ZCZC ...` header was parsed.
    pub header_ok: bool,
    /// True when the `NNNN` end-of-message marker was seen.
    pub end_ok: bool,
}

/// Parse a fully decoded NAVTEX character stream into a structured message.
///
/// `stream` is the recovered text (after FEC-B), still containing the
/// `ZCZC ...` header and trailing `NNNN`. Parsing is tolerant: if the
/// header is malformed the whole stream is returned as `text` with
/// `header_ok = false`; a missing `NNNN` sets `end_ok = false`.
pub fn parse(stream: &str) -> NavtexMessage {
    let mut msg = NavtexMessage {
        station: None,
        subject: None,
        subject_category: None,
        message_number: None,
        text: String::new(),
        header_ok: false,
        end_ok: false,
    };

    // Locate the header: "ZCZC" + space + B1 B2 B3 B4, where B1/B2 are
    // alphanumeric and B3 B4 are digits (fldigi `detect_header`).
    let body_start = if let Some(hpos) = stream.find("ZCZC") {
        let after = &stream[hpos + 4..];
        let chars: Vec<char> = after.chars().collect();
        if chars.len() >= 6
            && chars[0] == ' '
            && chars[1].is_ascii_alphanumeric()
            && chars[2].is_ascii_alphanumeric()
            && chars[3].is_ascii_digit()
            && chars[4].is_ascii_digit()
        {
            msg.station = Some(chars[1]);
            msg.subject = Some(chars[2]);
            msg.subject_category = Some(subject_category(chars[2]).to_string());
            let num = (chars[3] as u8 - b'0') * 10 + (chars[4] as u8 - b'0');
            msg.message_number = Some(num);
            msg.header_ok = true;
            // Body begins after the 6 header chars (space+B1B2B3B4); skip a
            // following CR/LF separator if present.
            let mut idx = hpos + 4 + char_byte_len(after, 5);
            idx += skip_leading_breaks(&stream[idx..]);
            idx
        } else {
            0
        }
    } else {
        0
    };

    let mut body = &stream[body_start..];

    // Strip the NNNN end marker if present (fldigi `detect_end`).
    if let Some(npos) = body.find("NNNN") {
        msg.end_ok = true;
        body = &body[..npos];
    }

    msg.text = normalize_text(body);
    msg
}

/// Byte length of the first `n` chars of `s` (header is ASCII, but be
/// UTF-8 safe).
fn char_byte_len(s: &str, n: usize) -> usize {
    s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len())
}

/// Count leading CR/LF/space bytes (the header/body separator).
fn skip_leading_breaks(s: &str) -> usize {
    let mut n = 0;
    for b in s.bytes() {
        if b == b'\r' || b == b'\n' || b == b' ' {
            n += 1;
        } else {
            break;
        }
    }
    n
}

/// Collapse runs of whitespace/line-breaks into single spaces/newlines and
/// trim, mirroring fldigi `ccir_message::cleanup` (a CR/LF run becomes one
/// newline; a space run becomes one space).
fn normalize_text(s: &str) -> String {
    let mut out = String::new();
    let mut pending_break = false;
    let mut pending_space = false;
    let mut seen = false;
    for c in s.chars() {
        match c {
            '\r' | '\n' => pending_break = true,
            ' ' | '\t' => pending_space = true,
            _ => {
                if seen {
                    if pending_break {
                        out.push('\n');
                    } else if pending_space {
                        out.push(' ');
                    }
                }
                pending_break = false;
                pending_space = false;
                seen = true;
                out.push(c);
            }
        }
    }
    out
}

impl NavtexMessage {
    /// Serialize to compact JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("NavtexMessage serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_header_and_end() {
        // Spec-shaped frame: station E, subject A (nav warning), number 42.
        let stream = "ZCZC EA42\r\nTEST NAVAREA WARNING 123\r\nNNNN";
        let m = parse(stream);
        assert!(m.header_ok);
        assert!(m.end_ok);
        assert_eq!(m.station, Some('E'));
        assert_eq!(m.subject, Some('A'));
        assert_eq!(m.subject_category.as_deref(), Some("Navigational warning"));
        assert_eq!(m.message_number, Some(42));
        assert_eq!(m.text, "TEST NAVAREA WARNING 123");
    }

    #[test]
    fn subject_categories_match_imo_table() {
        assert_eq!(subject_category('A'), "Navigational warning");
        assert_eq!(subject_category('B'), "Meteorological warning");
        assert_eq!(subject_category('E'), "Meteorological forecast");
        assert_eq!(subject_category('D'), "Search & rescue information, pirate warnings");
        assert_eq!(subject_category('Z'), "No message on hand");
    }

    #[test]
    fn header_with_leading_phasing_garbage() {
        // Real receivers see phasing chars before the header.
        let stream = "***ZCZC FA07\r\nGALE WARNING\r\nNNNN";
        let m = parse(stream);
        assert!(m.header_ok);
        assert_eq!(m.station, Some('F'));
        assert_eq!(m.subject, Some('A'));
        assert_eq!(m.message_number, Some(7));
        assert_eq!(m.text, "GALE WARNING");
    }

    #[test]
    fn missing_end_marker() {
        let stream = "ZCZC GB05\r\nINCOMPLETE";
        let m = parse(stream);
        assert!(m.header_ok);
        assert!(!m.end_ok);
        assert_eq!(m.text, "INCOMPLETE");
    }

    #[test]
    fn no_header_returns_raw_text() {
        let m = parse("RANDOM JUNK NNNN");
        assert!(!m.header_ok);
        assert!(m.end_ok);
        assert_eq!(m.text, "RANDOM JUNK");
    }

    #[test]
    fn json_round_trip() {
        let stream = "ZCZC EA42\r\nHELLO\r\nNNNN";
        let m = parse(stream);
        let json = m.to_json();
        let back: NavtexMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        assert!(json.contains("\"station\":\"E\""));
        assert!(json.contains("\"message_number\":42"));
    }
}
