//! MIAM (Media Independent Aircraft Messaging, ARINC 841) — ported from
//! MIT-licensed libacars (miam.c / miam-core.c; see PROVENANCE.md).
//!
//! MIAM rides on ACARS label MA. The first text character selects the
//! ACARS Convergence Function frame: 'T' single transfer (a complete
//! MIAM CORE PDU), 'F'/'K'/'S'/'A'/'Y'/'X' file-transfer signalling.
//! CORE PDUs carry a base85-armored header and body around a '|'
//! delimiter; DEFLATE-compressed bodies are inflated (raw stream, as
//! libacars inflates with windowBits −15).

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum MiamFrame {
    SingleTransfer(CorePdu),
    FileTransferReq { file_id: u16, file_size: u32 },
    FileTransferAccept { file_id: u16, segment_size: u8 },
    FileSegment {
        file_id: u16,
        segment_id: u16,
        /// CORE-PDU text fragment carried by this segment.
        payload: String,
    },
    FileTransferAbort { file_id: u16, reason: u8 },
    XoffInd { file_id: Option<u16> },
    XonInd { file_id: Option<u16> },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CorePdu {
    pub version: u8,
    pub pdu_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aircraft_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    pub msg_num: Option<u8>,
    pub compressed: bool,
    /// Inner content when it decodes as text (often an embedded ACARS
    /// message or XML document).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Inner content length in bytes (text or binary).
    pub data_len: usize,
}

/// Parse a label-MA ACARS text as MIAM. `None` = not MIAM.
pub fn parse(text: &str) -> Option<MiamFrame> {
    let (first, rest) = {
        let mut ch = text.chars();
        (ch.next()?, ch.as_str())
    };
    let num = |s: &str, n: usize| -> Option<u32> { s.get(..n)?.parse().ok() };
    match first {
        'T' => core_pdu(rest).map(MiamFrame::SingleTransfer),
        'F' => Some(MiamFrame::FileTransferReq {
            file_id: num(rest, 3)? as u16,
            file_size: num(&rest[3..], 6)?,
        }),
        'K' => Some(MiamFrame::FileTransferAccept {
            file_id: num(rest, 3)? as u16,
            segment_size: {
                let c = rest.as_bytes().get(3).copied()?;
                match c {
                    b'0'..=b'9' => c - b'0',
                    b'A'..=b'F' => c - b'A' + 10,
                    _ => return None,
                }
            },
        }),
        'S' => Some(MiamFrame::FileSegment {
            file_id: num(rest, 3)? as u16,
            segment_id: num(&rest[3..], 3)? as u16,
            payload: rest.get(6..)?.to_string(),
        }),
        'A' => Some(MiamFrame::FileTransferAbort {
            file_id: num(rest, 3)? as u16,
            reason: num(&rest[3..], 1)? as u8,
        }),
        'Y' => Some(MiamFrame::XoffInd { file_id: num(rest, 3).map(|v| v as u16) }),
        'X' => Some(MiamFrame::XonInd { file_id: num(rest, 3).map(|v| v as u16) }),
        _ => None,
    }
}

/// Reassembles multi-message MIAM file transfers (libacars semantics:
/// the FileTransferReq registers file id + size; segments numbered
/// from 1 carry CORE-PDU text fragments; the combined text parses as a
/// CORE PDU once the declared size is reached).
pub struct FileReassembler {
    pending: std::collections::HashMap<(String, u16), FileEntry>,
}

struct FileEntry {
    expected: usize,
    parts: std::collections::BTreeMap<u16, String>,
    time: f64,
}

const FILE_TIMEOUT_SECS: f64 = 600.0;

impl FileReassembler {
    pub fn new() -> Self {
        Self { pending: std::collections::HashMap::new() }
    }

    /// Offer a label-MA message text; returns the completed CORE PDU
    /// when a file transfer finishes.
    pub fn push(&mut self, tail: &str, text: &str, now: f64) -> Option<CorePdu> {
        self.pending.retain(|_, e| now - e.time < FILE_TIMEOUT_SECS);
        match parse(text)? {
            MiamFrame::FileTransferReq { file_id, file_size } => {
                self.pending.insert(
                    (tail.to_owned(), file_id),
                    FileEntry {
                        expected: file_size as usize,
                        parts: Default::default(),
                        time: now,
                    },
                );
                None
            }
            MiamFrame::FileTransferAbort { file_id, .. } => {
                self.pending.remove(&(tail.to_owned(), file_id));
                None
            }
            MiamFrame::FileSegment { file_id, segment_id, payload } => {
                let e = self.pending.get_mut(&(tail.to_owned(), file_id))?;
                e.parts.insert(segment_id, payload);
                e.time = now;
                let total: usize = e.parts.values().map(|p| p.len()).sum();
                if total < e.expected {
                    return None;
                }
                // Contiguous from segment 1.
                let contiguous = e
                    .parts
                    .keys()
                    .copied()
                    .zip(1u16..)
                    .all(|(have, want)| have == want);
                if !contiguous {
                    return None;
                }
                let combined: String =
                    e.parts.values().map(String::as_str).collect();
                self.pending.remove(&(tail.to_owned(), file_id));
                core_pdu(&combined)
            }
            _ => None,
        }
    }
}

impl Default for FileReassembler {
    fn default() -> Self {
        Self::new()
    }
}

/// base85 with '!' (0x21) zero digit and 'z' shorthand for an all-zero
/// word; 5 chars → 4 bytes, big-endian.
fn base85_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 5 * 4 + 8);
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'z' {
            out.extend_from_slice(&[0, 0, 0, 0]);
            i += 1;
            continue;
        }
        if i + 5 > b.len() {
            return None; // truncated group
        }
        let mut v: u64 = 0;
        for k in 0..5 {
            let d = b[i + k];
            if !(0x21..0x21 + 85).contains(&d) {
                return None;
            }
            v = v * 85 + (d - 0x21) as u64;
        }
        out.extend_from_slice(&(v as u32).to_be_bytes());
        i += 5;
    }
    Some(out)
}

fn core_pdu(text: &str) -> Option<CorePdu> {
    let b = text.as_bytes();
    if b.len() < 3 {
        return None;
    }
    let bpad = b[0];
    let hpad = b[1];
    if !matches!(bpad, b'0'..=b'3' | b'-' | b'.') || !matches!(hpad, b'0'..=b'3') {
        return None;
    }
    let rest = &text[2..];
    let (hdr_txt, body_txt) = rest.split_once('|')?;
    if hdr_txt.is_empty() {
        return None;
    }
    let mut hdr = base85_decode(hdr_txt)?;
    let hpad = (hpad - b'0') as usize;
    if hdr.len() < hpad + 1 {
        return None;
    }
    hdr.truncate(hdr.len() - hpad);

    let body: Vec<u8> = match bpad {
        b'0'..=b'3' => {
            let mut v = base85_decode(body_txt)?;
            let p = (bpad - b'0') as usize;
            if v.len() >= p {
                v.truncate(v.len() - p);
            }
            v
        }
        b'-' => body_txt.as_bytes().to_vec(),
        _ => Vec::new(), // '.': no body
    };

    let version = hdr[0] & 0xF;
    let pdu_type_v = (hdr[0] >> 4) & 0xF;
    let pdu_type = match pdu_type_v {
        0 => "data",
        1 => "ack",
        2 => "aloha",
        3 => "aloha-reply",
        _ => "unknown",
    };
    let mut pdu = CorePdu {
        version,
        pdu_type,
        aircraft_id: None,
        app_id: None,
        msg_num: None,
        compressed: false,
        text: None,
        data_len: 0,
    };
    if pdu_type_v != 0 || !(version == 1 || version == 2) {
        return Some(pdu); // non-DATA PDUs: type identification only
    }

    // DATA PDU header walk (v1 carries pdu_len + aircraft id; v2 skips
    // straight to the message fields after one reserved octet).
    let mut h = &hdr[1..];
    if version == 1 {
        if h.len() < 19 {
            return Some(pdu);
        }
        h = &h[3..]; // 24-bit pdu_len (over header+body; not re-checked)
        pdu.aircraft_id = Some(String::from_utf8_lossy(&h[..7]).trim().to_owned());
        h = &h[7..];
    } else {
        if h.len() < 6 {
            return Some(pdu);
        }
        h = &h[1..];
    }
    pdu.msg_num = Some((h[0] >> 1) & 0x7F);
    h = &h[1..];
    let compression = ((h[0] << 2) | ((h[1] >> 6) & 0x3)) & 0x7;
    let app_type = h[1] & 0xF;
    h = &h[2..];
    let app_id_len = match app_type {
        0 => 2,
        1 => 4,
        2 | 3 => 6,
        t if version == 2 && (t & 0x8) != 0 && t != 0xD => (t & 0x7) as usize + 1,
        _ => 0,
    };
    if app_id_len > 0 && h.len() >= app_id_len {
        pdu.app_id = Some(String::from_utf8_lossy(&h[..app_id_len]).trim().to_owned());
    }
    pdu.compressed = compression == 0x1; // DEFLATE

    let data: Vec<u8> = if pdu.compressed && !body.is_empty() {
        // Raw deflate stream (libacars: inflateInit2 windowBits −15).
        miniz_oxide::inflate::decompress_to_vec(&body).ok()?
    } else {
        body
    };
    pdu.data_len = data.len();
    if !data.is_empty() && data.iter().all(|&c| c == b'\r' || c == b'\n' || c == b'\t' || (0x20..0x7F).contains(&c)) {
        pdu.text = Some(String::from_utf8_lossy(&data).into_owned());
    }
    Some(pdu)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base85_encode(data: &[u8]) -> String {
        let mut s = String::new();
        for chunk in data.chunks(4) {
            let mut w = [0u8; 4];
            w[..chunk.len()].copy_from_slice(chunk);
            let mut v = u32::from_be_bytes(w) as u64;
            let mut digits = [0u8; 5];
            for d in digits.iter_mut().rev() {
                *d = (v % 85) as u8 + 0x21;
                v /= 85;
            }
            s.extend(digits.iter().map(|&d| d as char));
        }
        s
    }

    #[test]
    fn base85_roundtrip() {
        let data = b"MIAM-CORE-TEST!!";
        let enc = base85_encode(data);
        assert_eq!(base85_decode(&enc).unwrap(), data);
        // 'z' shorthand
        assert_eq!(base85_decode("z").unwrap(), vec![0, 0, 0, 0]);
    }

    #[test]
    fn file_transfer_frames_parse() {
        assert_eq!(
            parse("F012000345"),
            Some(MiamFrame::FileTransferReq { file_id: 12, file_size: 345 })
        );
        assert_eq!(
            parse("S012003abc"),
            Some(MiamFrame::FileSegment {
                file_id: 12,
                segment_id: 3,
                payload: "abc".into()
            })
        );
        assert_eq!(parse("not miam"), None);
    }

    #[test]
    fn v1_data_pdu_with_deflate_body_roundtrips() {
        // Build a v1 DATA PDU: header (ver 1, type 0), pdu_len, aircraft
        // id, msg_num/ack, compression=DEFLATE, app_type=2-char + CRC32.
        let inner = b"#T2BThis is the embedded MIAM payload";
        let compressed =
            miniz_oxide::deflate::compress_to_vec(inner, 6);
        let mut hdr = vec![0x01u8]; // version 1, pdu_type DATA
        let total = 20 + compressed.len();
        hdr.extend_from_slice(&[(total >> 16) as u8, (total >> 8) as u8, total as u8]);
        hdr.extend_from_slice(b".N12345"); // aircraft id (7)
        hdr.push((42 << 1) | 1); // msg_num 42, ack
        // compression DEFLATE (0x1), encoding ISO5, app_type 0 (2-char):
        // comp = ((h0<<2)|(h1>>6))&7 → h0=0x00, h1=0b01_00_0000
        hdr.push(0x00);
        hdr.push(0x40);
        hdr.extend_from_slice(b"T2"); // app id
        hdr.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // CRC32 (not checked)
        // Pad both parts to whole base85 words; the pad counts ride in
        // the two characters after the frame id.
        let hpad = (4 - hdr.len() % 4) % 4;
        let bpad = (4 - compressed.len() % 4) % 4;
        let mut hdr_p = hdr.clone();
        hdr_p.extend(std::iter::repeat(0).take(hpad));
        let mut body_p = compressed.clone();
        body_p.extend(std::iter::repeat(0).take(bpad));
        let txt = format!("T{bpad}{hpad}{}|{}", base85_encode(&hdr_p), base85_encode(&body_p));

        let MiamFrame::SingleTransfer(pdu) = parse(&txt).expect("miam") else {
            panic!("expected single transfer");
        };
        assert_eq!(pdu.version, 1);
        assert_eq!(pdu.pdu_type, "data");
        assert_eq!(pdu.aircraft_id.as_deref(), Some(".N12345"));
        assert_eq!(pdu.msg_num, Some(42));
        assert!(pdu.compressed);
        assert_eq!(pdu.text.as_deref(), Some(std::str::from_utf8(inner).unwrap()));
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;

    #[test]
    fn file_transfer_reassembles_to_core_pdu() {
        // Build a small v1 DATA core PDU text (uncompressed body).
        fn b85(data: &[u8]) -> String {
            let mut s = String::new();
            for chunk in data.chunks(4) {
                let mut w = [0u8; 4];
                w[..chunk.len()].copy_from_slice(chunk);
                let mut v = u32::from_be_bytes(w) as u64;
                let mut digits = [0u8; 5];
                for d in digits.iter_mut().rev() {
                    *d = (v % 85) as u8 + 0x21;
                    v /= 85;
                }
                s.extend(digits.iter().map(|&d| d as char));
            }
            s
        }
        let mut hdr = vec![0x01u8];
        hdr.extend_from_slice(&[0, 0, 32]);
        hdr.extend_from_slice(b".N00001");
        hdr.push(2 << 1);
        hdr.push(0x00); // no compression
        hdr.push(0x00); // ISO5, app type 0
        hdr.extend_from_slice(b"T2");
        hdr.extend_from_slice(&[0, 0, 0, 0]);
        let hpad = (4 - hdr.len() % 4) % 4;
        hdr.extend(std::iter::repeat(0).take(hpad));
        let core = format!("-{hpad}{}|HELLO FILE WORLD", b85(&hdr));

        let (a, b) = core.split_at(core.len() / 2);
        let mut fr = FileReassembler::new();
        let req = format!("F007{:06}", core.len());
        assert!(fr.push("N12345", &req, 0.0).is_none());
        assert!(fr.push("N12345", &format!("S007001{a}"), 1.0).is_none());
        let pdu = fr.push("N12345", &format!("S007002{b}"), 2.0).expect("complete");
        assert_eq!(pdu.version, 1);
        assert_eq!(pdu.text.as_deref(), Some("HELLO FILE WORLD"));
    }
}
