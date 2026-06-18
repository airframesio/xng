//! Embedded web dashboard: a live map of decoded aircraft (Mode S CPR)
//! and vessels (AIS) plus a message stream and per-mode counters —
//! the in-browser view single-mode decoders get from tar1090 or the
//! AIS-catcher viewer, here for every mode at once.
//!
//! One hand-rolled HTTP endpoint (same pattern as the Prometheus
//! exporter): `GET /` serves the embedded page, `GET /api/state`
//! serves the JSON snapshot the page polls.

use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;
use xng_types::{Message, MessageBody};

const PAGE: &str = include_str!("assets/dashboard.html");
const RECENT_CAP: usize = 4000;
/// Recent log messages sent in each snapshot (the log pane shows these).
const RECENT_SENT: usize = 2000;
/// Drop map entities not heard from in this many seconds.
const EXPIRE_S: u64 = 300;

/// Iridium ring-alert positions are split by altitude (cf. iridium-toolkit
/// live-map) via `crate::beam::classify_altitude`: a frame's geocentric
/// position is either the broadcasting satellite (~780 km) or a ground beam
/// footprint (~0 km).
/// Satellites move continuously; drop one ~2 min after it was last heard so a
/// satellite that has flown out of range (and its projected beam pattern)
/// clears promptly instead of lingering as a stale ghost. An overhead Iridium
/// satellite is heard every few seconds, so 2 min is ample margin.
const SAT_EXPIRE_S: u64 = 120;
const RING_EXPIRE_S: u64 = 300;

#[derive(Default)]
struct Dash {
    aircraft: HashMap<String, Value>,
    /// Identifier-token → master aircraft id, so one aircraft heard via
    /// several sources (ADS-B icao, ACARS tail/flight, …) coalesces into one
    /// entity. Tokens are namespaced: `ic:<HEX>`, `rg:<reg>`, `fl:<flight>`.
    ac_index: HashMap<String, String>,
    vessels: HashMap<u32, Value>,
    /// Iridium satellite positions, keyed by satellite id.
    iridium_sats: HashMap<u64, Value>,
    /// Iridium ring/beam ground footprints, keyed by "sat-beam".
    iridium_rings: HashMap<String, Value>,
    /// Iridium mobile-terminal positions (vessels/aircraft/handhelds that
    /// report their own ECEF), keyed by quantized lat/lon so a stationary
    /// terminal coalesces and a moving one leaves recent fixes.
    iridium_devices: HashMap<String, Value>,
    /// Iridium SBD terminals seen on the short-burst-data channel, keyed by
    /// IMEI — the identifiable transmitters behind the "sbd" frames (asset
    /// trackers, SATCOM modems), surfaced as entities so the activity is
    /// visible even though most carry no aircraft ACARS payload.
    iridium_terminals: HashMap<String, Value>,
    /// Position-bearing non-aircraft/vessel beacons (radiosondes, ADS-L
    /// conspicuity, COSPAS-SARSAT distress, DSC distress), keyed by "mode:id".
    beacons: HashMap<String, Value>,
    /// Rail EOT/HOT telemetry units, keyed by unit address (no position).
    trains: HashMap<String, Value>,
    /// Pager capcodes (FLEX/POCSAG), keyed by "proto:capcode" (no position).
    pagers: HashMap<String, Value>,
    /// Reconstructed 48-beam pattern, projected under tracked satellites.
    beams: crate::beam::BeamReconstructor,
    /// Last unix-secs the beam pattern was persisted.
    beams_saved: u64,
    recent: VecDeque<Value>,
    totals: HashMap<String, u64>,
    /// Last time (unix secs) a message of each mode was seen — drives
    /// the `xng status` per-session liveness column.
    last_seen: HashMap<String, u64>,
    /// Monotonic message id — lets the page keep expansion state
    /// across poll re-renders.
    next_id: u64,
    station: String,
    started: u64,
    /// Static per-session descriptors (SDR, mode, tuning) for status.
    sessions: Vec<Value>,
}

/// Append to the entity's position trail (decimated: only when moved
/// meaningfully; capped length).
fn push_trail(o: &mut serde_json::Map<String, Value>, lat: f64, lon: f64) {
    let trail = o.entry("trail").or_insert_with(|| json!([]));
    let arr = trail.as_array_mut().unwrap();
    if let Some(last) = arr.last().and_then(Value::as_array) {
        let (pl, po) = (last[0].as_f64().unwrap_or(0.0), last[1].as_f64().unwrap_or(0.0));
        if (pl - lat).abs() < 1e-4 && (po - lon).abs() < 1e-4 {
            return;
        }
    }
    arr.push(json!([lat, lon]));
    if arr.len() > 60 {
        arr.remove(0);
    }
}

fn now_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Per-message aircraft identifiers. ICAO (uppercased) is the primary merge
/// key, then registration (tail), then flight number.
struct AcIds {
    icao: Option<String>,
    flight: Option<String>,
    reg: Option<String>,
}

/// Strip control + HTML-significant characters from a decoded identifier.
/// Defence in depth: these never appear in a legit ARINC/ICAO/AIS identifier,
/// and the values become the entity id / `data-*` keys on the dashboard (which
/// also escapes on output). Keeps junk from a corrupt-but-passing frame inert.
fn sanitize_id(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() && !matches!(c, '<' | '>' | '"' | '\'' | '&')).collect()
}

/// Upsert an aircraft entity, coalescing across sources by ICAO > reg > flight.
/// `contrib` = the display fields this message contributes (latest-wins on the
/// merged master, recorded verbatim on the per-source row). `source` = the
/// carrier/mode label. `pos` (if any) extends the shared position trail.
fn merge_aircraft(
    d: &mut Dash,
    source: &str,
    ids: AcIds,
    contrib: serde_json::Map<String, Value>,
    pos: Option<(f64, f64)>,
) {
    // Sanitize identifiers (defence in depth) before they key the entity.
    let ids = AcIds {
        icao: ids.icao.map(|s| sanitize_id(&s)).filter(|s| !s.is_empty()),
        flight: ids.flight.map(|s| sanitize_id(&s)).filter(|s| !s.is_empty()),
        reg: ids.reg.map(|s| sanitize_id(&s)).filter(|s| !s.is_empty()),
    };
    let mut tokens: Vec<String> = Vec::new();
    if let Some(i) = &ids.icao {
        tokens.push(format!("ic:{i}"));
    }
    if let Some(r) = &ids.reg {
        tokens.push(format!("rg:{r}"));
    }
    if let Some(f) = &ids.flight {
        tokens.push(format!("fl:{f}"));
    }
    if tokens.is_empty() {
        return;
    }

    // Resolve (merging if a linking message joined separate entities).
    let mut masters: Vec<String> =
        tokens.iter().filter_map(|t| d.ac_index.get(t).cloned()).collect();
    masters.sort();
    masters.dedup();
    let master_id = match masters.len() {
        0 => ids.icao.clone().or_else(|| ids.reg.clone()).or_else(|| ids.flight.clone()).unwrap(),
        1 => masters.remove(0),
        _ => {
            // Prefer the ICAO-keyed survivor; fold the rest in, repoint index.
            let survivor = ids
                .icao
                .as_ref()
                .filter(|i| masters.iter().any(|m| m == *i))
                .cloned()
                .unwrap_or_else(|| masters[0].clone());
            for mid in masters.iter().filter(|m| **m != survivor) {
                if let Some(other) = d.aircraft.remove(mid) {
                    fold_entity(&mut d.aircraft, &survivor, other);
                }
            }
            for v in d.ac_index.values_mut() {
                if masters.contains(v) {
                    *v = survivor.clone();
                }
            }
            survivor
        }
    };
    for t in &tokens {
        d.ac_index.insert(t.clone(), master_id.clone());
    }

    let e = d.aircraft.entry(master_id.clone()).or_insert_with(|| json!({}));
    let o = e.as_object_mut().unwrap();
    o.insert("id".into(), json!(master_id));
    if let Some(i) = &ids.icao {
        o.insert("icao".into(), json!(i));
    }
    if let Some(f) = &ids.flight {
        o.insert("flight".into(), json!(f));
    }
    if let Some(r) = &ids.reg {
        o.insert("reg".into(), json!(r));
    }
    o.insert("seen".into(), json!(now_s()));
    let msgs = o.get("msgs").and_then(Value::as_u64).unwrap_or(0);
    o.insert("msgs".into(), json!(msgs + 1));
    for (k, v) in &contrib {
        o.insert(k.clone(), v.clone());
    }
    if let Some((la, lo)) = pos {
        push_trail(o, la, lo);
    }

    // Per-source contribution: what THIS source provided + its own counts.
    let smap = o.entry("sources").or_insert_with(|| json!({})).as_object_mut().unwrap();
    let so = smap.entry(source.to_string()).or_insert_with(|| json!({})).as_object_mut().unwrap();
    so.insert("source".into(), json!(source));
    if let Some(i) = &ids.icao {
        so.insert("icao".into(), json!(i));
    }
    if let Some(f) = &ids.flight {
        so.insert("flight".into(), json!(f));
    }
    if let Some(r) = &ids.reg {
        so.insert("reg".into(), json!(r));
    }
    for (k, v) in &contrib {
        so.insert(k.clone(), v.clone());
    }
    let sm = so.get("msgs").and_then(Value::as_u64).unwrap_or(0);
    so.insert("msgs".into(), json!(sm + 1));
    so.insert("seen".into(), json!(now_s()));
}

/// Fold a merged-away aircraft into the survivor: sum msgs, merge source rows,
/// adopt fields the survivor lacks (never clobbering its own identity).
fn fold_entity(aircraft: &mut HashMap<String, Value>, survivor: &str, other: Value) {
    let Some(s) = aircraft.get_mut(survivor).and_then(|v| v.as_object_mut()) else {
        return;
    };
    let Some(oo) = other.as_object() else { return };
    let add = oo.get("msgs").and_then(Value::as_u64).unwrap_or(0);
    let cur = s.get("msgs").and_then(Value::as_u64).unwrap_or(0);
    s.insert("msgs".into(), json!(cur + add));
    if let Some(osrc) = oo.get("sources").and_then(Value::as_object) {
        let smap = s.entry("sources").or_insert_with(|| json!({})).as_object_mut().unwrap();
        for (k, v) in osrc {
            smap.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    for (k, v) in oo {
        if k == "msgs" || k == "sources" || k == "id" {
            continue;
        }
        s.entry(k.clone()).or_insert_with(|| v.clone());
    }
}

/// Iridium link-layer housekeeping that carries no user content and arrives at
/// tens of thousands of frames per session — excluded from the web log feed so
/// content frames stay visible (still counted in the per-mode totals).
fn is_link_housekeeping(m: &Message) -> bool {
    matches!(
        &m.body,
        MessageBody::Iridium { kind, .. } if matches!(kind.as_str(), "sync" | "ida" | "itl")
    )
}

fn update(d: &mut Dash, m: &Message) {
    let mode = m.mode.as_str().to_string();
    *d.totals.entry(mode.clone()).or_insert(0) += 1;
    d.last_seen.insert(mode.clone(), now_s());

    // Only clean (CRC-valid) frames compose map/table entities. Corrupt frames
    // still count toward totals and reach the message log, but must never plant
    // junk aircraft/vessel/satellite entities (was a live dashboard bug).
    if m.decode.crc_ok {
    match &m.body {
        MessageBody::ModeS {
            icao: Some(icao),
            callsign,
            altitude_ft,
            lat,
            lon,
            speed_kt,
            track_deg,
            squawk,
            adsb_status,
            comm_b,
            ..
        } => {
            let icao_uc = icao.to_uppercase();
            let mut c = serde_json::Map::new();
            let mut reg: Option<String> = None;
            if let Ok(hex) = u32::from_str_radix(icao, 16) {
                if let Some(co) = crate::outputs::dbinfo::icao_country(hex) {
                    c.insert("country".into(), json!(co));
                }
                if let Some((r, t)) = crate::outputs::dbinfo::AircraftDb::lookup(hex) {
                    if !r.is_empty() {
                        reg = Some(r.to_string());
                    }
                    if !t.is_empty() {
                        c.insert("actype".into(), json!(t));
                    }
                }
            }
            for (k, v) in [
                ("alt", altitude_ft.map(|v| json!(v))),
                ("lat", lat.map(|v| json!(v))),
                ("lon", lon.map(|v| json!(v))),
                ("spd", speed_kt.map(|v| json!(v.round()))),
                ("trk", track_deg.map(|v| json!(v.round()))),
                ("squawk", squawk.as_ref().map(|v| json!(v))),
                ("adsb_version", adsb_status.as_ref().and_then(|s| s.get("version").cloned())),
                ("nacp", adsb_status.as_ref().and_then(|s| s.get("nac_p").cloned())),
                ("sil", adsb_status.as_ref().and_then(|s| s.get("sil").cloned())),
                // TC29 target-state selected altitude; BDS 4,4 wind/temp.
                ("sel_alt", adsb_status.as_ref().and_then(|s| s.get("selected_altitude").cloned())),
                ("wind_kt", comm_b.as_ref().and_then(|s| s.get("wind_speed").cloned())),
                ("oat_c", comm_b.as_ref().and_then(|s| s.get("static_air_temperature").cloned())),
            ] {
                if let Some(v) = v {
                    c.insert(k.into(), v);
                }
            }
            // Sticky map flags: non-"none" emergency, ACAS RA (BDS 3,0 / TC28).
            if let Some(em) = adsb_status
                .as_ref()
                .and_then(|s| s.get("emergency"))
                .and_then(|v| v.as_str())
                .filter(|e| *e != "none")
            {
                c.insert("emergency".into(), json!(em));
            }
            if comm_b.as_ref().and_then(|s| s.get("issued_ra")).and_then(|v| v.as_bool()) == Some(true)
                || adsb_status.as_ref().and_then(|s| s.get("acas_ra")).and_then(|v| v.as_bool()) == Some(true)
            {
                c.insert("acas_ra".into(), json!(true));
            }
            let flight = callsign.as_ref().map(|x| x.trim().to_uppercase()).filter(|s| !s.is_empty());
            let pos = match (lat, lon) {
                (Some(la), Some(lo)) => Some((*la, *lo)),
                _ => None,
            };
            merge_aircraft(d, "adsb", AcIds { icao: Some(icao_uc), flight, reg }, c, pos);
        }
        MessageBody::Ais { mmsi: Some(mmsi), details: Some(det), msg_type, .. } => {
            let e = d.vessels.entry(*mmsi).or_insert_with(|| json!({}));
            let o = e.as_object_mut().unwrap();
            o.insert("mmsi".into(), json!(mmsi));
            o.insert("seen".into(), json!(now_s()));
            if !o.contains_key("country") {
                if let Some(c) = crate::outputs::dbinfo::mid_country(*mmsi) {
                    o.insert("country".into(), json!(c));
                }
            }
            if let (Some(la), Some(lo_)) = (
                det.get("lat").and_then(Value::as_f64),
                det.get("lon").and_then(Value::as_f64),
            ) {
                push_trail(o, la, lo_);
            }
            if let Some(t) = msg_type {
                o.insert("type".into(), json!(t));
            }
            // Ship type (ITU-R M.1371 code, types 5/19/21/24) — sticky:
            // it arrives on static reports, the map marker is keyed to
            // it, and position reports must not clear it.
            if let Some(st) = det.get("ship_type").and_then(Value::as_u64) {
                if st != 0 {
                    o.insert("shiptype".into(), json!(st));
                }
            }
            // Distress beacon (AIS-SART/MOB/EPIRB-AIS by MMSI prefix) — sticky.
            if let Some(dist) = det.get("distress") {
                o.insert("distress".into(), dist.clone());
            }
            for (k, src) in
                [("lat", "lat"), ("lon", "lon"), ("sog", "sog_kt"), ("cog", "cog_deg"), ("name", "name")]
            {
                if let Some(v) = det.get(src) {
                    o.insert(k.into(), v.clone());
                }
            }
        }
        // ACARS (from any carrier — VHF, VDL2, HFDL, Aero, or Iridium SBD):
        // surface the aircraft as an entity keyed by tail (else flight), so it
        // shows in the table even without a position. A position appears only
        // if the application layer carries one (ADS-C report).
        MessageBody::Acars(a) => {
            let reg = a.tail.clone().filter(|s| !s.is_empty());
            let flight = a.flight.as_ref().map(|f| f.trim().to_uppercase()).filter(|s| !s.is_empty());
            if reg.is_some() || flight.is_some() {
                let mut c = serde_json::Map::new();
                let mut pos = None;
                // ADS-C position report from the application layer, if present.
                if let Some(app) = &a.app {
                    if app.get("app").and_then(|v| v.as_str()) == Some("adsc") {
                        for t in app.get("tags").and_then(Value::as_array).into_iter().flatten() {
                            if let (Some(lat), Some(lon)) = (
                                t.get("lat").and_then(Value::as_f64),
                                t.get("lon").and_then(Value::as_f64),
                            ) {
                                pos = Some((lat, lon));
                                c.insert("lat".into(), json!(lat));
                                c.insert("lon".into(), json!(lon));
                                if let Some(alt) = t.get("alt_ft").and_then(Value::as_i64) {
                                    c.insert("alt".into(), json!(alt));
                                }
                            }
                        }
                    }
                }
                // Source = the carrier mode (acars/vdl2/hfdl/aero-l/iridium), so
                // one aircraft on several carriers shows multiple source rows.
                merge_aircraft(d, &mode, AcIds { icao: None, flight, reg }, c, pos);
            }
        }
        // Iridium ring alerts carry the broadcasting satellite's geocentric
        // position; split into satellite positions (high altitude) and
        // targeted ground beam footprints (low altitude) for the map.
        MessageBody::Iridium { kind, details } if kind == "ring-alert" => {
            let (lat, lon, alt) = (
                details.get("lat").and_then(Value::as_f64),
                details.get("lon").and_then(Value::as_f64),
                details.get("alt_km").and_then(Value::as_f64),
            );
            let sat = details.get("sat").and_then(Value::as_u64);
            if let (Some(lat), Some(lon), Some(alt), Some(sat)) = (lat, lon, alt, sat) {
                let beam = details.get("beam").and_then(Value::as_u64).unwrap_or(0);
                // Feed the 48-beam reconstructor with the raw ECEF position
                // (details carry x/y/z in units of 4 km).
                if let (Some(x), Some(y), Some(z)) = (
                    details.get("x").and_then(Value::as_i64),
                    details.get("y").and_then(Value::as_i64),
                    details.get("z").and_then(Value::as_i64),
                ) {
                    let ecef = [x as f64 * 4.0, y as f64 * 4.0, z as f64 * 4.0];
                    d.beams.observe(sat, alt, ecef, beam as u8, m.timestamp.timestamp() as f64);
                }
                // Same altitude classifier the reconstructor uses, so a
                // garbage decode (implausible altitude) never plants a
                // phantom satellite marker or ground footprint.
                match crate::beam::classify_altitude(alt) {
                    crate::beam::AltClass::Satellite => {
                        let e = d.iridium_sats.entry(sat).or_insert_with(|| json!({}));
                        let o = e.as_object_mut().unwrap();
                        o.insert("sat".into(), json!(sat));
                        o.insert("beam".into(), json!(beam));
                        o.insert("lat".into(), json!(lat));
                        o.insert("lon".into(), json!(lon));
                        o.insert("alt".into(), json!(alt.round()));
                        o.insert("seen".into(), json!(now_s()));
                        if let Some(name) = details.get("satellite") {
                            o.insert("name".into(), name.clone());
                        }
                        push_trail(o, lat, lon); // satellite ground track
                    }
                    crate::beam::AltClass::Footprint => {
                        let e =
                            d.iridium_rings.entry(format!("{sat}-{beam}")).or_insert_with(|| json!({}));
                        let o = e.as_object_mut().unwrap();
                        o.insert("sat".into(), json!(sat));
                        o.insert("beam".into(), json!(beam));
                        o.insert("lat".into(), json!(lat));
                        o.insert("lon".into(), json!(lon));
                        o.insert("seen".into(), json!(now_s()));
                    }
                    crate::beam::AltClass::Implausible => {}
                }
            }
        }
        // Mobile-terminal self-reported positions: the actual Iridium
        // customer terminals (vessels/aircraft/handhelds), distinct from the
        // ring-alert beam footprints above.
        MessageBody::Iridium { kind, details } if kind == "mt-position" => {
            if let (Some(lat), Some(lon)) = (
                details.get("lat").and_then(Value::as_f64),
                details.get("lon").and_then(Value::as_f64),
            ) {
                let key = format!("{lat:.2},{lon:.2}");
                let e = d.iridium_devices.entry(key).or_insert_with(|| json!({}));
                let o = e.as_object_mut().unwrap();
                o.insert("lat".into(), json!(lat));
                o.insert("lon".into(), json!(lon));
                o.insert("alt_km".into(), json!(details.get("alt_km").and_then(Value::as_i64).unwrap_or(0)));
                if let Some(mt) = details.get("msg_type") {
                    o.insert("msg_type".into(), mt.clone());
                }
                o.insert("seen".into(), json!(now_s()));
            }
        }
        // SBD short-burst-data: most frames are transport signaling
        // (registration, MO/MT control) rather than aircraft ACARS — a real
        // ACARS payload arrives as MessageBody::Acars above. The frames that
        // carry an IMEI identify a transmitting terminal (asset tracker,
        // SATCOM modem), so surface it as an entity to make the activity
        // visible.
        MessageBody::Iridium { kind, details } if kind == "sbd" => {
            if let Some(imei) = details.get("imei").and_then(Value::as_str) {
                let e = d.iridium_terminals.entry(imei.to_string()).or_insert_with(|| json!({}));
                let o = e.as_object_mut().unwrap();
                o.insert("imei".into(), json!(imei));
                o.insert("seen".into(), json!(now_s()));
                if let Some(t) = details.get("type") {
                    o.insert("type".into(), t.clone());
                }
                if let Some(mo) = details.get("momsn") {
                    o.insert("momsn".into(), mo.clone());
                }
                let msgs = o.get("msgs").and_then(Value::as_u64).unwrap_or(0);
                o.insert("msgs".into(), json!(msgs + 1));
                if let Some(txt) = details.get("payload_text").and_then(Value::as_str) {
                    if !txt.trim().is_empty() {
                        o.insert("text".into(), json!(txt));
                    }
                }
            }
        }
        // UAT 978 MHz ADS-B downlink: a real aircraft — route through the same
        // aircraft merge so it lands on the map AND coalesces with 1090 ADS-B
        // by ICAO. (FIS-B uplink "fisb" carries weather, not an entity.)
        MessageBody::Uat { kind, details } if kind == "adsb" => {
            let icao = details.get("address").and_then(Value::as_str).map(|s| s.to_uppercase());
            let flight = details
                .get("callsign")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_uppercase())
                .filter(|s| !s.is_empty());
            let mut c = serde_json::Map::new();
            if let Some(a) = details.get("geometric_altitude").or_else(|| details.get("altitude")) {
                c.insert("alt".into(), a.clone());
            }
            if let Some(v) = details.get("ground_speed") {
                c.insert("spd".into(), v.clone());
            }
            if let Some(v) = details.get("true_track") {
                c.insert("trk".into(), v.clone());
            }
            let pos = match (
                details.get("lat").and_then(Value::as_f64),
                details.get("lon").and_then(Value::as_f64),
            ) {
                (Some(la), Some(lo)) => Some((la, lo)),
                _ => None,
            };
            if let Some((la, lo)) = pos {
                c.insert("lat".into(), json!(la));
                c.insert("lon".into(), json!(lo));
            }
            if icao.is_some() || flight.is_some() {
                merge_aircraft(d, "uat", AcIds { icao, flight, reg: None }, c, pos);
            }
        }
        // HFDL non-ACARS events: position-bearing HFNPDUs (performance-data /
        // frequency-data) carry an aircraft GPS fix, and the decoder back-fills
        // the ICAO from its logon cache (HFDL-3) onto both `who` and the nested
        // `position` object. Route any event with a resolved ICAO through the
        // aircraft merge so HFDL positions land on the map AND coalesce with
        // 1090/UAT ADS-B and ACARS by ICAO (XM-2.2). Uplink/pre-logon events
        // carry no resolved ICAO and are skipped.
        MessageBody::Hfdl { details, .. } => {
            let posobj = details.get("position");
            let pos = posobj.and_then(|p| {
                match (
                    p.get("lat").and_then(Value::as_f64),
                    p.get("lon").and_then(Value::as_f64),
                ) {
                    (Some(la), Some(lo)) => Some((la, lo)),
                    _ => None,
                }
            });
            let icao = posobj
                .and_then(|p| p.get("icao"))
                .or_else(|| details.get("who").and_then(|w| w.get("icao")))
                .or_else(|| details.get("icao"))
                .and_then(Value::as_str)
                .map(|s| s.to_uppercase())
                .filter(|s| s.len() == 6 && s != "000000");
            let flight = posobj
                .and_then(|p| p.get("flight"))
                .and_then(Value::as_str)
                .map(|s| s.trim().to_uppercase())
                .filter(|s| !s.is_empty());
            if let Some(icao) = icao {
                let mut c = serde_json::Map::new();
                if let Some((la, lo)) = pos {
                    c.insert("lat".into(), json!(la));
                    c.insert("lon".into(), json!(lo));
                }
                merge_aircraft(d, "hfdl", AcIds { icao: Some(icao), flight, reg: None }, c, pos);
            }
        }
        // Position-bearing beacons (radiosonde / ADS-L / COSPAS-SARSAT / DSC):
        // surface them on the map as their own layer (was silently dropped).
        MessageBody::Sonde { details, .. }
        | MessageBody::AdsL { details, .. }
        | MessageBody::Sarsat { details, .. }
        | MessageBody::Dsc { details, .. } => {
            let lat = details
                .get("lat")
                .or_else(|| details.get("latitude"))
                .and_then(Value::as_f64);
            let lon = details
                .get("lon")
                .or_else(|| details.get("longitude"))
                .and_then(Value::as_f64);
            if let (Some(lat), Some(lon)) = (lat, lon) {
                let id = ["serial", "address", "hex_id", "beacon_id", "from", "mmsi"]
                    .iter()
                    .find_map(|k| {
                        details.get(*k).map(|v| {
                            v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())
                        })
                    })
                    .unwrap_or_else(|| "?".into());
                let e = d.beacons.entry(format!("{mode}:{id}")).or_insert_with(|| json!({}));
                let o = e.as_object_mut().unwrap();
                o.insert("mode".into(), json!(mode));
                o.insert("id".into(), json!(id));
                o.insert("lat".into(), json!(lat));
                o.insert("lon".into(), json!(lon));
                if let Some(a) =
                    details.get("alt_m").or_else(|| details.get("altitude")).or_else(|| details.get("alt"))
                {
                    o.insert("alt".into(), a.clone());
                }
                o.insert("seen".into(), json!(now_s()));
                let msgs = o.get("msgs").and_then(Value::as_u64).unwrap_or(0);
                o.insert("msgs".into(), json!(msgs + 1));
                push_trail(o, lat, lon);
            }
        }
        // Rail EOT/HOT telemetry → a "train" entity keyed by unit address (no
        // GPS position — table-only). kind = eot (telemetry) | hot (command).
        MessageBody::Eot { kind, details } => {
            let unit = details
                .get("unit_addr")
                .map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))
                .filter(|u| !u.is_empty());
            if let Some(unit) = unit {
                let e = d.trains.entry(unit.clone()).or_insert_with(|| json!({}));
                let o = e.as_object_mut().unwrap();
                o.insert("unit".into(), json!(unit));
                o.insert("kind".into(), json!(kind));
                for (k, dk) in [
                    ("pressure_psi", "pressure"),
                    ("motion", "motion"),
                    ("marker_light", "marker"),
                    ("battery_charge_pct", "batt"),
                ] {
                    if let Some(v) = details.get(k) {
                        o.insert(dk.into(), v.clone());
                    }
                }
                o.insert("seen".into(), json!(now_s()));
                let n = o.get("msgs").and_then(Value::as_u64).unwrap_or(0);
                o.insert("msgs".into(), json!(n + 1));
            }
        }
        // FLEX / POCSAG paging → a "pager" entity keyed by proto+capcode (no
        // position). Both protocols share the dashboard's Pagers view.
        MessageBody::Flex { details, .. } | MessageBody::Pocsag { details, .. } => {
            if let Some(cap) = details.get("capcode") {
                let proto = m.mode.as_str();
                let e = d.pagers.entry(format!("{proto}:{cap}")).or_insert_with(|| json!({}));
                let o = e.as_object_mut().unwrap();
                o.insert("proto".into(), json!(proto));
                o.insert("capcode".into(), cap.clone());
                for (k, dk) in [("function", "function"), ("baud", "baud"), ("kind", "class")] {
                    if let Some(v) = details.get(k) {
                        o.insert(dk.into(), v.clone());
                    }
                }
                if let Some(t) = details.get("text").and_then(Value::as_str) {
                    let t = t.trim();
                    if !t.is_empty() {
                        o.insert("text".into(), json!(t));
                        // Per-capcode message history (newest last, capped) so a
                        // pager row can expand to show its past pages.
                        let kind = details.get("kind").cloned().unwrap_or(Value::Null);
                        let hist = o.entry("history").or_insert_with(|| json!([])).as_array_mut().unwrap();
                        hist.push(json!({ "text": t, "kind": kind, "seen": now_s() }));
                        if hist.len() > 25 {
                            hist.remove(0);
                        }
                    }
                }
                o.insert("seen".into(), json!(now_s()));
                let n = o.get("msgs").and_then(Value::as_u64).unwrap_or(0);
                o.insert("msgs".into(), json!(n + 1));
            }
        }
        _ => {}
    }
    }

    // Message stream entry: a one-line summary, plus the full decoded message
    // for the click-to-expand detail view. The high-rate Iridium link layer
    // (sync words, IDA link-access, inter-satellite ITL) is housekeeping that
    // floods the log and buries content frames (SBD/ACARS/ring-alert/MT) — it
    // still counts in the per-mode totals, but is kept out of the log feed.
    if is_link_housekeeping(m) {
        return;
    }
    let line = crate::outputs::console::format_message(m, crate::outputs::console::ConsoleFormat::Pretty);
    d.next_id += 1;
    d.recent.push_back(json!({
        "id": d.next_id,
        "t": m.timestamp.to_rfc3339(),
        "mode": mode,
        "freq": m.frequency_hz,
        "text": line,
        "detail": serde_json::to_value(m).unwrap_or(Value::Null),
    }));
    while d.recent.len() > RECENT_CAP {
        d.recent.pop_front();
    }
}

fn snapshot(d: &mut Dash) -> String {
    let cutoff = now_s().saturating_sub(EXPIRE_S);
    d.aircraft.retain(|_, v| v["seen"].as_u64().unwrap_or(0) >= cutoff);
    // Drop identifier-index entries whose master aircraft has expired.
    let live: std::collections::HashSet<String> = d.aircraft.keys().cloned().collect();
    d.ac_index.retain(|_, mid| live.contains(mid));
    d.vessels.retain(|_, v| v["seen"].as_u64().unwrap_or(0) >= cutoff);
    let sat_cut = now_s().saturating_sub(SAT_EXPIRE_S);
    let ring_cut = now_s().saturating_sub(RING_EXPIRE_S);
    d.iridium_sats.retain(|_, v| v["seen"].as_u64().unwrap_or(0) >= sat_cut);
    d.iridium_rings.retain(|_, v| v["seen"].as_u64().unwrap_or(0) >= ring_cut);
    d.iridium_devices.retain(|_, v| v["seen"].as_u64().unwrap_or(0) >= ring_cut);
    d.iridium_terminals.retain(|_, v| v["seen"].as_u64().unwrap_or(0) >= cutoff);
    d.beacons.retain(|_, v| v["seen"].as_u64().unwrap_or(0) >= cutoff);
    d.trains.retain(|_, v| v["seen"].as_u64().unwrap_or(0) >= cutoff);
    d.pagers.retain(|_, v| v["seen"].as_u64().unwrap_or(0) >= cutoff);
    // Persist the accumulated beam pattern occasionally so it survives
    // restarts and keeps refining across sessions.
    if now_s().saturating_sub(d.beams_saved) > 120 {
        d.beams.save(&crate::beam::BeamReconstructor::default_path());
        d.beams_saved = now_s();
    }
    json!({
        "station": d.station,
        "version": env!("CARGO_PKG_VERSION"),
        "started": d.started,
        "sessions": d.sessions,
        "aircraft": d.aircraft.values().collect::<Vec<_>>(),
        "vessels": d.vessels.values().collect::<Vec<_>>(),
        "beacons": d.beacons.values().collect::<Vec<_>>(),
        "trains": d.trains.values().collect::<Vec<_>>(),
        "pagers": d.pagers.values().collect::<Vec<_>>(),
        "iridium_sats": d.iridium_sats.values().collect::<Vec<_>>(),
        "iridium_rings": d.iridium_rings.values().collect::<Vec<_>>(),
        "iridium_devices": d.iridium_devices.values().collect::<Vec<_>>(),
        "iridium_terminals": d.iridium_terminals.values().collect::<Vec<_>>(),
        "iridium_beam_cells": d.beams.project(now_s() as f64, SAT_EXPIRE_S as f64),
        "messages": d.recent.iter().rev().take(RECENT_SENT).collect::<Vec<_>>(),
        "totals": d.totals,
        "last_seen": d.last_seen,
        "now": now_s(),
    })
    .to_string()
}

/// readsb/tar1090-compatible `aircraft.json`: every live aircraft entity that
/// has a real ICAO hex, mapped to the readsb field schema. Makes xng a
/// drop-in source for tar1090 / graphs1090 / VRS without Beast. (ECO-4)
fn aircraft_json(d: &Dash) -> String {
    let now = now_s();
    let cutoff = now.saturating_sub(EXPIRE_S);
    let messages: u64 = d.totals.values().sum();
    let copy = |o: &mut serde_json::Map<String, Value>, a: &Value, from: &str, to: &str| {
        if let Some(v) = a.get(from) {
            if !v.is_null() {
                o.insert(to.into(), v.clone());
            }
        }
    };
    let list: Vec<Value> = d
        .aircraft
        .values()
        .filter(|a| a.get("seen").and_then(Value::as_u64).unwrap_or(0) >= cutoff)
        .filter_map(|a| {
            let icao = a.get("icao").and_then(Value::as_str)?; // readsb needs a hex id
            let mut o = serde_json::Map::new();
            o.insert("hex".into(), json!(icao.to_lowercase()));
            copy(&mut o, a, "flight", "flight");
            copy(&mut o, a, "reg", "r");
            copy(&mut o, a, "actype", "t");
            copy(&mut o, a, "alt", "alt_baro");
            copy(&mut o, a, "spd", "gs");
            copy(&mut o, a, "trk", "track");
            copy(&mut o, a, "squawk", "squawk");
            copy(&mut o, a, "nacp", "nac_p");
            copy(&mut o, a, "sil", "sil");
            copy(&mut o, a, "adsb_version", "version");
            copy(&mut o, a, "sel_alt", "nav_altitude_mcp");
            if a.get("lat").is_some() && a.get("lon").is_some() {
                copy(&mut o, a, "lat", "lat");
                copy(&mut o, a, "lon", "lon");
            }
            copy(&mut o, a, "msgs", "messages");
            if let Some(seen) = a.get("seen").and_then(Value::as_u64) {
                let age = now.saturating_sub(seen);
                o.insert("seen".into(), json!(age));
                if a.get("lat").is_some() {
                    o.insert("seen_pos".into(), json!(age));
                }
            }
            Some(Value::Object(o))
        })
        .collect();
    json!({ "now": now, "messages": messages, "aircraft": list }).to_string()
}

/// readsb/tar1090 `receiver.json`: version, refresh cadence, and (if a session
/// reported `receiver-pos`) the receiver location so tar1090 centers the map.
fn receiver_json(d: &Dash) -> String {
    let mut o = serde_json::Map::new();
    o.insert("version".into(), json!(format!("xng-{}", env!("CARGO_PKG_VERSION"))));
    o.insert("refresh".into(), json!(1000));
    o.insert("history".into(), json!(0));
    for s in &d.sessions {
        if let Some(rp) = s.get("receiver_pos").and_then(Value::as_array) {
            if rp.len() == 2 {
                o.insert("lat".into(), rp[0].clone());
                o.insert("lon".into(), rp[1].clone());
                break;
            }
        }
    }
    Value::Object(o).to_string()
}

pub async fn run(
    mut rx: broadcast::Receiver<Arc<Message>>,
    addr: String,
    station: String,
    sessions: Vec<Value>,
) -> std::io::Result<()> {
    let mut dash = Dash { station, started: now_s(), sessions, ..Dash::default() };
    // Resume the accumulated 48-beam pattern across restarts.
    dash.beams = crate::beam::BeamReconstructor::load(&crate::beam::BeamReconstructor::default_path());
    let state = Arc::new(Mutex::new(dash));

    // HTTP listener.
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("dashboard on http://{addr}/");
    let http_state = state.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let state = http_state.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/");
                let (ctype, body) = if path.starts_with("/api/state") {
                    ("application/json", snapshot(&mut state.lock().unwrap()))
                } else if path.starts_with("/data/aircraft.json") {
                    ("application/json", aircraft_json(&state.lock().unwrap()))
                } else if path.starts_with("/data/receiver.json") {
                    ("application/json", receiver_json(&state.lock().unwrap()))
                } else {
                    ("text/html; charset=utf-8", PAGE.to_string())
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nCache-Control: no-cache, no-store, must-revalidate\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    // Bus consumer.
    loop {
        match rx.recv().await {
            Ok(msg) => update(&mut state.lock().unwrap(), &msg),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("dashboard lagged, dropped {n} messages");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use xng_types::{AppInfo, DecodeQuality, Mode, Provenance, StationIdentity};

    fn ring_alert(sat: u64, alt_km: f64) -> Message {
        Message {
            mode: Mode::Iridium,
            timestamp: chrono::Utc::now(),
            frequency_hz: 1_626_270_000,
            signal: Default::default(),
            decode: DecodeQuality { crc_ok: true, ..Default::default() },
            body: MessageBody::Iridium {
                kind: "ring-alert".into(),
                details: json!({
                    "sat": sat, "beam": 5, "lat": 40.0, "lon": -120.0,
                    "alt_km": alt_km, "satellite": "IRIDIUM 106"
                }),
            },
            raw: None,
            source: Provenance {
                station: StationIdentity::new("T"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        }
    }

    #[test]
    fn buckets_iridium_satellites_and_rings_by_altitude() {
        let mut d = Dash::default();
        update(&mut d, &ring_alert(44, 797.0)); // satellite altitude → sat
        update(&mut d, &ring_alert(77, 16.0)); // ground footprint → ring
        assert_eq!(d.iridium_sats.len(), 1);
        assert_eq!(d.iridium_rings.len(), 1);
        let snap: Value = serde_json::from_str(&snapshot(&mut d)).unwrap();
        assert_eq!(snap["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(snap["iridium_sats"][0]["sat"], 44);
        assert_eq!(snap["iridium_sats"][0]["name"], "IRIDIUM 106");
        assert_eq!(snap["iridium_rings"][0]["sat"], 77);
        assert_eq!(snap["iridium_rings"][0]["beam"], 5);
    }

    fn mt_position(lat: f64, lon: f64) -> Message {
        Message {
            mode: Mode::Iridium,
            timestamp: chrono::Utc::now(),
            frequency_hz: 1_622_000_000,
            signal: Default::default(),
            decode: DecodeQuality { crc_ok: true, ..Default::default() },
            body: MessageBody::Iridium {
                kind: "mt-position".into(),
                details: json!({
                    "type": "mt-position", "msg_type": "7605",
                    "lat": lat, "lon": lon, "alt_km": 0,
                }),
            },
            raw: None,
            source: Provenance {
                station: StationIdentity::new("T"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        }
    }

    #[test]
    fn maps_iridium_terminal_positions() {
        let mut d = Dash::default();
        update(&mut d, &mt_position(37.78, -122.50));
        update(&mut d, &mt_position(37.781, -122.501)); // same ~0.01° cell → coalesces
        update(&mut d, &mt_position(39.01, -123.79)); // distinct terminal
        assert_eq!(d.iridium_devices.len(), 2, "two distinct terminal cells");
        let snap: Value = serde_json::from_str(&snapshot(&mut d)).unwrap();
        assert_eq!(snap["iridium_devices"].as_array().unwrap().len(), 2);
        // mt-position must NOT leak into the beam-footprint (spot beam) layer.
        assert_eq!(snap["iridium_rings"].as_array().unwrap().len(), 0);
    }

    fn iridium(kind: &str, details: Value) -> Message {
        Message {
            mode: Mode::Iridium,
            timestamp: chrono::Utc::now(),
            frequency_hz: 1_622_000_000,
            signal: Default::default(),
            decode: DecodeQuality { crc_ok: true, ..Default::default() },
            body: MessageBody::Iridium { kind: kind.into(), details },
            raw: None,
            source: Provenance {
                station: StationIdentity::new("T"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        }
    }

    #[test]
    fn sbd_with_imei_surfaces_a_terminal_entity() {
        let mut d = Dash::default();
        update(&mut d, &iridium("sbd", json!({"type": "0600", "imei": "300034012295320", "momsn": 50815})));
        update(&mut d, &iridium("sbd", json!({"type": "0600", "imei": "300034012295320", "momsn": 50816})));
        // Same IMEI coalesces; msg count accrues.
        assert_eq!(d.iridium_terminals.len(), 1);
        let snap: Value = serde_json::from_str(&snapshot(&mut d)).unwrap();
        assert_eq!(snap["iridium_terminals"][0]["imei"], "300034012295320");
        assert_eq!(snap["iridium_terminals"][0]["msgs"], 2);
        // An SBD frame with no IMEI (transport control) creates no entity.
        update(&mut d, &iridium("sbd", json!({"type": "7608", "mtmsn": 3})));
        assert_eq!(d.iridium_terminals.len(), 1);
    }

    #[test]
    fn link_housekeeping_is_kept_out_of_the_log_but_counted() {
        let mut d = Dash::default();
        update(&mut d, &iridium("sync", json!({"sync_idle": true})));
        update(&mut d, &iridium("ida", json!({"len": 2})));
        update(&mut d, &iridium("itl", json!({})));
        update(&mut d, &iridium("ring-alert", json!({"sat": 5})));
        // sync/ida/itl are housekeeping: excluded from the log feed.
        assert_eq!(d.recent.len(), 1, "only the ring-alert reaches the log");
        // ...but all four still count toward the per-mode total.
        assert_eq!(*d.totals.get("iridium").unwrap(), 4);
    }

    fn acars(tail: &str, flight: &str) -> Message {
        Message {
            mode: Mode::AcarsPoa,
            timestamp: chrono::Utc::now(),
            frequency_hz: 131_550_000,
            signal: Default::default(),
            decode: DecodeQuality { crc_ok: true, ..Default::default() },
            body: MessageBody::Acars(xng_types::AcarsCore {
                tail: Some(tail.into()),
                flight: Some(flight.into()),
                label: "H1".into(),
                text: "hi".into(),
                ..Default::default()
            }),
            raw: None,
            source: Provenance {
                station: StationIdentity::new("T"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        }
    }

    #[test]
    fn acars_creates_flight_entity() {
        let mut d = Dash::default();
        update(&mut d, &acars("N12345", "UA123"));
        update(&mut d, &acars("N12345", "UA123")); // same tail coalesces, counts msgs
        assert_eq!(d.aircraft.len(), 1, "ACARS surfaces an aircraft entity");
        let snap: Value = serde_json::from_str(&snapshot(&mut d)).unwrap();
        let ac = &snap["aircraft"][0];
        assert_eq!(ac["id"], "N12345"); // keyed by tail (no ICAO from ACARS)
        assert_eq!(ac["reg"], "N12345");
        assert_eq!(ac["flight"], "UA123");
        assert_eq!(ac["msgs"], 2);
    }

    #[test]
    fn crc_failed_message_creates_no_aircraft() {
        let mut d = Dash::default();
        let mut m = acars("N99999", "XX999");
        m.decode.crc_ok = false;
        update(&mut d, &m);
        // The bug fix: a corrupt frame must not plant a junk entity...
        assert_eq!(d.aircraft.len(), 0);
        // ...but it still counts toward the per-mode total.
        assert_eq!(*d.totals.get("acars").unwrap(), 1);
    }

    #[test]
    fn aircraft_merges_across_carrier_sources() {
        let mut d = Dash::default();
        // Same tail heard via VHF ACARS and over VDL2 → one entity, two sources.
        update(&mut d, &acars("N12345", "UA100"));
        let mut via_vdl2 = acars("N12345", "UA100");
        via_vdl2.mode = Mode::Vdl2;
        update(&mut d, &via_vdl2);
        assert_eq!(d.aircraft.len(), 1, "carriers coalesce by tail");
        let snap: Value = serde_json::from_str(&snapshot(&mut d)).unwrap();
        let ac = &snap["aircraft"][0];
        assert_eq!(ac["reg"], "N12345");
        assert_eq!(ac["msgs"], 2, "total across sources");
        let srcs = ac["sources"].as_object().unwrap();
        assert!(srcs.contains_key("acars") && srcs.contains_key("vdl2"), "two source rows");
        assert_eq!(srcs["acars"]["msgs"], 1);
        assert_eq!(srcs["vdl2"]["msgs"], 1);
    }

    fn msg(mode: Mode, body: MessageBody) -> Message {
        Message {
            mode,
            timestamp: chrono::Utc::now(),
            frequency_hz: 978_000_000,
            signal: Default::default(),
            decode: DecodeQuality { crc_ok: true, ..Default::default() },
            body,
            raw: None,
            source: Provenance {
                station: StationIdentity::new("T"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        }
    }

    #[test]
    fn uat_surfaces_as_aircraft_and_merges_with_adsb_by_icao() {
        let mut d = Dash::default();
        // ADS-B (1090) by ICAO, then UAT (978) for the same aircraft (lowercase
        // address) → one merged aircraft with two sources, plotted on the map.
        update(&mut d, &msg(Mode::Adsb, MessageBody::ModeS {
            df: 17, icao: Some("AC82EC".into()), callsign: None, altitude_ft: Some(35000),
            squawk: None, lat: Some(40.0), lon: Some(-120.0), speed_kt: None, speed_type: None,
            track_deg: None, vertical_rate_fpm: None, comm_b: None, adsb_status: None,
        }));
        update(&mut d, &msg(Mode::Uat, MessageBody::Uat {
            kind: "adsb".into(),
            details: json!({"address": "ac82ec", "callsign": "N5130E", "lat": 40.01, "lon": -120.01, "geometric_altitude": 34000, "ground_speed": 120}),
        }));
        assert_eq!(d.aircraft.len(), 1, "UAT coalesces with ADS-B by ICAO");
        let snap: Value = serde_json::from_str(&snapshot(&mut d)).unwrap();
        let ac = &snap["aircraft"][0];
        assert_eq!(ac["icao"], "AC82EC");
        assert!(ac["lat"].is_number(), "has a map position");
        let srcs = ac["sources"].as_object().unwrap();
        assert!(srcs.contains_key("adsb") && srcs.contains_key("uat"), "two sources");
    }

    #[test]
    fn aircraft_json_emits_readsb_schema() {
        let mut d = Dash::default();
        update(&mut d, &msg(Mode::Adsb, MessageBody::ModeS {
            df: 17, icao: Some("AC82EC".into()), callsign: Some("N5130E".into()),
            altitude_ft: Some(35000), squawk: Some("1200".into()),
            lat: Some(40.0), lon: Some(-120.0), speed_kt: Some(420.0), speed_type: Some("GS".into()),
            track_deg: Some(270.0), vertical_rate_fpm: None, comm_b: None, adsb_status: None,
        }));
        let v: Value = serde_json::from_str(&aircraft_json(&d)).unwrap();
        assert!(v["now"].is_number() && v["messages"].is_number());
        let a = &v["aircraft"][0];
        assert_eq!(a["hex"], "ac82ec", "hex is lowercased ICAO");
        assert_eq!(a["flight"], "N5130E");
        assert_eq!(a["alt_baro"], 35000);
        assert_eq!(a["gs"], 420.0);
        assert_eq!(a["squawk"], "1200");
        assert!(a["lat"].is_number() && a["lon"].is_number() && a["seen_pos"].is_number());
        // receiver.json carries version + (no position configured here).
        let r: Value = serde_json::from_str(&receiver_json(&d)).unwrap();
        assert!(r["version"].as_str().unwrap().starts_with("xng-"));
    }

    #[test]
    fn hfdl_position_surfaces_as_aircraft_and_merges_by_icao() {
        let mut d = Dash::default();
        // ADS-B (1090) by ICAO, then an HFDL performance-data position report for
        // the same aircraft (ICAO back-filled by the decoder onto the position
        // object) → one merged aircraft, two sources, plotted on the map.
        update(&mut d, &msg(Mode::Adsb, MessageBody::ModeS {
            df: 17, icao: Some("40612F".into()), callsign: None, altitude_ft: Some(38000),
            squawk: None, lat: Some(51.0), lon: Some(0.5), speed_kt: None, speed_type: None,
            track_deg: None, vertical_rate_fpm: None, comm_b: None, adsb_status: None,
        }));
        update(&mut d, &msg(Mode::Hfdl, MessageBody::Hfdl {
            kind: "performance-data".into(),
            details: json!({
                "who": {"dir": "downlink", "gs_id": 1, "aircraft_id": 0x42, "icao": "40612F"},
                "position": {"lat": 51.02, "lon": 0.55, "utc_s": 3600, "flight": "BA117", "icao": "40612F"},
            }),
        }));
        assert_eq!(d.aircraft.len(), 1, "HFDL coalesces with ADS-B by ICAO");
        let snap: Value = serde_json::from_str(&snapshot(&mut d)).unwrap();
        let ac = &snap["aircraft"][0];
        assert_eq!(ac["icao"], "40612F");
        assert_eq!(ac["flight"], "BA117", "flight from the HFDL position report");
        assert!(ac["lat"].is_number(), "HFDL fix gives a map position");
        let srcs = ac["sources"].as_object().unwrap();
        assert!(srcs.contains_key("adsb") && srcs.contains_key("hfdl"), "two sources");
        // An HFDL event with no resolved ICAO (uplink/pre-logon) plots nothing.
        update(&mut d, &msg(Mode::Hfdl, MessageBody::Hfdl {
            kind: "squitter".into(),
            details: json!({"gs_id": 1, "gs_name": "San Francisco, USA"}),
        }));
        assert_eq!(d.aircraft.len(), 1, "ICAO-less HFDL event adds no aircraft");
    }

    #[test]
    fn sonde_creates_a_map_beacon() {
        let mut d = Dash::default();
        update(&mut d, &msg(Mode::Sonde, MessageBody::Sonde {
            kind: "rs41".into(),
            details: json!({"serial": "R1234567", "lat": -34.72, "lon": 138.69, "alt_m": 12000.0}),
        }));
        assert_eq!(d.beacons.len(), 1, "radiosonde becomes a beacon entity");
        let snap: Value = serde_json::from_str(&snapshot(&mut d)).unwrap();
        let b = &snap["beacons"][0];
        assert_eq!(b["mode"], "sonde");
        assert_eq!(b["id"], "R1234567");
        assert!(b["lat"].is_number() && b["lon"].is_number());
        // A beacon with no position (e.g. a DSC routine call) creates nothing.
        update(&mut d, &msg(Mode::Dsc, MessageBody::Dsc { kind: "individual".into(), details: json!({"from": 1234}) }));
        assert_eq!(d.beacons.len(), 1, "no-position message plots no beacon");
    }

    #[test]
    fn eot_creates_a_train_entity() {
        let mut d = Dash::default();
        update(&mut d, &msg(Mode::Eot, MessageBody::Eot {
            kind: "eot".into(),
            details: json!({"unit_addr": 96147, "pressure_psi": 29, "motion": 1, "marker_light": 0}),
        }));
        update(&mut d, &msg(Mode::Eot, MessageBody::Eot {
            kind: "eot".into(),
            details: json!({"unit_addr": 96147, "pressure_psi": 31, "motion": 1, "marker_light": 0}),
        }));
        assert_eq!(d.trains.len(), 1, "same unit coalesces");
        let snap: Value = serde_json::from_str(&snapshot(&mut d)).unwrap();
        let t = &snap["trains"][0];
        assert_eq!(t["unit"], "96147");
        assert_eq!(t["pressure"], 31, "latest pressure wins");
        assert_eq!(t["msgs"], 2);
    }

    #[test]
    fn flex_and_pocsag_create_pager_entities() {
        let mut d = Dash::default();
        update(&mut d, &msg(Mode::Flex, MessageBody::Flex {
            kind: "alpha".into(),
            details: json!({"capcode": 1234567, "function": 3, "baud": 1600, "text": "PAGE TEXT"}),
        }));
        update(&mut d, &msg(Mode::Pocsag, MessageBody::Pocsag {
            kind: "numeric".into(),
            details: json!({"capcode": 1234567, "function": 0, "baud": 1200, "text": "12345"}),
        }));
        // Same capcode, different protocol → two distinct pager entities.
        assert_eq!(d.pagers.len(), 2, "flex + pocsag keyed separately by proto");
        let snap: Value = serde_json::from_str(&snapshot(&mut d)).unwrap();
        let protos: Vec<&str> = snap["pagers"].as_array().unwrap().iter()
            .map(|p| p["proto"].as_str().unwrap()).collect();
        assert!(protos.contains(&"flex") && protos.contains(&"pocsag"));
    }
}
