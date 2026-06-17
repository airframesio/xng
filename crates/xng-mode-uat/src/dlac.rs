//! DLAC 6-bit text decoding and FIS-B product-id tables (DO-358 / FAA
//! AC 00-63B; the alphabet and product list follow dump978's
//! `legacy/uat_decode.c`).
//!
//! DLAC ("Document Library and Application Codes") packs characters six
//! bits each, four characters per three bytes. Code 28 is a TAB control:
//! the following code is the run-length of spaces to emit.

/// DLAC alphabet, indexed 0..63. Index 3 is ETX, 26 is SUB, 27 is TAB
/// glyph slot (unused — 28 is the run-length TAB), 30 is RS (record
/// separator), 31 is LF; control codes shown as their ASCII values.
const DLAC_ALPHABET: [char; 64] = [
    '\u{03}', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q',
    'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '\u{1a}', '\t', '\u{1e}', '\n', '|', ' ', '!',
    '"', '#', '$', '%', '&', '\'', '(', ')', '*', '+', ',', '-', '.', '/', '0', '1', '2', '3', '4',
    '5', '6', '7', '8', '9', ':', ';', '<', '=', '>', '?',
];

/// Decode `bytelen` bytes of DLAC 6-bit text. TAB (code 28) expands to a
/// run of spaces whose count is the next code (dump978 `decode_dlac`).
pub fn decode_dlac(data: &[u8]) -> String {
    let mut out = String::new();
    let mut step = 0usize;
    let mut tab = false;
    let mut i = 0usize;
    while i < data.len() {
        let ch: usize = match step {
            0 => {
                let v = (data[i] >> 2) as usize;
                i += 1;
                v
            }
            1 => {
                let prev = data[i - 1];
                let v = (((prev & 0x03) << 4) | (data[i] >> 4)) as usize;
                i += 1;
                v
            }
            2 => {
                let prev = data[i - 1];
                // Note: dump978 does NOT advance in step 2.
                (((prev & 0x0f) << 2) | (data[i] >> 6)) as usize
            }
            3 => {
                let v = (data[i] & 0x3f) as usize;
                i += 1;
                v
            }
            _ => unreachable!(),
        };

        if tab {
            for _ in 0..ch {
                out.push(' ');
            }
            tab = false;
        } else if ch == 28 {
            tab = true;
        } else {
            out.push(DLAC_ALPHABET[ch]);
        }
        step = (step + 1) % 4;
    }
    out
}

/// A single textual report split out of a generic-text APDU: the
/// space-delimited `type` / `location` / `time` header (when present)
/// followed by the body, mirroring dump978's product-413 handling.
#[derive(Debug, Clone, PartialEq)]
pub struct TextReport {
    pub report_type: Option<String>,
    pub location: Option<String>,
    pub time: Option<String>,
    pub text: String,
}

/// Split decoded DLAC text into individual reports. Records are separated
/// by RS (0x1e) or ETX (0x03); each report's first three space-separated
/// tokens are interpreted as type / location / time (DO-358 generic text).
pub fn split_text_reports(decoded: &str) -> Vec<TextReport> {
    let mut reports = Vec::new();
    for raw in decoded.split(['\u{1e}', '\u{03}']) {
        if raw.is_empty() {
            continue;
        }
        // Peel off up to three leading space-delimited header tokens.
        let mut rest = raw;
        let mut header = [None, None, None];
        for slot in header.iter_mut() {
            if let Some(pos) = rest.find(' ') {
                *slot = Some(rest[..pos].to_string());
                rest = &rest[pos + 1..];
            } else {
                break;
            }
        }
        reports.push(TextReport {
            report_type: header[0].clone(),
            location: header[1].clone(),
            time: header[2].clone(),
            text: rest.to_string(),
        });
    }
    reports
}

/// FIS-B product short name by product id (FAA AC 00-63B / DO-358;
/// dump978 `get_fisb_product_name`).
pub fn product_name(id: u32) -> &'static str {
    match id {
        0 | 20 => "METAR and SPECI",
        1 | 21 => "TAF and Amended TAF",
        2 | 22 => "SIGMET",
        3 | 23 => "Convective SIGMET",
        4 | 24 => "AIRMET",
        5 | 25 => "PIREP",
        6 | 26 => "AWW",
        7 | 27 => "Winds and Temperatures Aloft",
        8 => "NOTAM (Including TFRs) and Service Status",
        9 => "Aerodrome and Airspace - D-ATIS",
        10 => "Aerodrome and Airspace - TWIP",
        11 => "Aerodrome and Airspace - AIRMET",
        12 => "Aerodrome and Airspace - SIGMET/Convective SIGMET",
        13 => "Aerodrome and Airspace - SUA Status",
        51 => "National NEXRAD, Type 0 - 4 level",
        52 => "National NEXRAD, Type 1 - 8 level (quasi 6-level VIP)",
        53 => "National NEXRAD, Type 2 - 8 level",
        54 => "National NEXRAD, Type 3 - 16 level",
        55 => "Regional NEXRAD, Type 0 - low dynamic range",
        56 => "Regional NEXRAD, Type 1 - 8 level (quasi 6-level VIP)",
        57 => "Regional NEXRAD, Type 2 - 8 level",
        58 => "Regional NEXRAD, Type 3 - 16 level",
        59 => "Individual NEXRAD, Type 0 - low dynamic range",
        60 => "Individual NEXRAD, Type 1 - 8 level (quasi 6-level VIP)",
        61 => "Individual NEXRAD, Type 2 - 8 level",
        62 => "Individual NEXRAD, Type 3 - 16 level",
        63 => "Global Block Representation - Regional NEXRAD, Type 4 - 8 level",
        64 => "Global Block Representation - CONUS NEXRAD, Type 4 - 8 level",
        81 => "Radar echo tops graphic, scheme 1: 16-level",
        82 => "Radar echo tops graphic, scheme 2: 8-level",
        83 => "Storm tops and velocity",
        101 => "Lightning strike type 1 (pixel level)",
        102 => "Lightning strike type 2 (grid element level)",
        151 => "Point phenomena, vector format",
        201 => "Surface conditions/winter precipitation graphic",
        202 => "Surface weather systems",
        254 => "AIRMET, SIGMET: Bitmap encoding",
        351 => "System Time",
        352 => "Operational Status",
        353 => "Ground Station Status",
        401 => "Generic Raster Scan Data Product APDU Payload Format Type 1",
        402 | 411 => "Generic Textual Data Product APDU Payload Format Type 1",
        403 => "Generic Vector Data Product APDU Payload Format Type 1",
        404 | 412 => "Generic Symbolic Product APDU Payload Format Type 1",
        405 | 413 => "Generic Textual Data Product APDU Payload Format Type 2",
        600 => "FISDL Products - Proprietary Encoding",
        2000 => "FAA/FIS-B Product 1 - Developmental",
        2001 => "FAA/FIS-B Product 2 - Developmental",
        2002 => "FAA/FIS-B Product 3 - Developmental",
        2003 => "FAA/FIS-B Product 4 - Developmental",
        2004 => "WSI Products - Proprietary Encoding",
        2005 => "WSI Developmental Products",
        _ => "unknown",
    }
}

/// Whether the product is text-encoded with the DLAC alphabet
/// (`get_fisb_product_format` returning a Text/DLAC class).
pub fn is_dlac_text(id: u32) -> bool {
    matches!(id, 20..=27 | 411 | 412 | 413)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dlac_decodes_metar_word() {
        // Bytes 34 55 01 4a 08 20 pack the 6-bit codes for
        // M E T A R <sp> <sp> <sp>. dump978's `decode_dlac` (built and run
        // on this machine) returns exactly "METAR   ".
        let bytes = [0x34, 0x55, 0x01, 0x4a, 0x08, 0x20];
        assert_eq!(decode_dlac(&bytes), "METAR   ");
    }

    #[test]
    fn dlac_tab_run_length_matches_dump978() {
        // 05 c0 c2 → codes 1 (A), 28 (TAB), 3 (run length), 2 (B). The TAB
        // control expands to a run of 3 spaces, so dump978 `decode_dlac`
        // (built and run on this machine) returns "A   B". This pins both
        // the non-advancing step-2 read and the TAB run-length behaviour.
        assert_eq!(decode_dlac(&[0x05, 0xc0, 0xc2]), "A   B");
    }

    #[test]
    fn product_names_match_ac0063b() {
        // FAA AC 00-63B / DO-358 product ids, via dump978's table.
        assert_eq!(product_name(0), "METAR and SPECI");
        assert_eq!(product_name(1), "TAF and Amended TAF");
        assert_eq!(product_name(5), "PIREP");
        assert_eq!(product_name(8), "NOTAM (Including TFRs) and Service Status");
        assert_eq!(product_name(413), "Generic Textual Data Product APDU Payload Format Type 2");
        assert_eq!(product_name(99999), "unknown");
    }

    #[test]
    fn dlac_text_classification() {
        // METAR/TAF/PIREP (DLAC) families and the generic-text products.
        assert!(is_dlac_text(20)); // METAR DLAC
        assert!(is_dlac_text(25)); // PIREP DLAC
        assert!(is_dlac_text(413)); // generic textual
        assert!(!is_dlac_text(8)); // NOTAM (text/graphic, not DLAC)
        assert!(!is_dlac_text(63)); // NEXRAD graphic
    }
}
