//! dumpvdl2 `decoded:json` output (FEED-2.1): re-encode an xng VDL2
//! ACARS-over-AVLC or XID message into the nested
//! `{vdl2:{…,avlc:{…,acars|xid:{…}}}}` object dumpvdl2 emits, so Airframes
//! ingests it natively on UDP :5552.
//!
//! Field names + shapes are pinned to dumpvdl2 2.6.0 / libacars 2.2.1 output
//! captured on the vendored off-air fixture (see the golden in tests). The
//! AVLC link wrapper rides on the `#[serde(skip)]` transient `core.link`
//! field (stashed by `xng_mode_vdl2::to_message`), so it never leaks into the
//! public outputs and survives multi-block reassembly. XID frames are
//! re-encoded from xng's own XID parse; their JSON shapes are pinned by
//! black-box dumpvdl2 2.7.0 output (golden vectors in tests). Other
//! non-ACARS AVLC frames (S, ATN) are not yet emitted.

use serde_json::{json, Value};
use xng_types::{AcarsCore, Message, MessageBody};

/// dumpvdl2 address-type label for an xng `AddressType` (serde snake_case).
fn addr_type(kind: &str) -> &'static str {
    match kind {
        "aircraft" => "Aircraft",
        "ground_icao" | "ground_delegated" => "Ground station",
        "all_stations" => "All stations",
        _ => "Reserved",
    }
}

/// Render a VDL2 message as dumpvdl2 `decoded:json`, stamping `station` as
/// `vdl2.station` (the field the Airframes ingest reads as the station ident;
/// dumpvdl2 `--station-id`). ACARS-over-AVLC and XID frames (the
/// Airframes-valuable content) are emitted; `None` otherwise.
pub fn format_dumpvdl2(msg: &Message, station: Option<&str>) -> Option<Value> {
    if msg.mode != xng_types::Mode::Vdl2 {
        return None;
    }
    let (avlc, link) = match &msg.body {
        MessageBody::Acars(core) => (acars_avlc(msg, core)?, core.link.as_ref()?),
        MessageBody::Vdl2 { kind, details } if kind == "xid" => (xid_avlc(details)?, details),
        _ => return None,
    };
    let burst_len = link
        .get("burst_len_octets")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let hdr_fixed = link.get("hdr_bits_fixed").and_then(Value::as_u64);
    envelope(msg, avlc, burst_len, hdr_fixed, station)
}

/// The AVLC wrapper every frame shares: addresses, C/R, and the control field
/// — frame type plus sequence numbers and `poll` (I/S), or the command name
/// and `pf` (U).
fn avlc_header(src: &Value, dst: &Value, control: &Value) -> Value {
    // AVLC addresses. dumpvdl2 puts the A/G status (from the dst octet-1 bit)
    // on the transmitter (src); the C/R bit (src octet-5) gives Command/Response.
    let ag_on_ground = dst.get("status_bit").and_then(Value::as_bool).unwrap_or(false);
    let cr_response = src.get("status_bit").and_then(Value::as_bool).unwrap_or(false);
    let src_obj = json!({
        "addr": src.get("addr"),
        "type": addr_type(src.get("kind").and_then(Value::as_str).unwrap_or("")),
        "status": if ag_on_ground { "On ground" } else { "Airborne" },
    });
    let dst_obj = json!({
        "addr": dst.get("addr"),
        "type": addr_type(dst.get("kind").and_then(Value::as_str).unwrap_or("")),
    });

    // Control field → frame type + sequence numbers.
    let ctype = control.get("type").and_then(Value::as_str).unwrap_or("");
    let poll = control.get("poll").and_then(Value::as_bool).unwrap_or(false);
    let mut avlc = json!({
        "src": src_obj,
        "dst": dst_obj,
        "cr": if cr_response { "Response" } else { "Command" },
    });
    let a = avlc.as_object_mut().unwrap();
    match ctype {
        "info" => {
            a.insert("frame_type".into(), json!("I"));
            a.insert("rseq".into(), control.get("nr").cloned().unwrap_or(json!(0)));
            a.insert("sseq".into(), control.get("ns").cloned().unwrap_or(json!(0)));
            a.insert("poll".into(), json!(poll));
        }
        "supervisory" => {
            a.insert("frame_type".into(), json!("S"));
            a.insert("rseq".into(), control.get("nr").cloned().unwrap_or(json!(0)));
            a.insert("poll".into(), json!(poll));
        }
        _ => {
            a.insert("frame_type".into(), json!("U"));
            a.insert(
                "cmd".into(),
                control.get("kind").cloned().unwrap_or(json!("")),
            );
            a.insert("pf".into(), json!(poll));
        }
    }
    avlc
}

/// ACARS-over-AVLC: the AVLC wrapper plus the libacars-style `acars` object.
fn acars_avlc(msg: &Message, core: &AcarsCore) -> Option<Value> {
    let link = core.link.as_ref()?;
    let mut avlc = avlc_header(link.get("src")?, link.get("dst")?, link.get("control")?);

    let mut acars = libacars_object(msg, core);
    // Always present (empty when the block has no text), as dumpvdl2 emits
    // it; strict dumpvdl2-JSON parsers (acars_router) require the field.
    acars.entry("msg_text").or_insert_with(|| json!(""));
    avlc.as_object_mut()
        .unwrap()
        .insert("acars".into(), Value::Object(acars));
    Some(avlc)
}

/// `(sig_level, noise_level, freq_skew)` for the dumpvdl2/dumphfdl
/// envelopes, rounded to 0.1. The noise floor falls back to signal minus SNR
/// when only those were measured; a value that was never measured is 0.
pub(crate) fn signal_levels(msg: &Message) -> (f64, f64, f64) {
    let r = |v: f32| (v as f64 * 10.0).round() / 10.0;
    let s = &msg.signal;
    let sig = s.rssi_db.map(r).unwrap_or(0.0);
    let noise = s
        .noise_db
        .or_else(|| Some(s.rssi_db? - s.snr_db?))
        .map(r)
        .unwrap_or(0.0);
    (sig, noise, s.freq_skew_hz.map(r).unwrap_or(0.0))
}

/// The libacars `acars` object both dumpvdl2 and dumphfdl `decoded:json`
/// nest under their link layer: `{err, crc_ok, more, reg, mode, label,
/// blk_id, ack, flight, msg_num, msg_num_seq, msg_text}`.
pub(crate) fn libacars_object(msg: &Message, core: &AcarsCore) -> serde_json::Map<String, Value> {
    let reg = core.tail.as_deref().map(|t| {
        // ACARS registration is a 7-char dot-left-padded field; xng strips the
        // leading fill, so reconstruct it (".HB-IJW").
        let mut s = t.to_string();
        while s.len() < 7 {
            s.insert(0, '.');
        }
        s
    });
    // xng's combined 4-char msg number ("M06A") splits into the 3-char id +
    // the sequence char dumpvdl2 reports separately. Split on CHAR boundaries
    // (not byte offsets) so a garbled non-ASCII msg_num can never land mid-
    // codepoint and silently drop the whole field — ACARS msg nums are ASCII,
    // but the feed serializer must stay total on any input.
    let (msg_num, msg_num_seq) = match core.msg_num.as_deref() {
        Some(m) if m.chars().count() >= 4 => (
            Some(m.chars().take(3).collect::<String>()),
            Some(m.chars().skip(3).take(1).collect::<String>()),
        ),
        other => (other.map(str::to_string), None),
    };
    let mut acars = serde_json::Map::new();
    let errored = !msg.decode.crc_ok || msg.decode.errors.unwrap_or(0) > 0;
    acars.insert("err".into(), json!(errored));
    acars.insert("crc_ok".into(), json!(msg.decode.crc_ok));
    acars.insert("more".into(), json!(core.more_to_come));
    if let Some(r) = reg {
        acars.insert("reg".into(), json!(r));
    }
    acars.insert("mode".into(), json!(core.mode.to_string()));
    acars.insert("label".into(), json!(core.label));
    if let Some(b) = core.block_id {
        acars.insert("blk_id".into(), json!(b.to_string()));
    }
    // dumpvdl2 emits the ack char, or the literal "!" for a NAK.
    acars.insert("ack".into(), json!(core.ack.map(|c| c.to_string()).unwrap_or_else(|| "!".into())));
    if let Some(f) = &core.flight {
        acars.insert("flight".into(), json!(f));
    }
    if let Some(m) = msg_num {
        acars.insert("msg_num".into(), json!(m));
    }
    if let Some(s) = msg_num_seq {
        acars.insert("msg_num_seq".into(), json!(s));
    }
    if !core.text.is_empty() {
        acars.insert("msg_text".into(), json!(core.text));
    }
    acars
}

/// Wrap a rendered `avlc` object in dumpvdl2's `vdl2` envelope.
fn envelope(
    msg: &Message,
    avlc: Value,
    burst_len: u64,
    hdr_fixed: Option<u64>,
    station_id: Option<&str>,
) -> Option<Value> {
    // Envelope. app name/ver are hardcoded to the dumpvdl2 wire identity (not
    // xng's) so the ingest recognizes the producer. `sig_level`,
    // `noise_level` and `freq_skew` are always present: the Airframes
    // ingest's model requires all three and rejects the whole message when
    // one is missing. dumpvdl2 reports the carrier offset in ppm of the
    // channel frequency (dumphfdl's is in Hz).
    let ts = msg.timestamp;
    let (sig_level, noise_level, _) = signal_levels(msg);
    let freq_skew = match (msg.signal.freq_skew_hz, msg.frequency_hz) {
        (Some(s), f) if f > 0 => s as f64 / f as f64 * 1e6,
        _ => 0.0,
    };
    let mut vdl2 = json!({
        "app": { "name": "dumpvdl2", "ver": "2.6.0" },
        "t": { "sec": ts.timestamp(), "usec": ts.timestamp_subsec_micros() },
        "freq": msg.frequency_hz,
        "octets_corrected_by_fec": msg.decode.fec_corrected.unwrap_or(0),
        "sig_level": sig_level,
        "noise_level": noise_level,
        "freq_skew": freq_skew,
        // Required by strict dumpvdl2-JSON parsers (acars_router).
        "burst_len_octets": burst_len,
        "idx": msg.source.channel.as_ref().map_or(0, |c| c.index),
        "avlc": avlc,
    });
    if let Some(h) = hdr_fixed {
        vdl2["hdr_bits_fixed"] = json!(h);
    }
    if let Some(id) = station_id {
        vdl2["station"] = json!(id);
    }
    Some(json!({ "vdl2": vdl2 }))
}

/// XID: the AVLC wrapper plus dumpvdl2's `xid` object, re-encoded from
/// xng's parse (`details.params`, raw bytes in `params[].value_hex`).
fn xid_avlc(d: &Value) -> Option<Value> {
    let mut avlc = avlc_header(d.get("src")?, d.get("dst")?, d.get("control")?);
    let response = avlc["cr"] == "Response";
    let pf = avlc["pf"].as_bool().unwrap_or(false);
    let info = d
        .get("info_hex")
        .and_then(Value::as_str)
        .and_then(hex_bytes)?;
    let params = d.get("params").and_then(Value::as_array);
    avlc.as_object_mut()
        .unwrap()
        .insert("xid".into(), xid_object(response, pf, &info, params));
    Some(avlc)
}

fn hex_bytes(s: &str) -> Option<Vec<u8>> {
    (0..s.len())
        .step_by(2)
        .map(|i| s.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()))
        .collect()
}

/// XID format identifier (ISO 8885 general-purpose XID).
const XID_FMT_ID: u8 = 0x82;

/// The `xid` object. An information field that is not a well-formed
/// ISO 8885 XID (wrong format identifier, a length that overruns its group,
/// or a zero-length parameter) is reported the way dumpvdl2 does: `err` with
/// the raw octets under `unknown_proto`.
fn xid_object(response: bool, pf: bool, info: &[u8], params: Option<&Vec<Value>>) -> Value {
    let malformed = || json!({ "err": true, "unknown_proto": { "data": info } });
    let Some(params) = params else {
        return malformed();
    };
    if info.first() != Some(&XID_FMT_ID) {
        return malformed();
    }
    let mut pub_params = Vec::new();
    let mut vdl_params = Vec::new();
    let mut conn_mgmt = None;
    for p in params {
        let (Some(group), Some(id)) = (p["group"].as_u64(), p["id"].as_u64()) else {
            return malformed();
        };
        let Some(value) = p["value_hex"].as_str().and_then(hex_bytes) else {
            return malformed();
        };
        if value.is_empty() {
            return malformed();
        }
        let (group, id) = (group as u8, id as u8);
        if group == XID_GID_PRIVATE && id == 0x01 && conn_mgmt.is_none() {
            conn_mgmt = Some(value[0]);
        }
        let rendered = match group {
            XID_GID_PUBLIC => pub_param(id, &value),
            XID_GID_PRIVATE => vdl_param(id, &value),
            _ => None,
        };
        // Unknown parameter ids are dropped, as dumpvdl2 does.
        if let Some((name, v)) = rendered {
            let entry = json!({ "name": name, "value": v });
            if group == XID_GID_PUBLIC {
                pub_params.push(entry)
            } else {
                vdl_params.push(entry)
            }
        }
    }
    let (type_name, type_descr) = xid_type(response, pf, conn_mgmt);
    let mut x = json!({ "err": false, "type": type_name, "type_descr": type_descr });
    if !pub_params.is_empty() {
        x["pub_params"] = json!(pub_params);
    }
    x["vdl_params"] = json!(vdl_params);
    x
}

/// XID group identifiers (ISO 8885): public (HDLC) and VDL-private.
const XID_GID_PUBLIC: u8 = 0x80;
const XID_GID_PRIVATE: u8 = 0xF0;

/// XID frame type from C/R, P/F and the connection-management parameter's
/// h (bit 0) and r (bit 1) bits (ICAO Doc 9776 Table 5.12). Frames without
/// the parameter (GSIF, LPM) read as h = r = 1; unassigned combinations
/// carry empty names.
fn xid_type(response: bool, pf: bool, conn_mgmt: Option<u8>) -> (&'static str, &'static str) {
    let (h, r) = conn_mgmt.map_or((1, 1), |c| (c & 1, (c >> 1) & 1));
    match (response as u8) << 3 | (pf as u8) << 2 | h << 1 | r {
        1 => ("XID_CMD_LCR", "Link Connection Refused"),
        2 => ("XID_CMD_HO", "Handoff Request / Broadcast Handoff"),
        3 => ("GSIF", "Ground Station Information Frame"),
        4 => ("XID_CMD_LE", "Link Establishment"),
        6 => ("XID_CMD_HO", "Handoff Initiation"),
        7 => ("XID_CMD_LPM", "Link Parameter Modification"),
        12 => ("XID_RSP_LE", "Link Establishment Response"),
        13 => ("XID_RSP_LCR", "Link Connection Refused Response"),
        14 => ("XID_RSP_HO", "Handoff Response"),
        15 => ("XID_RSP_LPM", "Link Parameter Modification Response"),
        _ => ("", ""),
    }
}

fn ascii(v: &[u8]) -> Value {
    json!(String::from_utf8_lossy(v))
}

/// A structured parameter whose value has the wrong length.
fn unparseable(id: u8, v: &[u8]) -> Value {
    json!({ "typecode": id, "data": v })
}

/// Round an f32-computed quantity the way dumpvdl2 prints it (6 decimals).
fn f32_6(x: f32) -> f64 {
    ((x as f64) * 1e6).round() / 1e6
}

/// Modulation-support bitfield (bit 1 = VDL Mode 2, bit 2 = Mode 3).
fn modulations(bits: u8) -> Value {
    let mut m = Vec::new();
    if bits & 0x02 != 0 {
        m.push("VDL-M2, D8PSK, 31500 bps");
    }
    if bits & 0x04 != 0 {
        m.push("VDL-M3, D8PSK, 31500 bps");
    }
    json!(m)
}

/// 2-octet VDL2 frequency field: modulation nibble + 12-bit channel number,
/// `kHz = (raw + 10000) · 10`, rounded up to the 25 kHz raster.
fn vdl2_freq(v: &[u8]) -> Value {
    let raw = (u16::from_be_bytes([v[0], v[1]]) & 0x0FFF) as u32;
    let mut khz = (raw + 10_000) * 10;
    if khz % 25 != 0 {
        khz += 25 - khz % 25;
    }
    json!({ "freq_mhz": f32_6(khz as f32 / 1000.0), "modulation_support": modulations(v[0] >> 4) })
}

/// 24-bit address (hex) of a 4-octet AVLC address field.
fn avlc_addr(v: &[u8]) -> String {
    xng_mode_vdl2::avlc::parse_address(v).addr
}

/// 12-bit two's-complement latitude/longitude in 0.1° (3 octets).
fn location(v: &[u8]) -> Value {
    let w = (v[0] as u32) << 16 | (v[1] as u32) << 8 | v[2] as u32;
    let s12 = |x: u32| ((x as i32) << 20 >> 20) as f32 * 0.1;
    json!({ "lat": f32_6(s12(w >> 12)), "lon": f32_6(s12(w & 0xFFF)) })
}

fn lcr_cause(code: u8) -> Option<&'static str> {
    Some(match code {
        0x00 => "Bad local parameter",
        0x01 => "Out of link layer resources",
        0x02 => "Out of packet layer resources",
        0x03 => "Terrestrial network not available",
        0x04 => "Terrestrial network congestion",
        0x05 => "Cannot support autotune",
        0x06 => "Station cannot support initiating handoff",
        0x7F => "Other unspecified local reason",
        0x80 => "Bad global parameter",
        0x81 => "Protocol violation",
        0x82 => "Ground system out of resources",
        0xFF => "Other unspecified system reason",
        _ => return None,
    })
}

/// Public (ISO 8885 HDLC) parameter set, group 0x80.
fn pub_param(id: u8, v: &[u8]) -> Option<(&'static str, Value)> {
    let name = match id {
        0x01 => return Some(("param_set_id", ascii(v))),
        0x02 => "procedure_classes",
        0x03 => "hdlc_options",
        0x05 => "n1_downlink",
        0x06 => "n1_uplink",
        0x07 => "k_downlink",
        0x08 => "k_uplink",
        0x09 => "timer_t1_downlink",
        0x0A => "counter_n2",
        0x0B => "timer_t2",
        _ => return None,
    };
    Some((name, json!(v)))
}

/// VDL-private parameter set, group 0xF0 (ICAO Doc 9776).
fn vdl_param(id: u8, v: &[u8]) -> Option<(&'static str, Value)> {
    let addr_list = |v: &[u8]| {
        if v.len() % 4 == 0 {
            json!(v.chunks_exact(4).map(avlc_addr).collect::<Vec<_>>())
        } else {
            unparseable(id, v)
        }
    };
    let octets = |name| Some((name, json!(v)));
    Some(match id {
        0x00 => ("param_set_id", ascii(v)),
        0x01 => ("conn_mgmt", json!(v[0])),
        0x02 => return octets("sqp"),
        0x03 => ("xid_sequencing", json!({ "seq": v[0] & 0x07, "retry": v[0] >> 4 })),
        0x04 => return octets("avlc_specific_options"),
        0x05 => return octets("expedited_sn_connection"),
        0x06 => (
            "lcr_cause",
            if v.len() >= 3 {
                let mut c = json!({ "cause_code": v[0] });
                if let Some(d) = lcr_cause(v[0]) {
                    c["cause_descr"] = json!(d);
                }
                c["delay"] = json!(u16::from_be_bytes([v[1], v[2]]));
                if v.len() > 3 {
                    c["additional_data"] = json!(&v[3..]);
                }
                c
            } else {
                unparseable(id, v)
            },
        ),
        0x40 => ("autotune_freq", if v.len() >= 2 { vdl2_freq(v) } else { unparseable(id, v) }),
        0x41 => ("replacement_ground_stations", addr_list(v)),
        0x42 => return octets("timer_t4"),
        0x43 => return octets("mac_persistence"),
        0x44 => return octets("counter_m1"),
        0x45 => return octets("timer_tm2"),
        0x46 => return octets("timer_tg5"),
        0x47 => return octets("timer_t3min"),
        0x48 => ("gs_addr_filter", addr_list(v)),
        0x49 => return octets("broadcast_connection"),
        0x81 => ("modulation_support", modulations(v[0])),
        0x82 => ("alternate_ground_stations", addr_list(v)),
        0x83 => ("dst_airport", ascii(v)),
        0x84 => (
            "ac_location",
            if v.len() >= 4 {
                json!({ "loc": location(v), "alt": v[3] as u32 * 1000 })
            } else {
                unparseable(id, v)
            },
        ),
        0xC0 => (
            "freq_support_list",
            if v.len() % 6 == 0 {
                json!(v
                    .chunks_exact(6)
                    .map(|e| json!({ "gs_addr": avlc_addr(&e[2..6]), "gs_freq": vdl2_freq(&e[0..2]) }))
                    .collect::<Vec<_>>())
            } else {
                unparseable(id, v)
            },
        ),
        0xC1 => ("airport_coverage", ascii(v)),
        0xC3 => ("nearest_airport_id", ascii(v)),
        0xC4 => return octets("atn_router_nets"),
        0xC5 => ("system_mask", addr_list(v)),
        0xC6 => return octets("timer_tg3"),
        0xC7 => return octets("timer_tg4"),
        0xC8 => ("gs_location", if v.len() >= 3 { location(v) } else { unparseable(id, v) }),
        _ => return None,
    })
    .map(|(name, value)| {
        // Structured parameters with the wrong length keep dumpvdl2's
        // placeholder name.
        if value.get("typecode").is_some() && value.get("data").is_some() {
            ("__unparseable_tlv_tag", value)
        } else {
            (name, value)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_complex::Complex;

    // FEED-2.1: decode the vendored VDL2 off-air fixture and assert the
    // serialized dumpvdl2 JSON matches the captured dumpvdl2 2.6.0 output for
    // the HB-IJW downlink ACARS — an external-tool oracle, field-for-field on
    // the content (timestamps + xng-untracked metadata excluded).
    #[test]
    fn matches_dumpvdl2_golden_for_offair_acars() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/crates/xng-mode-vdl2/tests/data/vdl2_offair_6s.i16"
        );
        let raw = std::fs::read(path).expect("fixture present");
        let samples: Vec<Complex<f32>> = raw
            .chunks_exact(4)
            .map(|b| {
                Complex::new(
                    i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
                    i16::from_le_bytes([b[2], b[3]]) as f32 / 32768.0,
                )
            })
            .collect();
        let prov = xng_types::Provenance {
            station: xng_types::StationIdentity::new("XX-TEST"),
            app: xng_types::AppInfo::xng(),
            sdr: None,
            channel: None,
        };
        let mut dec = xng_mode_vdl2::Vdl2ChannelDecoder::new(50_000.0, 0.0).unwrap();
        let mut out = None;
        for chunk in samples.chunks(65_536) {
            for f in dec.process(chunk) {
                let is_hbijw =
                    f.acars.as_ref().and_then(|b| b.core.tail.as_deref()) == Some("HB-IJW");
                if is_hbijw && out.is_none() {
                    let msg = xng_mode_vdl2::to_message(&f, 136_975_000, -10.77, prov.clone());
                    out = Some(msg);
                }
            }
        }
        let msg = out.expect("HB-IJW VDL2 ACARS decoded");
        let v = format_dumpvdl2(&msg, None).expect("HB-IJW VDL2 ACARS serialized to dumpvdl2 JSON");
        let vdl2 = &v["vdl2"];
        assert_eq!(vdl2["app"]["name"], "dumpvdl2");
        assert_eq!(vdl2["app"]["ver"], "2.6.0");
        assert_eq!(vdl2["freq"], 136_975_000);
        assert!(
            vdl2.get("station").is_none(),
            "no station unless one is given"
        );
        let stamped = format_dumpvdl2(&msg, Some("XX-KSEA-VDL2")).unwrap();
        assert_eq!(stamped["vdl2"]["station"], "XX-KSEA-VDL2");
        // The Airframes ingest requires the three level fields; dumpvdl2
        // gives the carrier offset in ppm.
        assert!(vdl2["sig_level"].is_number(), "{vdl2}");
        assert!(vdl2["noise_level"].is_number(), "{vdl2}");
        let skew_ppm = msg.signal.freq_skew_hz.unwrap() as f64 / 136.975;
        assert!(
            (vdl2["freq_skew"].as_f64().unwrap() - skew_ppm).abs() < 1e-9,
            "{vdl2}"
        );
        // Strict dumpvdl2-JSON parsers (acars_router) require these.
        assert!(vdl2["burst_len_octets"].as_u64().unwrap() > 0, "{vdl2}");
        assert!(vdl2["idx"].is_number(), "{vdl2}");
        assert!(vdl2["hdr_bits_fixed"].is_number(), "{vdl2}");
        let avlc = &vdl2["avlc"];
        assert_eq!(avlc["src"]["addr"], "0468D2", "{avlc}");
        assert_eq!(avlc["src"]["type"], "Aircraft");
        assert_eq!(avlc["src"]["status"], "Airborne");
        assert_eq!(avlc["dst"]["type"], "Ground station");
        assert_eq!(avlc["cr"], "Command");
        assert_eq!(avlc["frame_type"], "I");
        assert_eq!(avlc["rseq"], 1);
        assert_eq!(avlc["sseq"], 1);
        assert_eq!(avlc["poll"], false);
        let acars = &avlc["acars"];
        assert_eq!(acars["crc_ok"], true);
        assert_eq!(acars["reg"], ".HB-IJW", "leading-dot reg reconstructed");
        assert_eq!(acars["mode"], "2");
        assert_eq!(acars["label"], "B9");
        assert_eq!(acars["blk_id"], "9");
        assert_eq!(acars["ack"], "!", "NAK renders as \"!\"");
        assert_eq!(acars["flight"], "LX072K");
        assert_eq!(acars["msg_num"], "M06", "4-char min split to 3-char id");
        assert_eq!(acars["msg_num_seq"], "A");
        assert_eq!(acars["msg_text"], "/EHAM.TI2/040EHAMACFFA");
    }

    // XID: every golden frame (61 synthetic covering each parameter type,
    // wrong-length and malformed cases and every C/R x P/F x conn-mgmt
    // combination, plus 10 off-air) goes through the real AVLC bit-level
    // parse and message build; the U-frame header and `xid` object must equal
    // black-box dumpvdl2 2.7.0 output for the same frame.
    #[test]
    fn matches_dumpvdl2_golden_for_xid() {
        let prov = xng_types::Provenance {
            station: xng_types::StationIdentity::new("XX-TEST"),
            app: xng_types::AppInfo::xng(),
            sdr: None,
            channel: None,
        };
        let mut n = 0;
        for line in include_str!("testdata/dumpvdl2_xid.jsonl").lines() {
            let g: Value = serde_json::from_str(line).unwrap();
            let octets = hex_bytes(g["frame"].as_str().unwrap()).unwrap();
            let mut frames = xng_mode_vdl2::avlc::scan(&xng_mode_vdl2::avlc::build(&[octets]));
            assert_eq!(frames.len(), 1, "{}", g["frame"]);
            let f = xng_mode_vdl2::Vdl2Frame {
                avlc: frames.remove(0),
                acars: None,
                atn: None,
                rs_corrected: 0,
                freq_skew_hz: 0.0,
                snr_db: 0.0,
                burst_len_octets: 0,
                hdr_bits_fixed: 0,
                noise_dbfs: 0.0,
            };
            let msg = xng_mode_vdl2::to_message(&f, 136_975_000, -10.0, prov.clone());
            let v = format_dumpvdl2(&msg, None).expect("XID serialized");
            let avlc = &v["vdl2"]["avlc"];
            assert_eq!(avlc["frame_type"], "U");
            assert_eq!(avlc["cmd"], "XID");
            assert_eq!(avlc["cr"], g["cr"], "{}", g["frame"]);
            assert_eq!(avlc["pf"], g["pf"], "{}", g["frame"]);
            assert_eq!(avlc["xid"], g["xid"], "{} {}", g["src"], g["frame"]);
            n += 1;
        }
        assert_eq!(n, 71);
    }
}
