//! OHMA: Boeing aircraft-health JSON carried in ACARS (typically H1/T1
//! SMT downlinks). Recognition and framing ported from MIT-licensed
//! libacars (ohma.c; see PROVENANCE.md): an optional routing prefix,
//! the "OHMA"/"RYKO" marker, then base64 of a zlib stream containing
//! JSON.

use base64::Engine;

/// Decode an OHMA message from ACARS text. `None` = not OHMA.
pub fn parse(text: &str) -> Option<serde_json::Value> {
    let mut ptr = text;
    loop {
        // Long downlink form: '/' + 7-char ground address + '.';
        // uplink form: '/' + 2 chars + '.'.
        let b = ptr.as_bytes();
        if b.len() >= 13 && b[0] == b'/' && b[8] == b'.' {
            ptr = &ptr[9..];
        } else if b.len() >= 8 && b[0] == b'/' && b[3] == b'.' {
            ptr = &ptr[4..];
        }
        if !(ptr.starts_with("OHMA") || ptr.starts_with("RYKO")) {
            return None;
        }
        let prefix_len = text.len() - ptr.len() + 4;
        let payload = &ptr[4..];
        // Reassembly quirk (observed by libacars): a sender bug can
        // duplicate the first block, yielding "...OHMAxxx/O2.OHMAxxxyyy".
        // If the prefix recurs inside the payload, restart from there.
        if let Some(pos) = payload.find(&text[..prefix_len]) {
            ptr = &payload[pos..];
            continue;
        }
        let cleaned: String =
            payload.chars().filter(|c| !matches!(c, '\r' | '\n')).collect();
        let bin = base64::engine::general_purpose::STANDARD
            .decode(cleaned.as_bytes())
            .ok()?;
        // The payload is a zlib stream (CMF/FLG header + raw deflate).
        let inflated = miniz_oxide::inflate::decompress_to_vec_zlib(&bin).ok()?;
        return serde_json::from_slice(&inflated).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ohma(json: &str, prefix: &str) -> String {
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(json.as_bytes(), 6);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&compressed);
        format!("{prefix}OHMA{b64}")
    }

    #[test]
    fn short_form_decodes() {
        let v = parse(&make_ohma(r#"{"version":1,"message":{"sysid":"APU"}}"#, "")).unwrap();
        assert_eq!(v["message"]["sysid"], "APU");
    }

    #[test]
    fn long_form_prefix_skipped() {
        let v = parse(&make_ohma(r#"{"a":2}"#, "/RTNBOCR.")).unwrap();
        assert_eq!(v["a"], 2);
    }

    #[test]
    fn uplink_prefix_skipped() {
        let v = parse(&make_ohma(r#"{"b":3}"#, "/O2.")).unwrap();
        assert_eq!(v["b"], 3);
    }

    #[test]
    fn non_ohma_rejected() {
        assert!(parse("#DFB engine report").is_none());
        assert!(parse("OHMAnot-base64!!").is_none());
    }
}
