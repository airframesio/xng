//! ACARS application-layer decoding: the part of the stack shared by every
//! ACARS carrier (VHF POA, VDL-2 AOA, HFDL, Inmarsat Aero, Iridium).
//!
//! Ported from MIT-licensed libacars (see PROVENANCE.md). Decodes ARINC 622
//! ATS envelopes (ADS-C in full; CPDLC envelopes with verified payloads,
//! ASN.1 body decode pending), media advisory, and H1 sublabel/MFI
//! extraction.

mod bits;

pub mod adsc;
pub mod arinc622;
pub mod cpdlc;
pub mod block;
pub mod media_adv;
pub mod miam;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<AcarsApp>,
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
        "H1" => arinc622::parse(body, downlink)
            .or_else(|| ohma::parse(body).map(|message| AcarsApp::Ohma { message })),
        "MA" => miam::parse(body).map(|frame| AcarsApp::Miam { frame }),
        "SA" => media_adv::parse(body).map(AcarsApp::MediaAdvisory),
        _ => qseries::classify(label).map(AcarsApp::QSeries),
    };

    // OOOI fields can be embedded in many labels' text (Q-series and several
    // airline-application labels); run the extractor on the original text.
    out.oooi = oooi::decode(label, text);
    // Free-text position reports (labels 20/POS, 4J, H1 POS) → lat/lon.
    out.position = position::decode(label, text);
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
