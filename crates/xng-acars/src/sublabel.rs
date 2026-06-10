//! H1 sublabel / MFI extraction (ported from libacars `acars.c`,
//! `la_acars_extract_sublabel_and_mfi`).
//!
//! Downlinks: text starts `#xxB` (sublabel = xx). Uplinks: `- #xx`.
//! When a sublabel is present, an MFI may follow as `/yy ` .

/// Returns (sublabel, mfi, remaining_text).
pub fn extract(text: &str, downlink: bool) -> (Option<String>, Option<String>, &str) {
    let b = text.as_bytes();
    let (sublabel, mut consumed) = if downlink {
        if b.len() >= 4 && b[0] == b'#' && b[3] == b'B' {
            (Some(text[1..3].to_owned()), 4)
        } else {
            (None, 0)
        }
    } else if b.len() >= 5 && text.starts_with("- #") {
        (Some(text[3..5].to_owned()), 5)
    } else {
        (None, 0)
    };

    let mut mfi = None;
    if sublabel.is_some() {
        let rest = &b[consumed..];
        if rest.len() >= 4 && rest[0] == b'/' && rest[3] == b' ' {
            mfi = Some(text[consumed + 1..consumed + 3].to_owned());
            consumed += 4;
        }
    }
    (sublabel, mfi, &text[consumed..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downlink_sublabel_and_mfi() {
        let (s, m, rest) = extract("#DFB/M1 POSRPT", true);
        assert_eq!(s.as_deref(), Some("DF"));
        assert_eq!(m.as_deref(), Some("M1"));
        assert_eq!(rest, "POSRPT");
    }

    #[test]
    fn downlink_sublabel_only() {
        let (s, m, rest) = extract("#M1BPOSRPT", true);
        assert_eq!(s.as_deref(), Some("M1"));
        assert_eq!(m, None);
        assert_eq!(rest, "POSRPT");
    }

    #[test]
    fn uplink_sublabel() {
        let (s, m, rest) = extract("- #MDTEXT", false);
        assert_eq!(s.as_deref(), Some("MD"));
        assert_eq!(m, None);
        assert_eq!(rest, "TEXT");
    }

    #[test]
    fn no_sublabel_passthrough() {
        let (s, m, rest) = extract("PLAIN TEXT", true);
        assert_eq!(s, None);
        assert_eq!(m, None);
        assert_eq!(rest, "PLAIN TEXT");
    }
}
