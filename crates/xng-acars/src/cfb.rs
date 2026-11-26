//! H1 `#CFB` ("Crew Flight Bag") / `CF` Boeing-Airbus maintenance-telemetry
//! family classifier.
//!
//! The `#CFB` preamble (H1 sublabel `CF`) heads a large family of distinct
//! maintenance-telemetry sub-formats. This module classifies the sub-type
//! so each schema can be routed/labelled; it does not (yet) parse the full
//! per-sub-type body.
//!
//! Oracle: airframes acars-message-documentation `research/H1/CFB.md` and
//! `research/H1/CFB/CFB.01.md` — the documented sub-type set
//! (`APM_REPORT`, `ATA`, `AL`, `FDE`, `ECT`, `FLR`, `LIGHTS`, `MIL`, `MPF`,
//! `PAGE`, `WRN`, and the `.01`/`.1` failure form) and the acronym table
//! (`CFB` = Crew Flight Bag, `APM` = Aircraft Performance Monitoring,
//! `FDE` = Flight Deck Effect, `FLR` = Realtime Failure, `MPF` =
//! Maintenance Planning Function, `WRN` = Warning, `MIL` = Engine Spool
//! Vibration Units). Descriptions for sub-types without an acronym-table
//! entry are taken from the documented example content.

use serde::Serialize;

/// A `#CFB` maintenance-family sub-type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cfb {
    /// The sub-type token following `#CFB` (e.g. `FLR`, `APM_REPORT`,
    /// `.01`), as it appears in the message.
    pub subtype: String,
    pub kind: CfbKind,
    /// Human description of the sub-type (from the airframes CFB docs).
    pub description: &'static str,
}

/// Classification of a `#CFB` sub-format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CfbKind {
    /// Aircraft Performance Monitoring / ACMF snapshot report (`APM_REPORT`).
    ApmReport,
    /// ATA-chapter fault report (`ATA`).
    AtaFault,
    /// Realtime failure (`FLR`) — a fault detected in flight.
    RealtimeFailure,
    /// Flight Deck Effect (`FDE`).
    FlightDeckEffect,
    /// Engine / temperature status report (`AL`, `ECT`).
    EngineStatus,
    /// Engine spool vibration units report (`MIL`).
    VibrationReport,
    /// Maintenance Planning Function (`MPF`).
    MaintenancePlanning,
    /// Warning (`WRN`).
    Warning,
    /// Lighting status / fault (`LIGHTS`).
    Lights,
    /// MDC report page (`PAGE`).
    McduPage,
    /// `.01` / `.1` failure-fault-warning record.
    FailureRecord,
    /// Recognized `#CFB` message with no further documented sub-type.
    Generic,
}

/// Classify a `#CFB` message from the full H1 message text. Returns `None`
/// when the text is not a `#CFB` ("Crew Flight Bag") message.
pub fn classify(text: &str) -> Option<Cfb> {
    let rest = text.strip_prefix("#CFB")?;

    // Longest-prefix match so APM_REPORT is not shadowed by a shorter token.
    // Order matters only for the dotted form vs the bare tokens.
    let (subtype, kind, description): (&str, CfbKind, &str) =
        if rest.starts_with(".01") || rest.starts_with(".1") {
            let tok = if rest.starts_with(".01") { ".01" } else { ".1" };
            (tok, CfbKind::FailureRecord, "Failure/fault/warning record")
        } else if rest.starts_with("APM_REPORT") {
            ("APM_REPORT", CfbKind::ApmReport, "Aircraft Performance Monitoring / ACMF snapshot report")
        } else if rest.starts_with("APM") {
            ("APM", CfbKind::ApmReport, "Aircraft Performance Monitoring report")
        } else if rest.starts_with("ATA") {
            ("ATA", CfbKind::AtaFault, "ATA-chapter fault report")
        } else if rest.starts_with("FDE") {
            ("FDE", CfbKind::FlightDeckEffect, "Flight Deck Effect")
        } else if rest.starts_with("FLR") {
            ("FLR", CfbKind::RealtimeFailure, "Realtime failure")
        } else if rest.starts_with("ECT") {
            ("ECT", CfbKind::EngineStatus, "Engine status / fault report")
        } else if rest.starts_with("LIGHTS") {
            ("LIGHTS", CfbKind::Lights, "Lighting status / fault report")
        } else if rest.starts_with("MIL") {
            ("MIL", CfbKind::VibrationReport, "Engine spool vibration units report")
        } else if rest.starts_with("MPF") {
            ("MPF", CfbKind::MaintenancePlanning, "Maintenance Planning Function")
        } else if rest.starts_with("PAGE") {
            ("PAGE", CfbKind::McduPage, "MDC report page")
        } else if rest.starts_with("WRN") {
            ("WRN", CfbKind::Warning, "Warning")
        } else if rest.starts_with("AL") {
            ("AL", CfbKind::EngineStatus, "Air temperature / FADEC bleed status report")
        } else {
            ("", CfbKind::Generic, "Crew Flight Bag message")
        };

    Some(Cfb {
        subtype: subtype.to_owned(),
        kind,
        description,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference strings are the real documented examples from airframes'
    // acars-message-documentation research/H1/CFB.md (and CFB.01.md).

    #[test]
    fn apm_report() {
        let c = classify("#CFBAPM_REPORT_A_20200805180631S.CSV").unwrap();
        assert_eq!(c.subtype, "APM_REPORT");
        assert_eq!(c.kind, CfbKind::ApmReport);
    }

    #[test]
    fn ata_fault() {
        let c = classify("#CFBATA\n\nVIA-1//20DEC//1653//1026//0//DU-6 LOW BRIGHTNESS..").unwrap();
        assert_eq!(c.subtype, "ATA");
        assert_eq!(c.kind, CfbKind::AtaFault);
    }

    #[test]
    fn al_engine_status() {
        let c = classify("#CFBAL AIR TEMP            32.5 C").unwrap();
        assert_eq!(c.subtype, "AL");
        assert_eq!(c.kind, CfbKind::EngineStatus);
    }

    #[test]
    fn fde_flight_deck_effect() {
        let c = classify("#CFBFDE1807300805ABD").unwrap();
        assert_eq!(c.subtype, "FDE");
        assert_eq!(c.kind, CfbKind::FlightDeckEffect);
    }

    #[test]
    fn ect_engine_status() {
        let c = classify("#CFBECT FAULT     CH-A").unwrap();
        assert_eq!(c.subtype, "ECT");
        assert_eq!(c.kind, CfbKind::EngineStatus);
    }

    #[test]
    fn flr_realtime_failure() {
        let c = classify("#CFBFLR/FR19121418400034433406TCAS (1SG)").unwrap();
        assert_eq!(c.subtype, "FLR");
        assert_eq!(c.kind, CfbKind::RealtimeFailure);
        assert_eq!(c.description, "Realtime failure");
    }

    #[test]
    fn lights_report() {
        let c = classify("#CFBLIGHTS\n R PRIM NAV LT      DS23").unwrap();
        assert_eq!(c.subtype, "LIGHTS");
        assert_eq!(c.kind, CfbKind::Lights);
    }

    #[test]
    fn mil_vibration_report() {
        let c = classify("#CFBMIL\nR N1 VIBES                 0.2 MIL").unwrap();
        assert_eq!(c.subtype, "MIL");
        assert_eq!(c.kind, CfbKind::VibrationReport);
    }

    #[test]
    fn mpf_maintenance_planning() {
        let c = classify("#CFBMPF/               /AN.N660AW/FIAAL652").unwrap();
        assert_eq!(c.subtype, "MPF");
        assert_eq!(c.kind, CfbKind::MaintenancePlanning);
    }

    #[test]
    fn page_mdc_report() {
        let c = classify("#CFBPAGE 00001\nMDC REPORT: ENGINE TREND").unwrap();
        assert_eq!(c.subtype, "PAGE");
        assert_eq!(c.kind, CfbKind::McduPage);
    }

    #[test]
    fn wrn_warning() {
        let c = classify("#CFBWRN/WN19121418390034000006NAV TCAS FAULT").unwrap();
        assert_eq!(c.subtype, "WRN");
        assert_eq!(c.kind, CfbKind::Warning);
    }

    #[test]
    fn dotted_failure_record() {
        // research/H1/CFB/CFB.01.md
        let c = classify("#CFB.1/FLR/FR1602082254 27513406ADR1 X2,ADR3X,ADR2X").unwrap();
        assert_eq!(c.subtype, ".1");
        assert_eq!(c.kind, CfbKind::FailureRecord);
    }

    #[test]
    fn generic_cfb_with_slash() {
        // research/H1/CFB.md first example: "#CFB/1315//38//0//RA-1"
        let c = classify("#CFB/1315//38//0//RA-1").unwrap();
        assert_eq!(c.kind, CfbKind::Generic);
    }

    #[test]
    fn non_cfb_rejected() {
        assert!(classify("#DFB/M1 ENGINE DATA").is_none());
        assert!(classify("POSN38160W077075").is_none());
        assert!(classify("").is_none());
    }
}
