//! ACARS application-layer decoding: the part of the stack shared by every
//! ACARS carrier (VHF POA, VDL-2 AOA, HFDL, Inmarsat Aero, Iridium).
//!
//! Ported from MIT-licensed libacars (see PROVENANCE.md). Decodes ARINC 622
//! ATS envelopes (ADS-C in full; CPDLC envelopes with verified payloads,
//! ASN.1 body decode pending), media advisory, and H1 sublabel/MFI
//! extraction.

mod bits;

pub mod adsc;
pub mod airline5z;
pub mod arinc622;
pub mod cpdlc;
pub mod block;
pub mod cfb;
pub mod fpn;
pub mod gate;
pub mod loadsheet;
pub mod media_adv;
pub mod met;
pub mod miam;
pub mod military;
pub mod min;
pub mod ohma;
pub mod oooi;
pub mod position;
pub mod qseries;
pub mod reasm;
pub mod sublabel;

use serde::Serialize;

/// A decoded application-layer payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "app", rename_all = "snake_case")]
pub enum AcarsApp {
    /// ADS-C (FANS-1/A) message or disconnect, from an ARINC 622 envelope.
    Adsc {
        #[serde(flatten)]
        envelope: arinc622::Envelope,
        #[serde(flatten)]
        message: adsc::AdscMessage,
    },
    /// MIAM (ARINC 841) frame: single-transfer CORE PDUs (decompressed)
    /// or file-transfer signalling.
    Miam {
        #[serde(flatten)]
        frame: miam::MiamFrame,
    },
    /// OHMA aircraft-health JSON (Boeing), inflated and parsed.
    Ohma { message: serde_json::Value },
    /// CPDLC (FANS-1/A) message in an ARINC 622 envelope: header and the
    /// first message element identified from the unaligned-PER body
    /// (element arguments are a planned follow-up).
    Cpdlc {
        #[serde(flatten)]
        envelope: arinc622::Envelope,
        #[serde(flatten, skip_serializing_if = "Option::is_none")]
        message: Option<cpdlc::CpdlcMessage>,
        payload_hex: String,
    },
    /// Media advisory (label SA): datalink availability report.
    MediaAdvisory(media_adv::MediaAdvisory),
    /// `Q`-series link-test / squitter / OOOI-event label classification.
    QSeries(qseries::QSeries),
    /// H1 `#CFB` ("Crew Flight Bag") maintenance-telemetry family.
    Cfb(cfb::Cfb),
    /// ARINC 702 flight plan (H1 `FPN/` preamble).
    FlightPlan(fpn::FlightPlan),
    /// Label 5Z "Airline Designated Downlink" (United telex / structured
    /// free-text).
    Airline5z(airline5z::Airline5z),
    /// Military (AMC-style) ARINC 702 report: `<KIND>/ID<tail>,<callsign>,
    /// <mission>/…` on H1 (`MD` sublabel) and many other labels.
    Military(Box<military::MilitaryReport>),
    /// Weight and balance loadsheet (IATA AHM 517 and airline layouts), its
    /// acceptance by the crew, or a request for one; on any label.
    Loadsheet(Box<loadsheet::Loadsheet>),
    /// Gate assignment: arrival gate uplinks on label RA (United GATE ASSIGN
    /// / GATE CHANGE, Republic, SkyWest, Air Wisconsin, Frontier, American
    /// and its regionals), pre-departure clearance departure gates (B3), and
    /// airline gate, stand and parking messages on 10, 11, 21, 25, 3M, 3T,
    /// 40, 41, 42, 5Z and C1.
    Gate(gate::GateInfo),
}

/// Result of running the application layer over one ACARS message.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AppDecode {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sublabel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mfi: Option<String>,
    /// OOOI (OUT/OFF/ON/IN) gate/wheels times + departure/destination
    /// airports + ETA extracted from the text (acarsdec
    /// `depa`/`dsta`/`eta`/`gtout`/`gtin`/`wloff`/`wlin` fields). Flattened
    /// so the fields appear at the top level like acarsdec's JSON.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub oooi: Option<oooi::Oooi>,
    /// Latitude/longitude extracted from a free-text position report
    /// (labels `20`/POS, `4J`, `H1` POS).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<position::Position>,
    /// Winds-aloft / met fields from a position-weather report (label `4J`
    /// POSWX).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub met: Option<met::Met>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<AcarsApp>,
}

/// The structured application object inside a body's `app` value (see
/// [`AppDecode::app_value`]): nested under `app` when flat text fields (OOOI,
/// position, met) sit beside it, else the value itself.
pub fn structured_app(app: &serde_json::Value) -> &serde_json::Value {
    match app.get("app") {
        Some(nested @ serde_json::Value::Object(_)) => nested,
        _ => app,
    }
}

impl AppDecode {
    /// The single `app` JSON value a message body carries: the structured
    /// application decode plus the flat text-extracted fields (OOOI times and
    /// airports, free-text position, met). `None` when nothing decoded. Flat
    /// fields sit at the top level (like acarsdec's flat JSON); the structured
    /// app object is nested under `app` when both are present.
    ///
    /// The free-text position is kept only for downlinks: a message to the
    /// aircraft names other places (forecast winds at fixes along the route,
    /// weather), so a coordinate in an uplink is not where the aircraft is.
    pub fn app_value(&self, downlink: bool) -> Option<serde_json::Value> {
        let mut flat = serde_json::Map::new();
        if let Some(serde_json::Value::Object(m)) =
            self.oooi.as_ref().and_then(|o| serde_json::to_value(o).ok())
        {
            flat.extend(m);
        }
        if downlink {
            if let Some(pos) = self.position.as_ref().and_then(|p| serde_json::to_value(p).ok()) {
                flat.insert("position".to_string(), pos);
            }
        }
        if let Some(serde_json::Value::Object(m)) =
            self.met.as_ref().and_then(|m| serde_json::to_value(m).ok())
        {
            flat.extend(m);
        }
        let app = self.app.as_ref().map(|a| serde_json::to_value(a).unwrap_or_default());
        match (flat.is_empty(), app) {
            (true, a) => a,
            (false, None) => Some(serde_json::Value::Object(flat)),
            (false, Some(a)) => {
                flat.insert("app".to_string(), a);
                Some(serde_json::Value::Object(flat))
            }
        }
    }
}

/// Decode the application layer of an ACARS message. `text` is the message
/// text after the downlink MSN/flight-id header (i.e. [`xng-types`]'
/// `AcarsCore::text`); `downlink` from the block-id class.
pub fn decode(label: &str, text: &str, downlink: bool) -> AppDecode {
    let mut out = AppDecode::default();
    let mut body = text;

    if label == "H1" {
        let (sublabel, mfi, rest) = sublabel::extract(text, downlink);
        out.sublabel = sublabel;
        out.mfi = mfi;
        body = rest;
    }

    out.app = match label {
        "A6" | "AA" | "B6" | "BA" => arinc622::parse(body, downlink),
        // H1 carries ARINC 622, OHMA, or the #CFB maintenance family; #CFB
        // is recognized from the original text (the `#CFB` preamble is the
        // H1 `CF` sublabel, already stripped from `body`).
        "H1" => arinc622::parse(body, downlink)
            .or_else(|| military::parse(body).map(|m| AcarsApp::Military(Box::new(m))))
            .or_else(|| fpn::parse(body).map(AcarsApp::FlightPlan))
            .or_else(|| cfb::classify(text).map(AcarsApp::Cfb))
            .or_else(|| ohma::parse(body).map(|message| AcarsApp::Ohma { message })),
        "MA" => miam::parse(body).map(|frame| AcarsApp::Miam { frame }),
        "SA" => media_adv::parse(body).map(AcarsApp::MediaAdvisory),
        "5Z" => airline5z::parse(body).map(AcarsApp::Airline5z),
        "RA" => gate::parse(text)
            .map(AcarsApp::Gate)
            .or_else(|| military::parse(text).map(|m| AcarsApp::Military(Box::new(m)))),
        _ => qseries::classify(label)
            .map(AcarsApp::QSeries)
            .or_else(|| military::parse(text).map(|m| AcarsApp::Military(Box::new(m)))),
    };
    // Loadsheets travel on many labels (RA, C1, 2A, 10, 3L, 22, ...); try
    // them on any label whose text mentions one and nothing else decoded.
    if out.app.is_none() && loadsheet::may_be_loadsheet(text) {
        out.app = loadsheet::parse(text).map(|l| AcarsApp::Loadsheet(Box::new(l)));
    }
    // Gate reports (B3 clearances, airline gate/stand messages on 10/11/21/
    // 25/3M/3T/40/41/42/5Z/C1, and the label RA families after the uplinks above) come last, so
    // they never replace a richer decode of the same text.
    if out.app.is_none() {
        out.app = gate::parse_report(label, text).map(AcarsApp::Gate);
    }

    // OOOI fields can be embedded in many labels' text (Q-series and several
    // airline-application labels); run the extractor on the original text.
    out.oooi = oooi::decode(label, text);
    // Free-text position reports (labels 20/POS, 4J, H1 POS) → lat/lon.
    out.position = position::decode(label, text);
    // Winds-aloft / met from the 4J POSWX position-weather report.
    out.met = met::decode(label, text);
    out
}

/// One-line human summary of the most useful decoded content (for console
/// output); `None` when there is nothing succinct to say.
pub fn summary(app: &AcarsApp) -> Option<String> {
    match app {
        AcarsApp::Adsc { message, .. } => message.summary(),
        AcarsApp::Cpdlc { envelope, .. } => {
            Some(format!("CPDLC {} ({})", envelope.imi.as_str(), envelope.gs_addr))
        }
        AcarsApp::MediaAdvisory(m) => Some(format!(
            "MEDIA-ADV link {} {} at {}",
            m.current_link,
            if m.established { "established" } else { "lost" },
            m.time
        )),
        AcarsApp::QSeries(q) => Some(format!("{} {}", q.label, q.description)),
        AcarsApp::Cfb(c) => Some(if c.subtype.is_empty() {
            format!("CFB {}", c.description)
        } else {
            format!("CFB {} ({})", c.subtype, c.description)
        }),
        AcarsApp::FlightPlan(fp) => Some(format!(
            "FPN {}{}->{}",
            fp.flight_number.as_deref().map(|f| format!("{f} ")).unwrap_or_default(),
            fp.origin.as_deref().unwrap_or("?"),
            fp.destination.as_deref().unwrap_or("?"),
        )),
        AcarsApp::Airline5z(a) => Some(match a {
            airline5z::Airline5z::Text { text } => format!("5Z TXT {text}"),
            airline5z::Airline5z::Typed { message_type, description, .. } => {
                format!("5Z {message_type} ({description})")
            }
        }),
        AcarsApp::Military(m) => Some(m.summary()),
        AcarsApp::Loadsheet(l) => Some(l.summary()),
        AcarsApp::Gate(g) => Some(g.summary()),
        AcarsApp::Miam { frame } => Some(match frame {
            miam::MiamFrame::SingleTransfer(p) => format!(
                "MIAM v{} {}{}{}",
                p.version,
                p.pdu_type,
                p.app_id.as_deref().map(|a| format!(" app={a}")).unwrap_or_default(),
                if p.compressed { format!(" ({} bytes inflated)", p.data_len) } else { String::new() }
            ),
            miam::MiamFrame::FileTransferReq { file_id, file_size } => {
                format!("MIAM file-transfer-req id={file_id} size={file_size}")
            }
            miam::MiamFrame::FileSegment { file_id, segment_id, .. } => {
                format!("MIAM file-segment id={file_id} seg={segment_id}")
            }
            f => format!("MIAM {}", serde_json::json!(f)["frame"].as_str().unwrap_or("frame")),
        }),
        AcarsApp::Ohma { message } => Some(format!(
            "OHMA {}",
            message
                .pointer("/message/sysid")
                .or_else(|| message.get("version"))
                .map(|v| v.to_string().trim_matches('"').to_owned())
                .unwrap_or_default()
        )),
    }
}
