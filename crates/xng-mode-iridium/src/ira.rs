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

/// Pack an MSB-first bit slice into a lowercase hex string (left-aligned: the
/// last nibble is zero-padded on the right when the length isn't a multiple of
/// 4). Used to surface IBC sub-blocks the field parser doesn't interpret.
fn bits_hex(bits: &[u8]) -> String {
    bits.chunks(4)
        .map(|c| {
            let mut v = 0u8;
            for i in 0..4 {
                v = (v << 1) | c.get(i).copied().unwrap_or(0);
            }
            std::char::from_digit(v as u32, 16).unwrap()
        })
        .collect()
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

/// Iridium re-epochs ("ERAs"): the network periodically resets the L-Band
/// Frame Number, so the broadcast `iri_time` counter restarts near zero from a
/// new base instant. iridium-toolkit's `fmt_iritime` hard-codes the ERA2 base
/// (2014-05-11), which silently mis-decodes every post-2025 frame ~11 years
/// into the past once ERA3 took over. Each entry is `(base_unix, leaps)`: the
/// Unix time at which that era's counter equals zero, and the number of UTC
/// leap seconds that fall *within* the era's counter span (subtracted because
/// the 90 ms tick is a continuous TAI-like count, while the base is UTC).
///
///   ERA1: 2007-03-08T03:50:21Z (1_173_325_821) — predates xng captures.
///   ERA2: 2014-05-11T14:23:55Z (1_399_818_235) — toolkit's only base; two
///         leap seconds (2015-06-30, 2016-12-31) fall inside its span.
///   ERA3: 2025-02-14T00:00:00Z (1_739_491_200) — active 2025-02-14; no leap
///         second has occurred since 2016, so none fall inside.
///   ERA4: 2026-01-14T18:08:00Z (1_768_414_080) — next re-epoch per the
///         MetOcean technical bulletin; still no intervening leap second.
///
/// Dates are the externally-documented re-epoch instants (MetOcean bulletin /
/// 2026 security analysis). The newest era whose base is <= the capture's
/// wall-clock reference time is the one in force, since each re-epoch resets
/// the counter and the receiver knows what year it is.
const IRI_EPOCHS: [(f64, u32); 4] = [
    (1_399_818_235.0, 2), // ERA2
    (1_739_491_200.0, 0), // ERA3
    (1_768_414_080.0, 0), // ERA4
    (f64::MAX, 0),        // sentinel (never selected)
];

/// Core counter -> Unix conversion against an explicit era `base` and the
/// `leaps` leap seconds inside that era's span. The two ERA2 leap-second
/// adjustments (2015-06-30, 2016-12-31) are applied only when they actually
/// fall within the computed timestamp, exactly matching toolkit `fmt_iritime`.
fn iri_time_unix_for_base(iritime: u32, base: f64, leaps: u32) -> f64 {
    let mut ux = iritime as f64 * 90.0 / 1000.0 + base;
    if leaps >= 1 && ux > 1_435_708_799.0 {
        ux -= 1.0; // 2015-06-30T23:59:60Z
    }
    if leaps >= 2 && ux > 1_483_228_799.0 {
        ux -= 1.0; // 2016-12-31T23:59:60Z
    }
    ux
}

/// Convert an Iridium broadcast time counter to a Unix timestamp, selecting
/// the era that was in force at `now_unix` (the capture's wall-clock receive
/// time). This is what makes the result correct across a re-epoch boundary:
/// after ERA3/ERA4 the counter restarts near zero, and applying the ERA2 base
/// would place the frame ~11 years in the past.
pub(crate) fn iri_time_unix_at(iritime: u32, now_unix: f64) -> f64 {
    // Pick the newest epoch whose base does not exceed the reference time.
    let mut idx = 0usize;
    for (i, &(base, _)) in IRI_EPOCHS.iter().enumerate() {
        if base <= now_unix {
            idx = i;
        } else {
            break;
        }
    }
    let (base, leaps) = IRI_EPOCHS[idx];
    iri_time_unix_for_base(iritime, base, leaps)
}

/// Convert an Iridium broadcast time counter to a Unix timestamp using the
/// current system clock to select the active era (live-decode default). For
/// deterministic conversion against a known capture time use
/// [`iri_time_unix_at`]. Reused by the SBD transport decoder for the
/// registration timestamp.
pub(crate) fn iri_time_unix(iritime: u32) -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    iri_time_unix_at(iritime, now)
}

/// Full IBC (broadcast channel) decode (iridium-toolkit `IridiumBCMessage`).
/// `data` is the concatenated 21-bit BCH data fields, which IBC packs as
/// 42-bit blocks (exactly four for a well-formed frame): a satellite/beam
/// descriptor, a type-tagged info block (broadcast time / TMSI expiry /
/// max uplink power), and zero or more channel-assignment blocks.
pub fn parse_bc(bc_type: u32, data: &[u8], fixed: u32, raw_bits: &[u8]) -> IridiumFrame {
    let mut blocks: Vec<&[u8]> = data.chunks(42).filter(|c| c.len() == 42).collect();
    let mut d = serde_json::Map::new();
    d.insert("bc_type".into(), json!(bc_type));

    // IBC is exactly four 42-bit blocks. The toolkit truncates a longer frame
    // (flagging `{LONG}`) and tags a short one `{SHORT}`; mirror that so the
    // block-length anomaly is visible and the descriptor/info/assignment
    // parsers below always see at most the four real blocks.
    if blocks.len() > 4 {
        blocks.truncate(4);
        d.insert("block_trailer".into(), json!("LONG"));
    } else if blocks.len() < 4 {
        d.insert("block_trailer".into(), json!("SHORT"));
    }

    let mut next = 0usize;
    // Sub-block 1: satellite / cell descriptor (only for bc_type 0). For any
    // other bc_type the toolkit does NOT consume a descriptor/info block — all
    // blocks fall through to the assignment loop — so we mirror that exactly.
    if bc_type == 0 && next < blocks.len() {
        let b = blocks[next];
        next += 1;
        d.insert("sat".into(), json!(field(b, 0..7)));
        d.insert("beam".into(), json!(field(b, 7..13)));
        d.insert("unknown01".into(), json!(b[13]));
        d.insert("slot".into(), json!(b[14]));
        d.insert("sv_blocking".into(), json!(b[15]));
        d.insert("acq_classes".into(), json!(field(b, 16..32)));
        d.insert("acq_sub_band".into(), json!(field(b, 32..37)));
        d.insert("acq_channels".into(), json!(field(b, 37..40)));
        d.insert("unknown02".into(), json!(field(b, 40..42)));
    }
    // Sub-block 2: type-tagged info (broadcast time / tmsi expiry / power /
    // a known-constant filler at type 4). Unrecognized info types surface
    // their raw 42-bit payload as hex rather than being dropped, matching the
    // toolkit's `type:NN <bits>` fallthrough.
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
            4 => {
                // The toolkit treats one exact 42-bit constant as silent filler
                // and otherwise surfaces the raw payload. Match both arms.
                const FILLER4: &[u8; 42] = &[
                    0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 1, 1, 0,
                    0, 0, 0, 1, 1, 0, 0, 0, 0, 1, 1, 0, 0, 1, 1, 1, 1, 0, 0, 0, 0,
                ];
                if b != &FILLER4[..] {
                    d.insert("info_raw".into(), json!(bits_hex(b)));
                }
            }
            _ => {
                d.insert("info_raw".into(), json!(bits_hex(b)));
            }
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

#[cfg(test)]
mod bc_tests {
    //! IBC sub-block completeness (non-zero `bc_type`, unrecognized /
    //! filler `info_type`, the `{LONG}`/`{SHORT}` block-count trailer, and the
    //! `unknown01`/`unknown02` descriptor bits).
    //!
    //! Oracle = iridium-toolkit `IridiumBCMessage` (bitsparser.py). Each
    //! expected value below was produced by feeding the *identical* 42-bit
    //! blocks through that class (`IridiumBCMessage.__init__` with a stub
    //! `imsg`), so these are reference-decoder cross-checks, not loopbacks.
    use super::parse_bc;

    fn bits(s: &str) -> Vec<u8> {
        s.bytes().map(|c| (c == b'1') as u8).collect()
    }

    // A non-filler channel-assignment block the toolkit decodes as
    //   [111 Rid:001 ts:1 ul_sb:03 dl_sb:22 access:6 dtoa:212 dfoa:17 00]
    const ASG: &str = "111000000010000011101101011101010001000100";
    // Descriptor block: sat:013 cell:15 0 slot:0 sv_blkn:0 aq_cl:1111…1
    // aq_sb:20 aq_ch:2 00 (toolkit `IridiumBCMessage` sub-block 1).
    const DESC: &str = "000110100111100011111111111111111010001000";
    // The all-"111"+0 channel-assignment filler the toolkit prints as `[]`.
    const FILLER_ASG: &str = "111000000000000000000000000000000000000000";

    #[test]
    fn nonzero_bc_type_blocks_are_not_misparsed_as_descriptor() {
        // Toolkit: for bc_type != 0 NO descriptor/info block is consumed; every
        // block runs through the assignment loop. Block 0 here is a real
        // assignment, the rest are the `111000…` filler (toolkit prints `[]`).
        let asg = &ASG[..42];
        let data = bits(&format!("{asg}{FILLER_ASG}{FILLER_ASG}{FILLER_ASG}"));
        let f = parse_bc(1, &data, 0, &[]);
        let d = &f.details;
        assert_eq!(d["bc_type"], 1);
        // No descriptor/info fields for a non-zero bc_type.
        assert!(d.get("sat").is_none());
        assert!(d.get("info_type").is_none());
        // The single non-filler block is surfaced as a channel assignment,
        // with the exact fields the toolkit decoded.
        let a = d["assignments"].as_array().unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0]["random_id"], 1);
        assert_eq!(a[0]["timeslot"], 1);
        assert_eq!(a[0]["uplink_sub_band"], 3);
        assert_eq!(a[0]["downlink_sub_band"], 22);
        assert_eq!(a[0]["access"], 6);
        assert_eq!(a[0]["dtoa"], 212);
        assert_eq!(a[0]["dfoa"], 17);
    }

    #[test]
    fn descriptor_unknown_bits_are_surfaced() {
        // bc_type 0 descriptor + an all-zero info block (info_type 0).
        let data = bits(&format!("{DESC}{}", "0".repeat(42)));
        let d = parse_bc(0, &data, 0, &[]).details;
        assert_eq!(d["sat"], 13);
        assert_eq!(d["beam"], 15);
        // Toolkit `unknown01` (bit 13) and `unknown02` (bits 40..42); both 0
        // for this descriptor (toolkit prints the lone `0` and trailing `00`).
        assert_eq!(d["unknown01"], 0);
        assert_eq!(d["unknown02"], 0);
        assert_eq!(d["acq_sub_band"], 20);
        assert_eq!(d["acq_channels"], 2);
    }

    #[test]
    fn info_type_4_known_filler_is_silent() {
        // Toolkit recognizes this exact 42-bit constant as type-4 filler and
        // emits no raw payload.
        let filler4 = "000100000000100001110000110000110011110000";
        let data = bits(&format!("{DESC}{filler4}"));
        let d = parse_bc(0, &data, 0, &[]).details;
        assert_eq!(d["info_type"], 4);
        assert!(d.get("info_raw").is_none(), "filler-4 must stay silent");
    }

    #[test]
    fn unrecognized_info_type_surfaces_raw_payload() {
        // info_type 3 (no typed parse): toolkit prints `type:03 <42 bits>`. We
        // surface those bits as hex rather than dropping them.
        let info3 = format!("000011{}", "0".repeat(36)); // type=3, rest zero
        let data = bits(&format!("{DESC}{info3}"));
        let d = parse_bc(0, &data, 0, &[]).details;
        assert_eq!(d["info_type"], 3);
        // 42 bits, MSB-first nibbles -> "0c" then zeros (11 nibbles: the last
        // is the trailing 2 bits, zero-padded).
        assert_eq!(d["info_raw"], "0c000000000");
    }

    #[test]
    fn block_count_anomaly_is_flagged() {
        // Fewer than four blocks -> {SHORT}; more than four -> {LONG}.
        let short = bits(&format!("{DESC}{}", "0".repeat(42)));
        assert_eq!(parse_bc(0, &short, 0, &[]).details["block_trailer"], "SHORT");

        let asg = &ASG[..42];
        let long = bits(&format!(
            "{DESC}{}{asg}{asg}{asg}",
            "0".repeat(42)
        ));
        assert_eq!(parse_bc(0, &long, 0, &[]).details["block_trailer"], "LONG");
    }
}

#[cfg(test)]
mod time_tests {
    //! IRID-8: Iridium re-epoch (LBFN roll) correctness.
    //!
    //! Oracle = iridium-toolkit `util.fmt_iritime` for the ERA2 path (pinned
    //! values below were captured directly from that function), and the
    //! externally-documented re-epoch instants (MetOcean bulletin: ERA3
    //! 2025-02-14, ERA4 2026-01-14T18:08Z) for the boundary behaviour.
    use super::{iri_time_unix_at, iri_time_unix_for_base};

    // Reference time inside each era (Unix seconds).
    const T_2020: f64 = 1_590_000_000.0; // 2020-05-20, ERA2 active
    const T_2025: f64 = 1_745_000_000.0; // 2025-04-18, ERA3 active
    const T_2026: f64 = 1_775_000_000.0; // 2026-03-31, ERA4 active

    /// ERA2 path must reproduce iridium-toolkit `fmt_iritime` exactly. These
    /// expected values were generated by the toolkit (see util.fmt_iritime):
    ///   fmt_iritime(0)          == 1399818235.0  (2014-05-11T14:23:55Z)
    ///   fmt_iritime(1_000_000)  == 1399908235.0  (2014-05-12T15:23:55Z)
    ///   fmt_iritime(2_400_000_000) == 1615818233.0 (both 2015+2016 leaps)
    ///   fmt_iritime(3_000_000_000) == 1669818233.0
    #[test]
    fn era2_matches_toolkit_oracle() {
        // Decoded while ERA2 was in force -> identical to the toolkit.
        assert_eq!(iri_time_unix_at(0, T_2020), 1_399_818_235.0);
        assert_eq!(iri_time_unix_at(1_000_000, T_2020), 1_399_908_235.0);
        // Large counters straddle both leap seconds (the -2 s correction).
        assert_eq!(iri_time_unix_at(2_400_000_000, T_2020), 1_615_818_233.0);
        assert_eq!(iri_time_unix_at(3_000_000_000, T_2020), 1_669_818_233.0);
    }

    /// The core regression: across the ERA3 re-epoch, a counter that restarts
    /// near zero must decode to ~2025, NOT to ~2014 as the ERA2-only formula
    /// (and stock toolkit) would. Counter for ~63 days into ERA3:
    ///   60.5e6 ticks * 90 ms = 5_445_000 s after 2025-02-14T00:00:00Z.
    #[test]
    fn era3_reepoch_does_not_decode_into_the_past() {
        let counter = 60_500_000u32;
        let ux = iri_time_unix_at(counter, T_2025);
        // ERA3 base + counter*0.09, no leap correction.
        assert_eq!(ux, 1_739_491_200.0 + 60_500_000.0 * 0.09);
        // Sanity: lands in 2025, not 2014.
        assert!(
            ux > 1_739_000_000.0 && ux < 1_768_000_000.0,
            "expected a 2025 timestamp, got {ux}"
        );
        // The buggy ERA2 interpretation of the same counter would be ~2014:
        let buggy = iri_time_unix_for_base(counter, 1_399_818_235.0, 2);
        assert!(buggy < 1_420_000_000.0, "ERA2 path lands in 2014: {buggy}");
        assert!(ux - buggy > 330_000_000.0, "re-epoch must shift ~10.7 years");
    }

    /// Same counter, opposite verdict depending on which era was in force at
    /// receive time — proves selection is driven by the wall-clock reference,
    /// not the counter magnitude (the ranges overlap and can't self-classify).
    #[test]
    fn era_selected_by_reference_time() {
        let counter = 300_000_000u32; // ~2018 under ERA2, ~2025 under ERA3.
        let as_era2 = iri_time_unix_at(counter, T_2020);
        let as_era3 = iri_time_unix_at(counter, T_2025);
        let as_era4 = iri_time_unix_at(counter, T_2026);
        assert_eq!(as_era2, iri_time_unix_for_base(counter, 1_399_818_235.0, 2));
        assert_eq!(as_era3, iri_time_unix_for_base(counter, 1_739_491_200.0, 0));
        assert_eq!(as_era4, iri_time_unix_for_base(counter, 1_768_414_080.0, 0));
        assert!(as_era3 > as_era2 && as_era4 > as_era3);
    }

    /// Boundary: exactly at the ERA4 base instant the newer era takes over.
    #[test]
    fn epoch_boundary_is_inclusive_of_newer_era() {
        const ERA4_BASE: f64 = 1_768_414_080.0;
        // One second before ERA4 begins -> still ERA3.
        assert_eq!(
            iri_time_unix_at(0, ERA4_BASE - 1.0),
            iri_time_unix_for_base(0, 1_739_491_200.0, 0)
        );
        // At/after the ERA4 instant -> ERA4.
        assert_eq!(
            iri_time_unix_at(0, ERA4_BASE),
            iri_time_unix_for_base(0, ERA4_BASE, 0)
        );
    }
}
