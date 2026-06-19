//! ACARS `Q`-series link-test / squitter / OOOI-event label family
//! (ARINC 620 standard downlink labels `Q0`–`Q7`, `QA`–`QX`).
//!
//! The `Q` labels are the carrier's own link-management and gate/flight
//! event ("OOOI": OUT/OFF/ON/IN) reports rather than airline application
//! text. This module gives every `Q` label a human classification; the
//! actual OOOI airport/time fields are extracted by [`crate::oooi`].
//!
//! Oracle for the descriptions (every entry is backed by a documented
//! reference, never invented):
//!   - airframes acars-message-documentation: `Q0` = "ACARS Link Test",
//!     `Q2` = "ETA Report", `QF` = "OFF Destination Report",
//!     `QQ` = "OFF Report".
//!   - airframes acars-decoder-typescript plugins (airframes' own decoder):
//!     `QP` = "OUT Report", `QQ` = "OFF Report", `QR` = "ON Report",
//!     `QS` = "IN Report".
//!   - f00b4r0/acarsdec `label.c` OOOI table fixes the gate/wheels event
//!     each remaining `Q` label carries (`QA` gate-out → OUT, `QB`
//!     wheels-off → OFF, `QC` wheels-on → ON, `QD` gate-in → IN, ...),
//!     from which the classification below is named.

use serde::Serialize;

/// The link-management / flight-phase role of a `Q`-series label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QKind {
    /// Link establishment / keep-alive test (label `Q0`, normally empty).
    LinkTest,
    /// Departure-gate-out ("OUT") event report.
    OutReport,
    /// Wheels-off ("OFF") / takeoff event report.
    OffReport,
    /// Wheels-on ("ON") / landing event report.
    OnReport,
    /// Arrival-gate-in ("IN") event report.
    InReport,
    /// Combined OOOI / progress event report (more than one of OUT/OFF/ON/IN).
    OooiReport,
    /// Estimated time of arrival report (label `Q2`).
    EtaReport,
    /// Recognized `Q`-family label with no further documented role.
    Other,
}

/// One classified `Q`-series label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QSeries {
    pub label: String,
    pub kind: QKind,
    /// Human description (matches airframes' own decoder wording where one
    /// exists).
    pub description: &'static str,
}

/// Classify a `Q`-series label. Returns `None` for any label that is not in
/// the `Q0`–`Q7` / `QA`–`QX` link-test/squitter family.
pub fn classify(label: &str) -> Option<QSeries> {
    let b = label.as_bytes();
    if b.len() != 2 || b[0] != b'Q' {
        return None;
    }
    let c = b[1];
    let in_family = c.is_ascii_digit() && (b'0'..=b'7').contains(&c)
        || (b'A'..=b'X').contains(&c);
    if !in_family {
        return None;
    }

    // Descriptions track airframes' own wording (acars-message-documentation
    // and acars-decoder-typescript) where documented; the OOOI-bearing
    // labels are named from the acarsdec `label.c` event each carries.
    let (kind, description) = match c {
        b'0' => (QKind::LinkTest, "ACARS Link Test"),
        b'1' => (QKind::OooiReport, "OOOI Report"),
        b'2' => (QKind::EtaReport, "ETA Report"),
        b'A' => (QKind::OutReport, "OUT Report"),
        b'B' => (QKind::OffReport, "OFF Report"),
        b'C' => (QKind::OnReport, "ON Report"),
        b'D' => (QKind::InReport, "IN Report"),
        b'E' => (QKind::OooiReport, "OUT Report (with destination)"),
        b'F' => (QKind::OffReport, "OFF Destination Report"),
        b'G' => (QKind::OooiReport, "OUT/IN Report"),
        b'H' => (QKind::OutReport, "OUT Report"),
        b'K' => (QKind::OnReport, "ON Destination Report"),
        b'L' => (QKind::InReport, "IN Report"),
        b'M' => (QKind::Other, "Destination Report"),
        b'N' => (QKind::EtaReport, "ETA Report"),
        b'P' => (QKind::OutReport, "OUT Report"),
        b'Q' => (QKind::OffReport, "OFF Report"),
        b'R' => (QKind::OnReport, "ON Report"),
        b'S' => (QKind::InReport, "IN Report"),
        b'T' => (QKind::OooiReport, "OUT/IN Report"),
        // Remaining recognized link/squitter labels with no documented body.
        _ => (QKind::Other, "Link control / squitter"),
    };

    Some(QSeries { label: label.to_owned(), kind, description })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_labels_match_airframes_wording() {
        // airframes acars-message-documentation
        assert_eq!(classify("Q0").unwrap().description, "ACARS Link Test");
        assert_eq!(classify("Q2").unwrap().description, "ETA Report");
        assert_eq!(classify("QF").unwrap().description, "OFF Destination Report");
        // airframes acars-decoder-typescript plugin descriptions
        assert_eq!(classify("QP").unwrap().description, "OUT Report");
        assert_eq!(classify("QQ").unwrap().description, "OFF Report");
        assert_eq!(classify("QR").unwrap().description, "ON Report");
        assert_eq!(classify("QS").unwrap().description, "IN Report");
    }

    #[test]
    fn link_test_is_classified() {
        let q = classify("Q0").unwrap();
        assert_eq!(q.kind, QKind::LinkTest);
        assert_eq!(q.label, "Q0");
    }

    #[test]
    fn oooi_event_labels_classified_from_acarsdec_table() {
        // acarsdec label.c: QA carries gate-out, QB wheels-off,
        // QC wheels-on, QD gate-in.
        assert_eq!(classify("QA").unwrap().kind, QKind::OutReport);
        assert_eq!(classify("QB").unwrap().kind, QKind::OffReport);
        assert_eq!(classify("QC").unwrap().kind, QKind::OnReport);
        assert_eq!(classify("QD").unwrap().kind, QKind::InReport);
    }

    #[test]
    fn family_bounds() {
        // Q0..Q7 and QA..QX are in family.
        assert!(classify("Q7").is_some());
        assert!(classify("QX").is_some());
        // Outside the family / not Q labels.
        assert!(classify("Q8").is_none());
        assert!(classify("Q9").is_none());
        assert!(classify("QY").is_none());
        assert!(classify("QZ").is_none());
        assert!(classify("H1").is_none());
        assert!(classify("Q").is_none());
        assert!(classify("Q0X").is_none());
    }
}
