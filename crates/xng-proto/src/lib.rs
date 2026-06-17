//! asf-2.0 wire types (generated from `proto/asf2.proto`) and conversions
//! from the in-process message model.

pub mod asf2 {
    tonic::include_proto!("asf.v2");
}

use xng_types::{Message, MessageBody};

pub const PROTOCOL_VERSION: u32 = 2;
/// ALPN identifier for the QUIC transport.
pub const ALPN: &[u8] = b"asf2";

/// Length-prefix framing for the QUIC transport: u32 big-endian length,
/// then the protobuf-encoded Envelope.
pub fn frame_envelope(env: &asf2::Envelope) -> Vec<u8> {
    use prost::Message as _;
    let body = env.encode_to_vec();
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

pub fn hello(station_id: &str, station_ident: &str, auth_token: &str) -> asf2::Envelope {
    asf2::Envelope {
        kind: Some(asf2::envelope::Kind::Hello(asf2::Hello {
            protocol_version: PROTOCOL_VERSION,
            station_id: station_id.to_owned(),
            station_ident: station_ident.to_owned(),
            app: Some(asf2::App {
                name: "xng".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            }),
            auth_token: auth_token.to_owned(),
            flags: 0,
        })),
    }
}

pub fn batch(seq: u64, messages: Vec<asf2::DecodedMessage>) -> asf2::Envelope {
    asf2::Envelope {
        kind: Some(asf2::envelope::Kind::Batch(asf2::MessageBatch { seq, messages })),
    }
}

impl From<&Message> for asf2::DecodedMessage {
    fn from(m: &Message) -> Self {
        let body = match &m.body {
            MessageBody::Acars(a) => {
                Some(asf2::decoded_message::Body::Acars(asf2::AcarsBody {
                    mode: a.mode.to_string(),
                    tail: a.tail.clone(),
                    label: a.label.clone(),
                    sublabel: a.sublabel.clone(),
                    mfi: a.mfi.clone(),
                    app_json: a.app.as_ref().map(|v| v.to_string()),
                    block_id: a.block_id.map(|c| c.to_string()),
                    ack: a.ack.map(|c| c.to_string()),
                    flight: a.flight.clone(),
                    msg_num: a.msg_num.clone(),
                    text: a.text.clone(),
                    more_to_come: a.more_to_come,
                    reassembled: a.reassembled,
                }))
            }
            MessageBody::Ais { nmea, msg_type, mmsi, details } => {
                Some(asf2::decoded_message::Body::Ais(asf2::AisBody {
                    nmea: nmea.clone(),
                    msg_type: msg_type.map(u32::from),
                    mmsi: *mmsi,
                    details_json: details.as_ref().map(|v| v.to_string()),
                }))
            }
            MessageBody::ModeS {
                df,
                icao,
                callsign,
                altitude_ft,
                squawk,
                lat,
                lon,
                speed_kt,
                speed_type,
                track_deg,
                vertical_rate_fpm,
                comm_b,
                adsb_status,
            } => {
                Some(asf2::decoded_message::Body::ModeS(asf2::ModeSBody {
                    df: u32::from(*df),
                    icao: icao.clone(),
                    callsign: callsign.clone(),
                    altitude_ft: *altitude_ft,
                    squawk: squawk.clone(),
                    lat: *lat,
                    lon: *lon,
                    speed_kt: *speed_kt,
                    speed_type: speed_type.clone(),
                    track_deg: *track_deg,
                    vertical_rate_fpm: *vertical_rate_fpm,
                    comm_b_json: comm_b.as_ref().map(|v| v.to_string()),
                    adsb_status_json: adsb_status.as_ref().map(|v| v.to_string()),
                }))
            }
            MessageBody::Iridium { kind, details } => {
                Some(asf2::decoded_message::Body::Iridium(asf2::IridiumBody {
                    kind: kind.clone(),
                    details_json: details.to_string(),
                }))
            }
            MessageBody::Vdl2 { kind, details } => {
                Some(asf2::decoded_message::Body::Vdl2(asf2::Vdl2Body {
                    kind: kind.clone(),
                    details_json: details.to_string(),
                }))
            }
            MessageBody::Aero { kind, details } => {
                Some(asf2::decoded_message::Body::Aero(asf2::AeroBody {
                    kind: kind.clone(),
                    details_json: details.to_string(),
                }))
            }
            MessageBody::Hfdl { kind, details } => {
                Some(asf2::decoded_message::Body::Hfdl(asf2::HfdlBody {
                    kind: kind.clone(),
                    details_json: details.to_string(),
                }))
            }
            MessageBody::StdC { name, text, details } => {
                Some(asf2::decoded_message::Body::Stdc(asf2::StdcBody {
                    name: name.clone(),
                    text: text.clone(),
                    details_json: details.to_string(),
                }))
            }
            MessageBody::Undecoded => Some(asf2::decoded_message::Body::Undecoded(true)),
        };
        asf2::DecodedMessage {
            mode: m.mode.as_str().to_owned(),
            timestamp_ns: m.timestamp.timestamp_nanos_opt().unwrap_or(0).max(0) as u64,
            frequency_hz: m.frequency_hz,
            signal: Some(asf2::SignalQuality {
                rssi_db: m.signal.rssi_db,
                snr_db: m.signal.snr_db,
                noise_db: m.signal.noise_db,
                freq_skew_hz: m.signal.freq_skew_hz,
            }),
            decode: Some(asf2::DecodeQuality {
                crc_ok: m.decode.crc_ok,
                fec_corrected: m.decode.fec_corrected,
                errors: m.decode.errors,
            }),
            raw: m.raw.clone().unwrap_or_default(),
            sdr: m.source.sdr.as_ref().map(|s| asf2::SdrInfo {
                id: s.id.clone(),
                driver: s.driver.clone(),
                serial: s.serial.clone().unwrap_or_default(),
            }),
            channel: m.source.channel.map(|c| asf2::ChannelInfo {
                index: c.index,
                frequency_hz: c.frequency_hz,
                sample_rate: c.sample_rate,
            }),
            body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use xng_types::*;

    #[test]
    fn converts_acars_message() {
        let msg = Message {
            mode: Mode::AcarsPoa,
            timestamp: Utc::now(),
            frequency_hz: 131_550_000,
            signal: SignalQuality { rssi_db: Some(-20.0), ..Default::default() },
            decode: DecodeQuality { crc_ok: true, fec_corrected: Some(1), errors: Some(0) },
            body: MessageBody::Acars(AcarsCore {
                mode: '2',
                tail: Some("N471XG".into()),
                label: "H1".into(),
                text: "HELLO".into(),
                ..Default::default()
            }),
            raw: Some(vec![1, 2, 3]),
            source: Provenance {
                station: StationIdentity::new("XX-TEST"),
                app: AppInfo::xng(),
                sdr: None,
                channel: Some(ChannelInfo {
                    index: 0,
                    frequency_hz: 131_550_000,
                    sample_rate: 24000.0,
                }),
            },
        };
        let pm = asf2::DecodedMessage::from(&msg);
        assert_eq!(pm.mode, "acars");
        assert_eq!(pm.frequency_hz, 131_550_000);
        assert_eq!(pm.raw, vec![1, 2, 3]);
        let Some(asf2::decoded_message::Body::Acars(a)) = pm.body else {
            panic!("expected acars body");
        };
        assert_eq!(a.tail.as_deref(), Some("N471XG"));
        assert_eq!(a.text, "HELLO");

        // Envelope framing roundtrip.
        let env = batch(7, vec![asf2::DecodedMessage::from(&msg)]);
        let framed = frame_envelope(&env);
        let len = u32::from_be_bytes(framed[..4].try_into().unwrap()) as usize;
        assert_eq!(len, framed.len() - 4);
        use prost::Message as _;
        let back = asf2::Envelope::decode(&framed[4..]).unwrap();
        let Some(asf2::envelope::Kind::Batch(b)) = back.kind else {
            panic!("expected batch");
        };
        assert_eq!(b.seq, 7);
        assert_eq!(b.messages.len(), 1);
    }
}
