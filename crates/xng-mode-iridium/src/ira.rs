//! IRA (ring alert) and minimal IBC payload parsing (ported from
//! iridium-toolkit bitsparser.py, BSD-2 — see PROVENANCE.md).

use serde::Serialize;
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IridiumFrame {
    pub kind: &'static str,
    pub details: serde_json::Value,
    /// ACARS carried over SBD, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acars: Option<xng_acars::block::AcarsBlock>,
    #[serde(skip_serializing)]
    pub raw_bits: Vec<u8>,
}

fn field(bits: &[u8], range: std::ops::Range<usize>) -> u32 {
    bits[range].iter().fold(0u32, |v, &b| (v << 1) | b as u32)
}

/// Sign-magnitude-ish position component: sign bit then 11 bits.
fn pos_component(bits: &[u8], start: usize) -> i32 {
    let mag = field(bits, start + 1..start + 12) as i32;
    mag - (bits[start] as i32) * (1 << 11)
}

/// Parse the concatenated 21-bit BCH data blocks of a ring alert.
pub fn parse_ra(data: &[u8], fixed: u32, raw_bits: &[u8]) -> Option<IridiumFrame> {
    if data.len() < 63 {
        return None;
    }
    let sat = field(data, 0..7);
    let beam = field(data, 7..13);
    let x = pos_component(data, 13);
    let y = pos_component(data, 25);
    let z = pos_component(data, 37);
    // Reject the degenerate all-zero header: an idle/noisy burst whose
    // blocks BCH-correct to the trivially-valid all-zero codeword would
    // otherwise emit a bogus ring alert at sat 0 / position (0,0,0). No
    // real broadcasting satellite sits at Earth's center.
    if sat == 0 && x == 0 && y == 0 && z == 0 {
        return None;
    }
    let ra_int = field(data, 49..56);
    let ts = data[56];
    let eip = data[57];
    let sb = field(data, 58..63);

    let (xf, yf, zf) = (x as f64, y as f64, z as f64);
    let lat = zf.atan2((xf * xf + yf * yf).sqrt()).to_degrees();
    let lon = yf.atan2(xf).to_degrees();
    let radius_km = (xf * xf + yf * yf + zf * zf).sqrt() * 4.0;
    let alt_km = radius_km - 6378.0 + 23.0;

    // Pages: 42 bits each; an all-ones page terminates the list.
    let mut pages = Vec::new();
    let mut complete = false;
    for page in data[63..].chunks(42) {
        if page.len() < 42 {
            break;
        }
        if page.iter().all(|&b| b == 1) {
            complete = true;
            break;
        }
        pages.push(json!({
            "tmsi": format!("{:08x}", field(page, 0..32)),
            "msc_id": field(page, 34..39),
        }));
    }

    Some(IridiumFrame {
        kind: "ring-alert",
        acars: None,
        details: json!({
            "sat": sat,
            "beam": beam,
            "lat": lat,
            "lon": lon,
            "alt_km": alt_km,
            "ra_interval": ra_int,
            "timeslot": ts,
            "epi": eip,
            "bc_sub_band": sb,
            "pages": pages,
            "pages_complete": complete,
            "bch_corrected": fixed,
        }),
        raw_bits: raw_bits.to_vec(),
    })
}

/// Minimal IBC summary (type from the BCH(7,3) header; payload data bits
/// reported but not field-parsed in v1).
pub fn parse_bc(bc_type: u32, data: &[u8], fixed: u32, raw_bits: &[u8]) -> IridiumFrame {
    let hex: String = data
        .chunks(8)
        .map(|c| {
            let v = c.iter().fold(0u8, |a, &b| (a << 1) | b);
            format!("{:02x}", v << (8 - c.len()))
        })
        .collect();
    IridiumFrame {
        kind: "broadcast",
        acars: None,
        details: json!({
            "bc_type": bc_type,
            "data_hex": hex,
            "bch_corrected": fixed,
        }),
        raw_bits: raw_bits.to_vec(),
    }
}
