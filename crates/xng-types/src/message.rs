use crate::{Mode, Provenance};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// RF signal measurements taken at decode time. All fields optional — not
/// every demodulator can estimate every quantity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SignalQuality {
    /// Received signal strength, dBFS (or dBm where calibrated).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rssi_db: Option<f32>,
    /// Estimated signal-to-noise ratio, dB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snr_db: Option<f32>,
    /// Noise floor estimate, dBFS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub noise_db: Option<f32>,
    /// Carrier frequency offset from channel center, Hz.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freq_skew_hz: Option<f32>,
}

/// Decode/FEC quality for a frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodeQuality {
    /// Whether the final integrity check (CRC/FCS/parity) passed.
    pub crc_ok: bool,
    /// Bits corrected by FEC (Viterbi/RS/etc.), where known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fec_corrected: Option<u32>,
    /// Residual uncorrected errors, where known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<u32>,
}

/// The ACARS application-layer fields common to POA, VDL2 (AOA), HFDL
/// (HFNPDU), Aero and Iridium carriage. ARINC 618 naming.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AcarsCore {
    /// ACARS mode character (e.g. '2').
    pub mode: char,
    /// Aircraft registration ("tail"), dot-padded form stripped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail: Option<String>,
    /// Message label, e.g. `H1`, `Q0`, `_d`.
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sublabel: Option<String>,
    /// Multi-function identifier (follows the sublabel on H1 messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mfi: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_id: Option<char>,
    /// Technical ack character; `None` = NAK ('!').
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ack: Option<char>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flight: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_num: Option<String>,
    /// Message text payload (may be empty).
    pub text: String,
    /// True when more blocks follow (ETB rather than ETX).
    pub more_to_come: bool,
    /// True when `text` was reassembled from multiple blocks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reassembled: bool,
    /// Decoded application layer (ADS-C, CPDLC envelope, media advisory,
    /// ...), as produced by xng-acars.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<serde_json::Value>,
}

/// Typed per-mode message bodies. Deliberately minimal for M0; each mode core
/// extends this as it lands. The raw payload always travels alongside in
/// [`Message::raw`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageBody {
    /// An ACARS message (any carrier: POA, AOA, HFDL, Aero, Iridium).
    Acars(AcarsCore),
    /// AIS: NMEA AIVDM sentences plus decoded essentials (extended later).
    Ais {
        nmea: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        msg_type: Option<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        mmsi: Option<u32>,
        /// Decoded fields (position, kinematics, static/voyage data).
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
    },
    /// Mode S / ADS-B frame summary (positions/BDS depth land later).
    ModeS {
        df: u8,
        icao: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        callsign: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        altitude_ft: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        squawk: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        lat: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        lon: Option<f64>,
        /// Knots; see `speed_type` ("GS" ground / "AS" airspeed).
        #[serde(skip_serializing_if = "Option::is_none")]
        speed_kt: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        speed_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        track_deg: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        vertical_rate_fpm: Option<i32>,
        /// Comm-B register content (BDS-inferred from DF20/21).
        #[serde(skip_serializing_if = "Option::is_none")]
        comm_b: Option<serde_json::Value>,
    },
    /// Iridium frame (ring alert, broadcast, ...).
    Iridium {
        kind: String,
        details: serde_json::Value,
    },
    /// VDL2 non-ACARS frame: AVLC link events (acks, XID handoffs) and
    /// ATN traffic, with addresses and decoded parameters.
    Vdl2 {
        kind: String,
        details: serde_json::Value,
    },
    /// HFDL non-ACARS event (squitter, logon, performance data, ...).
    Hfdl {
        kind: String,
        details: serde_json::Value,
    },
    /// Inmarsat STD-C / EGC packet (SafetyNET, FleetNET, system).
    StdC {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        details: serde_json::Value,
    },
    /// A frame decoded at link layer but with no (or not-yet-implemented)
    /// application-layer interpretation.
    /// Inmarsat Aero non-ACARS structures (C-channel assignments, ...).
    Aero {
        kind: String,
        details: serde_json::Value,
    },
    Undecoded,
}

/// The normalized message: what every decode core emits onto the bus and
/// every output consumes. In-process form of the future asf-2.0 envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Decode mode that produced this message.
    pub mode: Mode,
    /// Reception timestamp (UTC). Nanosecond precision where the source
    /// provides it.
    pub timestamp: DateTime<Utc>,
    /// RF carrier frequency in Hz (u64 — no MHz floats on the wire).
    pub frequency_hz: u64,
    #[serde(default)]
    pub signal: SignalQuality,
    #[serde(default)]
    pub decode: DecodeQuality,
    pub body: MessageBody,
    /// Raw frame payload (link-layer), preserved for re-decoding.
    #[serde(skip_serializing_if = "Option::is_none", with = "hex_bytes", default)]
    pub raw: Option<Vec<u8>>,
    pub source: Provenance,
}

/// Serialize raw bytes as lowercase hex for JSON friendliness.
mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(bytes) => {
                let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                s.serialize_some(&hex)
            }
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let hex: Option<String> = Option::deserialize(d)?;
        match hex {
            None => Ok(None),
            Some(h) => {
                if h.len() % 2 != 0 {
                    return Err(serde::de::Error::custom("odd-length hex string"));
                }
                (0..h.len())
                    .step_by(2)
                    .map(|i| {
                        u8::from_str_radix(&h[i..i + 2], 16).map_err(serde::de::Error::custom)
                    })
                    .collect::<Result<Vec<u8>, _>>()
                    .map(Some)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppInfo, StationIdentity};

    fn sample() -> Message {
        Message {
            mode: Mode::AcarsPoa,
            timestamp: Utc::now(),
            frequency_hz: 131_550_000,
            signal: SignalQuality { rssi_db: Some(-21.5), snr_db: Some(12.0), ..Default::default() },
            decode: DecodeQuality { crc_ok: true, ..Default::default() },
            body: MessageBody::Acars(AcarsCore {
                mode: '2',
                tail: Some("N123AB".into()),
                label: "Q0".into(),
                text: String::new(),
                ..Default::default()
            }),
            raw: Some(vec![0x2b, 0x2a, 0x16]),
            source: Provenance {
                station: StationIdentity::new("XX-TEST-ACARS"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        }
    }

    #[test]
    fn json_roundtrip() {
        let msg = sample();
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"2b2a16\""), "raw should be hex: {json}");
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back.frequency_hz, msg.frequency_hz);
        assert_eq!(back.raw, msg.raw);
    }
}
