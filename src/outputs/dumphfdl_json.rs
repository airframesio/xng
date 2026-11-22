//! dumphfdl `decoded:json` output (FEED-2.2): re-encode xng HFDL messages
//! into the `{hfdl:{…,lpdu:{…,hfnpdu:{…}}}}` object dumphfdl emits, so the
//! Airframes HFDL ingest (`feed.airframes.io:5556`, newline-delimited JSON
//! over TCP) takes them natively.
//!
//! Two kinds are emitted:
//! - ACARS (HFNPDU 0xFF "Enveloped data", the libacars `acars` object), with
//!   the MPDU/LPDU addressing from the transient `core.link` field stashed by
//!   `xng_mode_hfdl::to_message`;
//! - performance data (HFNPDU 0xD1, 209), which carries the flight id and the
//!   aircraft position.
//!
//! Field names follow dumphfdl 1.x. `sig_level`, `noise_level`, `freq_skew`
//! and `bit_rate` are always present because the ingest's model requires
//! them. Other kinds (squitters, logons, frequency data, system tables) need
//! the system table to name their frequencies and are not sent yet.

use serde_json::{json, Map, Value};
use xng_types::{Message, MessageBody, Mode};

use crate::outputs::dumpvdl2_json::{libacars_object, signal_levels};

/// Render an HFDL message as one dumphfdl `decoded:json` object, stamping
/// `station` as `hfdl.station` (dumphfdl `--station-id`, the ident the
/// Airframes ingest credits). `None` for kinds that aren't sent.
pub fn format_dumphfdl(msg: &Message, station: Option<&str>) -> Option<Value> {
    if msg.mode != Mode::Hfdl {
        return None;
    }
    let (who, bps, hfnpdu) = match &msg.body {
        MessageBody::Acars(core) => {
            let link = core.link.as_ref()?;
            let hfnpdu = json!({
                "err": false,
                "type": { "id": 255, "name": "Enveloped data" },
                "acars": Value::Object(libacars_object(msg, core)),
            });
            (
                link.get("who")?.clone(),
                link.get("bps").and_then(Value::as_u64),
                hfnpdu,
            )
        }
        MessageBody::Hfdl { kind, details } if kind == "performance-data" => (
            details.get("who")?.clone(),
            details.get("bps").and_then(Value::as_u64),
            performance_data(details)?,
        ),
        _ => return None,
    };

    let (src, dst, ac_info) = addresses(&who)?;
    let mut lpdu = json!({
        "err": false,
        "src": src,
        "dst": dst,
        "type": { "id": 13, "name": "Unnumbered data" },
        "hfnpdu": hfnpdu,
    });
    if let Some(ac) = ac_info {
        lpdu["ac_info"] = ac;
    }

    let ts = msg.timestamp;
    let (sig_level, noise_level, freq_skew) = signal_levels(msg);
    let mut hfdl = json!({
        "app": { "name": "xng", "ver": env!("CARGO_PKG_VERSION") },
        "t": { "sec": ts.timestamp(), "usec": ts.timestamp_subsec_micros() },
        "freq": msg.frequency_hz,
        "bit_rate": bps.unwrap_or(0),
        "sig_level": sig_level,
        "noise_level": noise_level,
        "freq_skew": freq_skew,
        "lpdu": lpdu,
    });
    if let Some(st) = station {
        hfdl["station"] = json!(st);
    }
    Some(json!({ "hfdl": hfdl }))
}

/// dumphfdl `src`/`dst` entities and `ac_info` from the parser's `who`
/// (`{dir, gs_id, gs_name, aircraft_id | aircraft[], icao?}`).
fn addresses(who: &Value) -> Option<(Value, Value, Option<Value>)> {
    let gs_id = who.get("gs_id")?.as_u64()?;
    let mut gs = json!({ "type": "Ground station", "id": gs_id });
    if let Some(name) = who.get("gs_name").and_then(Value::as_str) {
        gs["name"] = json!(name);
    }
    let ac_info = who
        .get("icao")
        .and_then(Value::as_str)
        .map(|icao| json!({ "icao": icao }));
    match who.get("dir").and_then(Value::as_str)? {
        "downlink" => {
            let ac = json!({ "type": "Aircraft", "id": who.get("aircraft_id")?.as_u64()? });
            Some((ac, gs, ac_info))
        }
        "uplink" => {
            // An uplink MPDU can address several aircraft; dumphfdl names the
            // LPDU's own destination. With one aircraft that is unambiguous.
            let id = match who.get("aircraft").and_then(Value::as_array) {
                Some(list) if list.len() == 1 => list[0].get("aircraft_id").and_then(Value::as_u64),
                _ => None,
            };
            let mut ac = json!({ "type": "Aircraft" });
            if let Some(id) = id {
                ac["id"] = json!(id);
            }
            Some((gs, ac, ac_info))
        }
        _ => None,
    }
}

/// HFNPDU 209 (performance data) in dumphfdl's field names, from the xng
/// `performance-data` details.
fn performance_data(d: &Value) -> Option<Value> {
    let mut h = Map::new();
    h.insert("err".into(), json!(false));
    h.insert(
        "type".into(),
        json!({ "id": 209, "name": "Performance data" }),
    );
    h.insert("version".into(), d.get("version")?.clone());
    h.insert("flight_leg_num".into(), d.get("flight_leg")?.clone());
    if let Some(f) = d
        .get("flight")
        .and_then(Value::as_str)
        .filter(|f| !f.is_empty())
    {
        h.insert("flight_id".into(), json!(f));
    }
    // Only a real fix (the parser adds `position` when lat/lon are valid).
    if d.get("position").is_some() {
        if let (Some(lat), Some(lon)) = (d.get("lat"), d.get("lon")) {
            h.insert("pos".into(), json!({ "lat": lat, "lon": lon }));
        }
    }
    if let Some(utc) = d.get("utc") {
        h.insert("time".into(), utc.clone());
    }
    if let Some(gs) = d.get("gs_id") {
        h.insert("gs".into(), json!({ "type": "Ground station", "id": gs }));
    }
    if let Some(c) = d.get("freq_search_cnt") {
        h.insert("freq_search_cnt".into(), c.clone());
    }
    Some(Value::Object(h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use xng_types::*;

    fn prov() -> Provenance {
        Provenance {
            station: StationIdentity::new("XX-TEST"),
            app: AppInfo::xng(),
            sdr: None,
            channel: None,
        }
    }

    fn msg(body: MessageBody) -> Message {
        Message {
            mode: Mode::Hfdl,
            timestamp: Utc.with_ymd_and_hms(2026, 11, 22, 12, 0, 0).unwrap(),
            frequency_hz: 8_927_000,
            signal: SignalQuality {
                rssi_db: Some(-23.14),
                snr_db: Some(12.0),
                ..Default::default()
            },
            decode: DecodeQuality {
                crc_ok: true,
                fec_corrected: None,
                errors: Some(0),
            },
            body,
            raw: None,
            source: prov(),
        }
    }

    #[test]
    fn acars_downlink_matches_dumphfdl_shape() {
        let core = AcarsCore {
            mode: '2',
            tail: Some("N12345".into()),
            label: "H1".into(),
            block_id: Some('5'),
            ack: None,
            flight: Some("UA0001".into()),
            msg_num: Some("D01A".into()),
            text: "#M1BPOS".into(),
            link: Some(json!({
                "who": { "dir": "downlink", "gs_id": 7, "gs_name": "Agana, Guam", "aircraft_id": 56, "icao": "A1B2C3" },
                "bps": 1800,
            })),
            ..Default::default()
        };
        let v = format_dumphfdl(&msg(MessageBody::Acars(core)), Some("XX-TEST-HFDL")).unwrap();
        let h = &v["hfdl"];
        assert_eq!(h["station"], "XX-TEST-HFDL");
        assert_eq!(h["freq"], 8_927_000);
        assert_eq!(h["bit_rate"], 1800);
        assert_eq!(h["sig_level"], -23.1);
        assert_eq!(h["noise_level"], -35.1, "rssi - snr");
        assert_eq!(h["freq_skew"], 0.0);
        assert_eq!(h["t"]["sec"], 1_795_348_800);
        let l = &h["lpdu"];
        assert_eq!(l["err"], false);
        assert_eq!(l["src"], json!({ "type": "Aircraft", "id": 56 }));
        assert_eq!(l["dst"]["type"], "Ground station");
        assert_eq!(l["dst"]["id"], 7);
        assert_eq!(l["ac_info"]["icao"], "A1B2C3");
        let n = &l["hfnpdu"];
        assert_eq!(n["type"]["id"], 255);
        let a = &n["acars"];
        assert_eq!(a["reg"], ".N12345");
        assert_eq!(a["label"], "H1");
        assert_eq!(a["flight"], "UA0001");
        assert_eq!(a["msg_num"], "D01");
        assert_eq!(a["msg_num_seq"], "A");
        assert_eq!(a["msg_text"], "#M1BPOS");
        assert_eq!(a["ack"], "!");
    }

    #[test]
    fn uplink_puts_ground_station_first() {
        let core = AcarsCore {
            mode: '2',
            label: "SA".into(),
            link: Some(json!({
                "who": { "dir": "uplink", "gs_id": 3, "aircraft": [{ "aircraft_id": 12, "lpdus": 1 }] },
                "bps": 600,
            })),
            ..Default::default()
        };
        let v = format_dumphfdl(&msg(MessageBody::Acars(core)), None).unwrap();
        let l = &v["hfdl"]["lpdu"];
        assert_eq!(l["src"]["type"], "Ground station");
        assert_eq!(l["dst"], json!({ "type": "Aircraft", "id": 12 }));
        assert!(v["hfdl"].get("station").is_none());
    }

    #[test]
    fn performance_data_carries_flight_and_position() {
        let details = json!({
            "flight": "QF0012", "lat": -33.9, "lon": 151.2,
            "utc": { "hour": 11, "min": 59, "sec": 30 },
            "version": 1, "flight_leg": 2, "gs_id": 7,
            "freq_search_cnt": { "prev_leg": 0, "cur_leg": 3 },
            "position": { "lat": -33.9, "lon": 151.2 },
            "who": { "dir": "downlink", "gs_id": 7, "aircraft_id": 9 },
        });
        let m = msg(MessageBody::Hfdl {
            kind: "performance-data".into(),
            details,
        });
        let v = format_dumphfdl(&m, Some("S")).unwrap();
        let n = &v["hfdl"]["lpdu"]["hfnpdu"];
        assert_eq!(n["type"]["id"], 209);
        assert_eq!(n["flight_id"], "QF0012");
        assert_eq!(n["flight_leg_num"], 2);
        assert_eq!(n["pos"], json!({ "lat": -33.9, "lon": 151.2 }));
        assert_eq!(n["time"]["min"], 59);
        assert_eq!(n["freq_search_cnt"]["cur_leg"], 3);
    }

    #[test]
    fn other_kinds_and_modes_are_not_sent() {
        let m = msg(MessageBody::Hfdl {
            kind: "squitter".into(),
            details: json!({}),
        });
        assert!(format_dumphfdl(&m, None).is_none());
        let mut m = msg(MessageBody::Acars(AcarsCore::default()));
        assert!(format_dumphfdl(&m, None).is_none(), "no link wrapper");
        m.mode = Mode::Vdl2;
        assert!(format_dumphfdl(&m, None).is_none());
    }
}
