//! dumpvdl2 `decoded:json` output (FEED-2.1): re-encode an xng VDL2
//! ACARS-over-AVLC message into the nested `{vdl2:{…,avlc:{…,acars:{…}}}}`
//! object dumpvdl2 emits, so Airframes ingests it natively on UDP :5552.
//!
//! Field names + shapes are pinned to dumpvdl2 2.6.0 / libacars 2.2.1 output
//! captured on the vendored off-air fixture (see the golden in tests). The
//! AVLC link wrapper rides on the `#[serde(skip)]` transient `core.vdl2_link`
//! field (stashed by `xng_mode_vdl2::to_message`), so it never leaks into the
//! public outputs and survives multi-block reassembly. Non-ACARS AVLC/XID
//! frames are not yet emitted.

use serde_json::{json, Value};
use xng_types::{Message, MessageBody};

/// dumpvdl2 address-type label for an xng `AddressType` (serde snake_case).
fn addr_type(kind: &str) -> &'static str {
    match kind {
        "aircraft" => "Aircraft",
        "ground_icao" | "ground_delegated" => "Ground station",
        "all_stations" => "All stations",
        _ => "Reserved",
    }
}

/// Render a VDL2 message as dumpvdl2 `decoded:json`. Only ACARS-over-AVLC
/// frames (the Airframes-valuable content) are emitted today; `None` otherwise.
pub fn format_dumpvdl2(msg: &Message) -> Option<Value> {
    let MessageBody::Acars(core) = &msg.body else {
        return None;
    };
    if msg.mode != xng_types::Mode::Vdl2 {
        return None;
    }
    let link = core.vdl2_link.as_ref()?;
    let (src, dst, control) = (link.get("src")?, link.get("dst")?, link.get("control")?);

    // AVLC addresses. dumpvdl2 puts the A/G status (from the dst octet-1 bit)
    // on the transmitter (src); the C/R bit (src octet-5) gives Command/Response.
    let ag_on_ground = dst.get("status_bit").and_then(Value::as_bool).unwrap_or(false);
    let cr_response = src.get("status_bit").and_then(Value::as_bool).unwrap_or(false);
    let mut src_obj = json!({
        "addr": src.get("addr"),
        "type": addr_type(src.get("kind").and_then(Value::as_str).unwrap_or("")),
        "status": if ag_on_ground { "On ground" } else { "Airborne" },
    });
    let _ = &mut src_obj;
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
        "poll": poll,
    });
    let a = avlc.as_object_mut().unwrap();
    match ctype {
        "info" => {
            a.insert("frame_type".into(), json!("I"));
            a.insert("rseq".into(), control.get("nr").cloned().unwrap_or(json!(0)));
            a.insert("sseq".into(), control.get("ns").cloned().unwrap_or(json!(0)));
        }
        "supervisory" => {
            a.insert("frame_type".into(), json!("S"));
            a.insert("rseq".into(), control.get("nr").cloned().unwrap_or(json!(0)));
        }
        _ => {
            a.insert("frame_type".into(), json!("U"));
        }
    }

    // ACARS inner object — dumpvdl2/libacars field names + conventions.
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
    avlc.as_object_mut().unwrap().insert("acars".into(), Value::Object(acars));

    // Envelope. app name/ver are hardcoded to the dumpvdl2 wire identity (not
    // xng's) so the ingest recognizes the producer. Gap fields dumpvdl2 emits
    // but xng doesn't track (burst_len_octets / hdr_bits_fixed / idx /
    // noise_level) are omitted rather than faked.
    let ts = msg.timestamp;
    let mut vdl2 = json!({
        "app": { "name": "dumpvdl2", "ver": "2.6.0" },
        "t": { "sec": ts.timestamp(), "usec": ts.timestamp_subsec_micros() },
        "freq": msg.frequency_hz,
        "octets_corrected_by_fec": msg.decode.fec_corrected.unwrap_or(0),
        "avlc": avlc,
    });
    if let Some(l) = msg.signal.rssi_db {
        vdl2["sig_level"] = json!(l);
    }
    if let Some(s) = msg.signal.freq_skew_hz {
        vdl2["freq_skew"] = json!(s);
    }
    Some(json!({ "vdl2": vdl2 }))
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
                    out = format_dumpvdl2(&msg);
                }
            }
        }
        let v = out.expect("HB-IJW VDL2 ACARS serialized to dumpvdl2 JSON");
        let vdl2 = &v["vdl2"];
        assert_eq!(vdl2["app"]["name"], "dumpvdl2");
        assert_eq!(vdl2["app"]["ver"], "2.6.0");
        assert_eq!(vdl2["freq"], 136_975_000);
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
}

