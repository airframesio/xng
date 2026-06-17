//! Inbound AIVDM/AIVDO reassembly and per-MMSI aggregation (AIS-2).
//!
//! The native RF path in this crate deframes one complete HDLC burst at a
//! time, so a long message that occupies several AIS slots already arrives as
//! one bit string. The *interchange* form of AIS — the AIVDM/AIVDO NMEA
//! sentences that every other tool speaks (and that AIS-Catcher feeds to the
//! HTTP path) — instead splits a long message across several sentences
//! (`!AIVDM,2,1,...` / `!AIVDM,2,2,...`). This module is the inverse of the
//! [`crate::nmea`] encoder: it parses those sentences, joins the fragments of
//! one message back into a single bit string, and (optionally) folds the
//! per-message fields into a per-MMSI [`AisTracker`] so a type-24 Part A and
//! Part B, or a stream of type-5 voyage reports, collapse into one vessel
//! record.
//!
//! Reassembly is keyed on `(channel, total, seq)` and accepts fragments in
//! any order — matching the reference behaviour of pyais
//! (`NMEAMessage.assemble_from_iterable`, `decode_out_of_order`). The 6-bit
//! ASCII de-armoring, fill-bit accounting and field offsets are anchored to
//! the pyais (MIT) decode oracle; no pyais code was copied.

use crate::fields;
use serde_json::{Map, Value};
use std::collections::HashMap;

/// A single parsed AIVDM/AIVDO sentence (one fragment of a message).
#[derive(Debug, Clone, PartialEq)]
pub struct Sentence {
    /// Total number of fragments this message is split into.
    pub total: u8,
    /// 1-based index of this fragment.
    pub num: u8,
    /// Sequential message id linking the fragments (empty for single-part).
    pub seq: Option<u8>,
    /// AIS channel designator ('A'/'B'), or `None` when the field is blank.
    pub channel: Option<char>,
    /// 6-bit-armored payload characters.
    pub payload: String,
    /// Fill bits to drop from the end of this fragment's bit expansion.
    pub fill: u8,
}

/// De-armor one payload character to its 6-bit value (IEC 61162-1).
fn dearmor(c: u8) -> u8 {
    let v = c.wrapping_sub(48);
    if v > 39 { v - 8 } else { v }
}

/// Parse one NMEA sentence line into a [`Sentence`].
///
/// Accepts the AIS sentence formatters used in the wild — `AIVDM` (received),
/// `AIVDO` (own-ship), and talker-prefixed variants such as `BSVDM`/`ARVDM`
/// (matching pyais, which keys only on the `VDM`/`VDO` suffix). The leading
/// `!`/`$`, any `\...\` tag block, and the `*CS` checksum suffix are tolerated
/// but not required. Returns `None` if the line is not a well-formed AIS
/// sentence.
pub fn parse_sentence(line: &str) -> Option<Sentence> {
    let line = line.trim();
    // Drop an optional leading NMEA tag block: \...\!AIVDM,...
    let line = match line.strip_prefix('\\') {
        Some(rest) => rest.rsplit_once('\\').map(|(_, s)| s).unwrap_or(rest),
        None => line,
    };
    let line = line.trim_start_matches(['!', '$']);
    // Strip the checksum suffix (*HH) if present.
    let body = line.split('*').next()?;
    let mut f = body.split(',');
    let formatter = f.next()?;
    // Must be an AIS VDM/VDO sentence (talker prefix is ignored).
    if !(formatter.ends_with("VDM") || formatter.ends_with("VDO")) {
        return None;
    }
    let total: u8 = f.next()?.parse().ok()?;
    let num: u8 = f.next()?.parse().ok()?;
    let seq = match f.next()? {
        "" => None,
        s => Some(s.parse().ok()?),
    };
    let channel = match f.next()? {
        "" => None,
        c => c.chars().next(),
    };
    let payload = f.next()?.to_string();
    let fill: u8 = f.next()?.parse().ok()?;
    if total == 0 || num == 0 || num > total {
        return None;
    }
    Some(Sentence { total, num, seq, channel, payload, fill })
}

/// Expand armored payload characters to message bits (6 bits each).
fn payload_bits(payload: &str) -> Vec<u8> {
    payload
        .bytes()
        .flat_map(|c| {
            let v = dearmor(c);
            (0..6).rev().map(move |i| (v >> i) & 1)
        })
        .collect()
}

/// A message reassembled from one or more sentences.
#[derive(Debug, Clone, PartialEq)]
pub struct ReassembledMessage {
    /// Channel designator from the fragments (first non-blank wins).
    pub channel: Option<char>,
    /// Complete message bit string (fill bits already removed), MSB-first.
    pub bits: Vec<u8>,
    /// All sentence lines that composed this message, in fragment order.
    pub sentences: Vec<String>,
}

impl ReassembledMessage {
    /// Message type (bits 0..6), or `None` if too short.
    pub fn msg_type(&self) -> Option<u8> {
        if self.bits.len() < 6 {
            return None;
        }
        Some(self.bits[..6].iter().fold(0u8, |v, &b| (v << 1) | b))
    }

    /// Source MMSI (bits 8..38), or `None` if too short.
    pub fn mmsi(&self) -> Option<u32> {
        if self.bits.len() < 38 {
            return None;
        }
        Some(self.bits[8..38].iter().fold(0u32, |v, &b| (v << 1) | b as u32))
    }

    /// Field-decode the reassembled message via [`crate::fields::decode`].
    pub fn decode(&self) -> Option<Value> {
        fields::decode(self.msg_type()?, &self.bits)
    }
}

/// In-progress multi-fragment accumulator for one `(channel, total, seq)` key.
#[derive(Debug, Clone)]
struct Pending {
    /// Per-fragment armored payloads, indexed by `num - 1`; `None` until seen.
    parts: Vec<Option<String>>,
    /// Fill bits declared by the *last* fragment.
    fill: u8,
    channel: Option<char>,
    sentences: Vec<Option<String>>,
    /// Count of distinct fragments received so far.
    have: usize,
}

/// Stateful multi-fragment AIVDM reassembler.
///
/// Feed each sentence line in; a complete message is returned as soon as its
/// final missing fragment arrives. Single-fragment sentences pass straight
/// through. Out-of-order and interleaved (different `seq`) fragment streams
/// are handled — partial messages stay buffered, keyed independently.
#[derive(Default)]
pub struct SentenceReassembler {
    pending: HashMap<(Option<char>, u8, Option<u8>), Pending>,
}

impl SentenceReassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse and feed one sentence line; returns the message when complete.
    pub fn push_line(&mut self, line: &str) -> Option<ReassembledMessage> {
        let s = parse_sentence(line)?;
        self.push(s, line)
    }

    /// Feed an already-parsed sentence (keeping its original line text).
    pub fn push(&mut self, s: Sentence, line: &str) -> Option<ReassembledMessage> {
        if s.total == 1 {
            let mut bits = payload_bits(&s.payload);
            bits.truncate(bits.len().saturating_sub(s.fill as usize));
            return Some(ReassembledMessage {
                channel: s.channel,
                bits,
                sentences: vec![line.to_string()],
            });
        }

        let key = (s.channel, s.total, s.seq);
        let total = s.total as usize;
        let idx = (s.num - 1) as usize;
        let entry = self.pending.entry(key).or_insert_with(|| Pending {
            parts: vec![None; total],
            fill: 0,
            channel: None,
            sentences: vec![None; total],
            have: 0,
        });
        if entry.parts[idx].is_none() {
            entry.have += 1;
        }
        entry.parts[idx] = Some(s.payload);
        entry.sentences[idx] = Some(line.to_string());
        // Channel: first non-blank fragment wins.
        if entry.channel.is_none() {
            entry.channel = s.channel;
        }
        // Fill bits belong to the final fragment.
        if s.num as usize == total {
            entry.fill = s.fill;
        }
        if entry.have < total {
            return None;
        }

        let entry = self.pending.remove(&key)?;
        let mut payload = String::new();
        for p in entry.parts.iter() {
            payload.push_str(p.as_deref()?);
        }
        let mut bits = payload_bits(&payload);
        bits.truncate(bits.len().saturating_sub(entry.fill as usize));
        let sentences: Vec<String> = entry.sentences.into_iter().flatten().collect();
        Some(ReassembledMessage { channel: entry.channel, bits, sentences })
    }

    /// Number of partially-received messages still buffered.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

/// Per-MMSI aggregated vessel record (AIS-2 tracking).
///
/// Folds the static/voyage/identity fields seen across messages from one MMSI
/// into a single object — most importantly merging a type-24 **Part A**
/// (vessel name) with a type-24 **Part B** (type, callsign, dimensions or
/// mothership), and successive type-5 voyage reports, into one record. The
/// merge rule matches pyais's tracker (`update_track`): a field present in a
/// newer message overwrites the stored value; fields absent from the newer
/// message are preserved. Volatile per-report kinematics are intentionally not
/// carried here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VesselRecord {
    pub mmsi: u32,
    fields: Map<String, Value>,
}

/// Identity / static fields carried into the per-MMSI record. Position and
/// other volatile per-message kinematics are deliberately excluded so the
/// record reflects stable vessel identity, not a momentary fix.
const STATIC_FIELDS: &[&str] = &[
    "name", "callsign", "imo", "ship_type", "destination", "draught_m", "eta",
    "to_bow", "to_stern", "to_port", "to_starboard", "epfd", "ais_version",
    "dte_ready", "vendor_id", "model", "serial", "mothership_mmsi", "aton_type",
];

impl VesselRecord {
    fn new(mmsi: u32) -> Self {
        Self { mmsi, fields: Map::new() }
    }

    /// Merge the decoded fields of one message into the record.
    fn merge(&mut self, decoded: &Value) {
        let Some(obj) = decoded.as_object() else { return };
        for &k in STATIC_FIELDS {
            if let Some(v) = obj.get(k) {
                if !v.is_null() {
                    self.fields.insert(k.to_string(), v.clone());
                }
            }
        }
    }

    /// The merged static fields as a JSON object.
    pub fn as_json(&self) -> Value {
        Value::Object(self.fields.clone())
    }

    /// Look up one merged field.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.get(key)
    }
}

/// Per-MMSI aggregator over decoded AIS messages.
///
/// Type-24 A/B and type-5 voyage records for the same MMSI accumulate into one
/// [`VesselRecord`]; querying by MMSI returns the merged identity.
#[derive(Default)]
pub struct AisTracker {
    vessels: HashMap<u32, VesselRecord>,
}

impl AisTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the tracker from one decoded message.
    ///
    /// `mmsi` is the source MMSI; `decoded` is the output of
    /// [`crate::fields::decode`] for that message. Static/identity fields are
    /// merged into the MMSI's record; the updated record is returned.
    pub fn update(&mut self, mmsi: u32, decoded: &Value) -> &VesselRecord {
        let rec = self.vessels.entry(mmsi).or_insert_with(|| VesselRecord::new(mmsi));
        rec.merge(decoded);
        rec
    }

    /// Update directly from a [`ReassembledMessage`] (decodes it first).
    pub fn update_message(&mut self, msg: &ReassembledMessage) -> Option<&VesselRecord> {
        let mmsi = msg.mmsi()?;
        let decoded = msg.decode()?;
        Some(self.update(mmsi, &decoded))
    }

    /// The merged record for an MMSI, if any messages have been seen.
    pub fn get(&self, mmsi: u32) -> Option<&VesselRecord> {
        self.vessels.get(&mmsi)
    }

    /// Number of distinct MMSIs tracked.
    pub fn len(&self) -> usize {
        self.vessels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vessels.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Oracle: pyais 3.1.0 decode of the same sentences (2026-06-16).
    // Multi-fragment vectors are taken verbatim from the pyais test suite
    // (tests/test_decode.py): test_msg_type_5, test_msg_type_8_multipart,
    // the two-fragment type-21 (line ~812), test_msg_type_6_very_large,
    // test_decode_out_of_order, test_byte_stream / test_multiline_message,
    // and the canonical gpsd type-24 Part A/Part B pair (MMSI 271041815).
    // Asserted field values were produced by running pyais on this machine.
    // ------------------------------------------------------------------

    fn reassemble(lines: &[&str]) -> ReassembledMessage {
        let mut r = SentenceReassembler::new();
        let mut out = None;
        for l in lines {
            if let Some(m) = r.push_line(l) {
                out = Some(m);
            }
        }
        out.expect("message should have completed")
    }

    #[test]
    fn parse_basic_sentence() {
        let s = parse_sentence("!AIVDM,2,1,4,A,55O0W7,0*08").unwrap();
        assert_eq!(s.total, 2);
        assert_eq!(s.num, 1);
        assert_eq!(s.seq, Some(4));
        assert_eq!(s.channel, Some('A'));
        assert_eq!(s.payload, "55O0W7");
        assert_eq!(s.fill, 0);
    }

    #[test]
    fn parse_blank_seq_and_channel() {
        // AIVDO own-ship with blank seq, blank channel (pyais test_empty_channel).
        let s = parse_sentence("!AIVDO,1,1,,,B>qc:003wk?8mP=18D3Q3wgTiT;T,0*13").unwrap();
        assert_eq!(s.seq, None);
        assert_eq!(s.channel, None);
        assert_eq!(s.total, 1);
    }

    #[test]
    fn parse_talker_variants_and_tagblock() {
        // BSVDM (base station), ARVDM (repeater), plus a leading tag block.
        assert!(parse_sentence("!BSVDM,1,1,,B,83m;Fa,0*11").is_some());
        assert!(parse_sentence("!ARVDM,2,1,3,B,E>m1c1,0*5C").is_some());
        let s = parse_sentence("\\g:1-2-73874*A\\!AIVDM,1,1,,A,15M67F,0*5C").unwrap();
        assert_eq!(s.payload, "15M67F");
        // Non-AIS sentences are rejected.
        assert!(parse_sentence("$GPGGA,123519,4807.038,N,01131.000,E,1*47").is_none());
    }

    #[test]
    fn single_fragment_passthrough() {
        // Type 1 (gpsd canonical) — must field-decode identically to a frame.
        let m = reassemble(&["!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0*5C"]);
        assert_eq!(m.msg_type(), Some(1));
        assert_eq!(m.mmsi(), Some(477553000));
        let d = m.decode().unwrap();
        assert_eq!(d["nav_status"], "moored");
    }

    #[test]
    fn type5_two_fragments_match_pyais() {
        let m = reassemble(&[
            "!AIVDM,2,1,1,A,55?MbV02;H;s<HtKR20EHE:0@T4@Dn2222222216L961O5Gf0NSQEp6ClRp8,0*1C",
            "!AIVDM,2,2,1,A,88888888880,2*25",
        ]);
        assert_eq!(m.msg_type(), Some(5));
        assert_eq!(m.mmsi(), Some(351759000));
        assert_eq!(m.bits.len(), 424);
        let d = m.decode().unwrap();
        assert_eq!(d["imo"], 9134270);
        assert_eq!(d["callsign"], "3FOF8");
        assert_eq!(d["name"], "EVER DIADEM");
        assert_eq!(d["ship_type"], 70);
        assert_eq!(d["to_bow"], 225);
        assert_eq!(d["to_stern"], 70);
        assert_eq!(d["to_port"], 1);
        assert_eq!(d["to_starboard"], 31);
        assert_eq!(d["draught_m"], 12.2);
        assert_eq!(d["destination"], "NEW YORK");
        assert_eq!(d["epfd"], "GPS");
        assert_eq!(d["dte_ready"], true); // dte bit 0 = ready
        assert_eq!(d["eta"], "05-15T14:00");
    }

    #[test]
    fn type8_two_fragments_match_pyais() {
        // pyais test_msg_type_8_multipart: DAC=0/FID=0, MMSI 888888888.
        let m = reassemble(&[
            "!AIVDO,2,1,,A,8=?eN>0000:C=4B1KTTsgLoUelGetEo0FoWr8jo=?045TNv5Tge6sAUl4MKWo,0*5F",
            "!AIVDO,2,2,,A,vhOL9NIPln:BsP0=BLOiiCbE7;SKsSJfALeATapHfdm6Tl,2*79",
        ]);
        assert_eq!(m.msg_type(), Some(8));
        assert_eq!(m.mmsi(), Some(888888888));
        let d = m.decode().unwrap();
        assert_eq!(d["dac"], 0);
        assert_eq!(d["fid"], 0);
        // Unknown DAC/FID → data_hex fallback; the leading bytes match the
        // pyais `data` field (0x02 0x93 0x34 ...).
        assert!(d["data_hex"].as_str().unwrap().starts_with("029334"));
    }

    #[test]
    fn type21_two_fragments_match_pyais() {
        // AtoN split across two sentences (pyais line ~812).
        let m = reassemble(&[
            "!AIVDM,2,1,7,B,E4eHJhPR37q0000000000000000KUOSc=rq4h00000a,0*4A",
            "!AIVDM,2,2,7,B,@20,4*54",
        ]);
        assert_eq!(m.msg_type(), Some(21));
        let d = m.decode().unwrap();
        assert_eq!(d["aton_type"], 1);
        assert_eq!(d["name"], "DFO2");
        assert!((d["lat"].as_f64().unwrap() - 48.65457).abs() < 1e-5);
        assert!((d["lon"].as_f64().unwrap() - -123.429155).abs() < 1e-5);
    }

    #[test]
    fn type6_three_fragments_match_pyais() {
        // pyais test_msg_type_6_very_large: 3 fragments, MMSI 123345.
        let m = reassemble(&[
            "!AIVDO,3,1,0,A,6007Ql@007V40011@T=4AD52@lA5@D93A4E1@T=4AD52@lA5@D93A4E1@T=4,0*22",
            "!AIVDO,3,2,0,A,AD52@lA5@D93A4E1@T=4AD52@lA5@D93A4E1@T=4AD52@lA5@D93A4E1@T=4,0*5D",
            "!AIVDO,3,3,0,A,AD52@lA5@D93A4E1@T=4AD52@lA5@D93A4E1@T=4AD52@lA5,0*4E",
        ]);
        assert_eq!(m.msg_type(), Some(6));
        assert_eq!(m.mmsi(), Some(123345));
        let d = m.decode().unwrap();
        assert_eq!(d["dest_mmsi"], 7777);
        // 920-bit binary payload → data_hex; pyais reports b"ABCDE" repeated.
        // "ABCDE" = 0x41 0x42 0x43 0x44 0x45.
        assert!(d["data_hex"].as_str().unwrap().starts_with("4142434445"));
    }

    #[test]
    fn fragments_out_of_order_match_pyais() {
        // pyais test_decode_out_of_order: part 2 arrives before part 1.
        let m = reassemble(&[
            "!AIVDM,2,2,4,A,000000000000000,2*20",
            "!AIVDM,2,1,4,A,55O0W7`00001L@gCWGA2uItLth@DqtL5@F22220j1h742t0Ht0000000,0*08",
        ]);
        assert_eq!(m.msg_type(), Some(5));
        assert_eq!(m.mmsi(), Some(368060190));
    }

    #[test]
    fn interleaved_streams_stay_separate() {
        // pyais test_byte_stream: two type-5 messages (seq 1 and 9) for the
        // same vessel, fragments interleaved on the wire.
        let mut r = SentenceReassembler::new();
        let lines = [
            "!AIVDM,2,1,1,A,538CQ>02A;h?D9QC800pu8@T>0P4l9E8L0000017Ah:;;5r50Ahm5;C0,0*07",
            "!AIVDM,2,1,9,A,538CQ>02A;h?D9QC800pu8@T>0P4l9E8L0000017Ah:;;5r50Ahm5;C0,0*0F",
            "!AIVDM,2,2,1,A,F@V@00000000000,2*35",
            "!AIVDM,2,2,9,A,F@V@00000000000,2*3D",
        ];
        let mut completed = Vec::new();
        for l in &lines {
            if let Some(m) = r.push_line(l) {
                completed.push(m);
            }
        }
        assert_eq!(completed.len(), 2);
        for m in &completed {
            assert_eq!(m.msg_type(), Some(5));
            assert_eq!(m.mmsi(), Some(210035000));
            let d = m.decode().unwrap();
            assert_eq!(d["name"], "NORDIC HAMBURG");
        }
        assert_eq!(r.pending_count(), 0);
    }

    #[test]
    fn incomplete_message_is_buffered() {
        let mut r = SentenceReassembler::new();
        assert!(r.push_line("!AIVDM,2,1,1,A,538CQ>02A;h?D9QC800pu8@T>0P4l9E8L0000017Ah:;;5r50Ahm5;C0,0*07").is_none());
        assert_eq!(r.pending_count(), 1);
    }

    // ---- Per-MMSI tracker: type-24 Part A + Part B merge ---------------

    #[test]
    fn tracker_merges_type24_a_and_b() {
        // Canonical gpsd type-24 pair, MMSI 271041815 (verified via pyais):
        // Part A name "PROGUY"; Part B type 60, vendor "1D0", callsign
        // "TC6163", dims to_stern 15 / to_starboard 5.
        let part_a = reassemble(&["!AIVDM,1,1,,A,H42O55i18tMET00000000000000,2*6D"]);
        let part_b = reassemble(&["!AIVDM,1,1,,A,H42O55lti4hhhilD3nink000?050,0*40"]);
        assert_eq!(part_a.msg_type(), Some(24));
        assert_eq!(part_b.msg_type(), Some(24));
        assert_eq!(part_a.mmsi(), Some(271041815));
        assert_eq!(part_b.mmsi(), Some(271041815));

        let mut t = AisTracker::new();
        t.update_message(&part_a).unwrap();
        let rec = t.update_message(&part_b).unwrap().clone();

        assert_eq!(rec.mmsi, 271041815);
        // Part A contributed the name; Part B everything else — one record.
        assert_eq!(rec.get("name").unwrap(), "PROGUY");
        assert_eq!(rec.get("ship_type").unwrap(), 60);
        assert_eq!(rec.get("vendor_id").unwrap(), "1D0");
        assert_eq!(rec.get("model").unwrap(), 12);
        assert_eq!(rec.get("serial").unwrap(), 199796);
        assert_eq!(rec.get("callsign").unwrap(), "TC6163");
        assert_eq!(rec.get("to_stern").unwrap(), 15);
        assert_eq!(rec.get("to_starboard").unwrap(), 5);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn tracker_merge_order_independent() {
        // Part B first, then Part A: name still merges in, B fields preserved.
        let part_a = reassemble(&["!AIVDM,1,1,,A,H42O55i18tMET00000000000000,2*6D"]);
        let part_b = reassemble(&["!AIVDM,1,1,,A,H42O55lti4hhhilD3nink000?050,0*40"]);
        let mut t = AisTracker::new();
        t.update_message(&part_b).unwrap();
        let rec = t.update_message(&part_a).unwrap();
        assert_eq!(rec.get("name").unwrap(), "PROGUY");
        assert_eq!(rec.get("callsign").unwrap(), "TC6163");
        assert_eq!(rec.get("ship_type").unwrap(), 60);
    }

    #[test]
    fn tracker_merges_type5_voyage_then_position() {
        // A type-5 voyage report establishes identity; a later type-1
        // position report (no static fields) must not clobber it.
        let voyage = reassemble(&[
            "!AIVDM,2,1,1,A,55?MbV02;H;s<HtKR20EHE:0@T4@Dn2222222216L961O5Gf0NSQEp6ClRp8,0*1C",
            "!AIVDM,2,2,1,A,88888888880,2*25",
        ]);
        let mmsi = voyage.mmsi().unwrap();
        let mut t = AisTracker::new();
        t.update_message(&voyage).unwrap();
        // Synthetic type-1 fields carrying lat/lon but no name — feed directly
        // under the same MMSI to confirm volatile fields don't clobber static.
        let pos = serde_json::json!({ "lat": 1.0, "lon": 2.0, "sog_kt": 3.0 });
        let rec = t.update(mmsi, &pos);
        // Voyage identity preserved; volatile position not stored.
        assert_eq!(rec.get("name").unwrap(), "EVER DIADEM");
        assert_eq!(rec.get("destination").unwrap(), "NEW YORK");
        assert!(rec.get("lat").is_none());
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn tracker_keeps_distinct_mmsis_apart() {
        let a = reassemble(&["!AIVDM,1,1,,A,H42O55i18tMET00000000000000,2*6D"]);
        let mut t = AisTracker::new();
        t.update_message(&a).unwrap();
        t.update(999000111, &serde_json::json!({ "name": "OTHER" }));
        assert_eq!(t.len(), 2);
        assert_eq!(t.get(271041815).unwrap().get("name").unwrap(), "PROGUY");
        assert_eq!(t.get(999000111).unwrap().get("name").unwrap(), "OTHER");
    }
}
