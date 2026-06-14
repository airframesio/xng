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
            // Raw geocentric ECEF position (units of 4 km) plus the derived
            // geodetic-ish lat/lon/alt. The raw components feed downstream
            // Doppler/satellite-position work.
            "x": x,
            "y": y,
            "z": z,
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

/// Convert an Iridium broadcast time counter to a Unix timestamp
/// (iridium-toolkit `fmt_iritime`: ERA2 epoch 2014-05-11, 90 ms ticks,
/// minus the two leap seconds that have elapsed since). Reused by the SBD
/// transport decoder for the registration timestamp.
pub(crate) fn iri_time_unix(iritime: u32) -> f64 {
    let mut ux = iritime as f64 * 90.0 / 1000.0 + 1_399_818_235.0;
    if ux > 1_435_708_799.0 {
        ux -= 1.0; // 2015-06-30T23:59:60Z
    }
    if ux > 1_483_228_799.0 {
        ux -= 1.0; // 2016-12-31T23:59:60Z
    }
    ux
}

/// Full IBC (broadcast channel) decode (iridium-toolkit `IridiumBCMessage`).
/// `data` is the concatenated 21-bit BCH data fields, which IBC packs as
/// 42-bit blocks (exactly four for a well-formed frame): a satellite/beam
/// descriptor, a type-tagged info block (broadcast time / TMSI expiry /
/// max uplink power), and zero or more channel-assignment blocks.
pub fn parse_bc(bc_type: u32, data: &[u8], fixed: u32, raw_bits: &[u8]) -> IridiumFrame {
    let blocks: Vec<&[u8]> = data.chunks(42).filter(|c| c.len() == 42).collect();
    let mut d = serde_json::Map::new();
    d.insert("bc_type".into(), json!(bc_type));

    let mut next = 0usize;
    // Sub-block 1: satellite / cell descriptor (only for bc_type 0).
    if bc_type == 0 && next < blocks.len() {
        let b = blocks[next];
        next += 1;
        d.insert("sat".into(), json!(field(b, 0..7)));
        d.insert("beam".into(), json!(field(b, 7..13)));
        d.insert("slot".into(), json!(b[14]));
        d.insert("sv_blocking".into(), json!(b[15]));
        d.insert("acq_classes".into(), json!(field(b, 16..32)));
        d.insert("acq_sub_band".into(), json!(field(b, 32..37)));
        d.insert("acq_channels".into(), json!(field(b, 37..40)));
    }
    // Sub-block 2: type-tagged info (broadcast time / tmsi expiry / power).
    if bc_type == 0 && next < blocks.len() {
        let b = blocks[next];
        next += 1;
        let t = field(b, 0..6);
        d.insert("info_type".into(), json!(t));
        match t {
            0 => {
                d.insert("max_uplink_pwr".into(), json!(field(b, 36..42)));
            }
            1 => {
                let it = field(b, 10..42);
                d.insert("iri_time".into(), json!(it));
                d.insert("iri_time_unix".into(), json!(iri_time_unix(it)));
            }
            2 => {
                let ex = field(b, 10..42);
                d.insert("tmsi_expiry".into(), json!(ex));
                d.insert("tmsi_expiry_unix".into(), json!(iri_time_unix(ex)));
            }
            _ => {}
        }
    }
    // Remaining blocks: channel assignments (skip the all-"111"+0 filler).
    let mut assignments = Vec::new();
    for b in &blocks[next..] {
        let is_filler = b[0] == 1 && b[1] == 1 && b[2] == 1 && b[3..].iter().all(|&v| v == 0);
        if is_filler {
            continue;
        }
        assignments.push(json!({
            "random_id": field(b, 3..11),
            "timeslot": 1 + field(b, 11..13),
            "uplink_sub_band": field(b, 13..18),
            "downlink_sub_band": field(b, 18..23),
            "access": 1 + field(b, 23..26),
            "dtoa": field(b, 26..34),
            "dfoa": field(b, 34..40),
        }));
    }
    if !assignments.is_empty() {
        d.insert("assignments".into(), json!(assignments));
    }
    d.insert("bch_corrected".into(), json!(fixed));
    IridiumFrame {
        kind: "broadcast",
        acars: None,
        details: serde_json::Value::Object(d),
        raw_bits: raw_bits.to_vec(),
    }
}
