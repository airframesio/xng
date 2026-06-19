//! Generic ACARS sublabel / MFI extraction beyond the H1 case (ACARS-3.2).
//!
//! ## Background
//!
//! ARINC 620 lets an ACARS message carry a two-character *sublabel* and an
//! optional *Message Function Identifier* (MFI) at the front of the text,
//! which together with the ACARS label select the ground "SMI" (Standard
//! Message Identifier). The on-air grammar is identical regardless of which
//! label carries it:
//!
//! - **Downlink** (air→ground): text begins `#xxB` — `xx` is the sublabel,
//!   `B` (0x42) the sentinel that terminates it.
//! - **Uplink** (ground→air): text begins `- #xx` — `xx` is the sublabel.
//! - When a sublabel is present, an MFI may follow as `/yy ` (`yy` is the
//!   MFI, terminated by a space).
//!
//! ## Relationship to libacars (the oracle for the grammar)
//!
//! libacars 2.2.1 `acars.c` (`la_acars_extract_sublabel_and_mfi`) implements
//! exactly this byte-grammar, but gates it on `label == "H1"`:
//!
//! ```c
//! if (label[0] == 'H' && label[1] == '1') {
//!     if (msg_dir == LA_MSG_DIR_GND2AIR) {                 // uplink
//!         if (remaining >= 5 && strncmp(ptr, "- #", 3) == 0) { sublabel = ptr+3; ptr += 5; }
//!     } else if (msg_dir == LA_MSG_DIR_AIR2GND) {          // downlink
//!         if (remaining >= 4 && ptr[0] == '#' && ptr[3] == 'B') { sublabel = ptr+1; ptr += 4; }
//!     }
//!     if (sublabel != NULL && remaining >= 4 && ptr[0] == '/' && ptr[3] == ' ') { mfi = ptr+1; ptr += 4; }
//! }
//! ```
//!
//! The shared `xng-acars` crate already ports the H1 path verbatim
//! (`xng_acars::sublabel::extract`); this module reuses *the same grammar*
//! but applies it to the wider family of labels that carry the `#`-sublabel
//! convention in real ARINC 620 traffic — canonically **H2** (the second
//! "general aviation / maintenance" supervisory label, structurally a twin of
//! H1) — without modifying the shared crate. The grammar is libacars-grounded;
//! only the label gate is widened (ARINC 620-4 App C maps label+sublabel→SMI
//! for these families).
//!
//! We deliberately only *add* sublabels for labels libacars itself would not
//! touch, and only when the text actually presents the sentinel — so this
//! never contradicts the H1 decode that `xng-acars` already produces.

/// Labels (besides H1, handled upstream by `xng-acars`) that carry the same
/// `#xx` / `- #xx` sublabel grammar. H2 is the documented twin of H1.
const SUBLABEL_LABELS: &[&str] = &["H2"];

/// Result of generic sublabel/MFI extraction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sublabel {
    pub sublabel: Option<String>,
    pub mfi: Option<String>,
}

/// Extract a sublabel (and optional MFI) for a non-H1 label, using libacars's
/// H1 byte-grammar. Returns `None` when the label is not in the sublabel
/// family or the text does not present the sentinel — i.e. nothing is forced.
///
/// `label` is the two-character ACARS label; `text` is the message text
/// (after the downlink MSN/flight header, matching `AcarsCore::text`);
/// `downlink` from the block-id class.
pub fn extract(label: &str, text: &str, downlink: bool) -> Option<Sublabel> {
    // H1 is owned by xng-acars; never shadow it here.
    if label == "H1" || !SUBLABEL_LABELS.contains(&label) {
        return None;
    }

    let b = text.as_bytes();
    // Mirror la_acars_extract_sublabel_and_mfi's index arithmetic exactly.
    let (sublabel, mut consumed) = if downlink {
        // ptr[0]=='#' && ptr[3]=='B', sublabel = ptr[1..3]
        if b.len() >= 4 && b[0] == b'#' && b[3] == b'B' {
            (text[1..3].to_owned(), 4)
        } else {
            return None;
        }
    } else {
        // "- #" then sublabel = ptr[3..5]
        if b.len() >= 5 && text.starts_with("- #") {
            (text[3..5].to_owned(), 5)
        } else {
            return None;
        }
    };

    let mut mfi = None;
    let rest = &b[consumed..];
    // "/yy " — ptr[0]=='/' && ptr[3]==' ', mfi = ptr[1..3].
    if rest.len() >= 4 && rest[0] == b'/' && rest[3] == b' ' {
        mfi = Some(text[consumed + 1..consumed + 3].to_owned());
        consumed += 4;
    }
    let _ = consumed; // text-stripping is the caller's choice; we surface fields only.

    Some(Sublabel { sublabel: Some(sublabel), mfi })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Oracle: libacars `la_acars_extract_sublabel_and_mfi` grammar ---
    //
    // libacars only RUNS this grammar for H1, but the grammar itself is the
    // oracle. We validate that our port produces libacars-identical results
    // for the H1 worked example (the one quoted in libacars PROG_GUIDE.md and
    // unit-tested in xng-acars) by temporarily treating H2 with the same input
    // shapes — byte-for-byte the same parsing libacars applies to H1.

    #[test]
    fn downlink_sublabel_and_mfi_h2() {
        // PROG_GUIDE.md worked example shape: "#xxB/yy ...": sublabel xx, mfi yy.
        // (libacars example uses "#M1B6..."→ sublabel "M1"; here H2 + DF/M1.)
        let r = extract("H2", "#DFB/M1 POSRPT", true).expect("sublabel expected");
        assert_eq!(r.sublabel.as_deref(), Some("DF"));
        assert_eq!(r.mfi.as_deref(), Some("M1"));
    }

    #[test]
    fn downlink_sublabel_only_h2() {
        let r = extract("H2", "#M1BPOSRPT", true).expect("sublabel expected");
        assert_eq!(r.sublabel.as_deref(), Some("M1"));
        assert_eq!(r.mfi, None);
    }

    #[test]
    fn uplink_sublabel_h2() {
        let r = extract("H2", "- #MDTEXT", false).expect("sublabel expected");
        assert_eq!(r.sublabel.as_deref(), Some("MD"));
        assert_eq!(r.mfi, None);
    }

    #[test]
    fn uplink_sublabel_and_mfi_h2() {
        let r = extract("H2", "- #MD/A6 TEXT", false).expect("sublabel expected");
        assert_eq!(r.sublabel.as_deref(), Some("MD"));
        assert_eq!(r.mfi.as_deref(), Some("A6"));
    }

    #[test]
    fn h1_is_left_to_xng_acars() {
        // We must never produce a sublabel for H1; xng-acars owns it.
        assert_eq!(extract("H1", "#DFB/M1 POSRPT", true), None);
    }

    #[test]
    fn non_family_label_yields_nothing() {
        // A plain label with a leading '#' must NOT be mistaken for a sublabel.
        assert_eq!(extract("Q0", "#DFB/M1 POSRPT", true), None);
        assert_eq!(extract("5Z", "#ABBHELLO", true), None);
    }

    #[test]
    fn family_label_without_sentinel_yields_nothing() {
        // H2 text that doesn't present the sentinel → no false sublabel.
        assert_eq!(extract("H2", "PLAIN TEXT", true), None);
        assert_eq!(extract("H2", "#DFXPOSRPT", true), None); // 4th char not 'B'
        assert_eq!(extract("H2", "- PLAIN", false), None); // no "- #"
    }

    #[test]
    fn mfi_requires_trailing_space() {
        // "/yy " grammar: without the terminating space, no MFI (libacars rule).
        let r = extract("H2", "#DFB/M1POSRPT", true).expect("sublabel expected");
        assert_eq!(r.sublabel.as_deref(), Some("DF"));
        assert_eq!(r.mfi, None);
    }
}
