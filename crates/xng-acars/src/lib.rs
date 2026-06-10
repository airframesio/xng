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
pub mod block;
pub mod media_adv;
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
    /// CPDLC (FANS-1/A) message in an ARINC 622 envelope. The payload has
    /// passed the envelope CRC; ASN.1 PER body decoding lands later.
    Cpdlc {
        #[serde(flatten)]
        envelope: arinc622::Envelope,
        payload_hex: String,
    },
    /// Media advisory (label SA): datalink availability report.
    MediaAdvisory(media_adv::MediaAdvisory),
}

/// Result of running the application layer over one ACARS message.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AppDecode {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sublabel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mfi: Option<String>,
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
        "A6" | "AA" | "B6" | "BA" | "H1" => arinc622::parse(body, downlink),
        "SA" => media_adv::parse(body).map(AcarsApp::MediaAdvisory),
        _ => None,
    };
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
    }
}
