//! Pick airband channels to feed from the ATC catalog near the receiver
//! (`--channels auto`, `xng atc nearby`).
//!
//! Asks api-server `GET /v1/atc/frequencies?near=` for the VHF frequencies
//! around the station, keeps voice channels worth feeding (tower, approach,
//! departure, ground, delivery, center, ...; not ATIS/AWOS/VOLMET, which
//! would hold the squelch open all day), scores each by service and
//! distance, and takes the slice of the band one SDR can cover that holds
//! the most useful channels. Each pick carries its catalog key and a label,
//! so the feed is linked to the catalog without a lookup. Same scoring as
//! aggregator-atc's atc-feed-nearby.py.

use serde::Deserialize;
use std::time::Duration;

pub const AIRBAND_LO: u64 = 118_000_000;
pub const AIRBAND_HI: u64 = 137_000_000;

/// One channel to feed.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    pub freq_hz: u64,
    pub id: String,
    pub label: String,
    pub service: String,
    pub distance_km: Option<f64>,
    score: f64,
}

impl Pick {
    #[cfg(test)]
    pub(crate) fn for_test(freq_hz: u64, id: &str, label: &str) -> Pick {
        Pick { freq_hz, id: id.into(), label: label.into(), service: "TWR".into(), distance_km: None, score: 1.0 }
    }
}

/// What `--channels auto` tunes.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub center_hz: u64,
    pub channels: Vec<Pick>,
}

/// How much a channel is worth.
fn service_weight(svc: &str) -> f64 {
    match svc {
        "TWR" => 10.0,
        "APP" | "DEP" => 8.0,
        "GND" => 6.0,
        "CTR" => 5.0,
        "CLD" => 4.0,
        "AFIS" => 3.0,
        "CTAF" => 1.5,
        "RAMP" | "OPS" | "FSS" => 1.0,
        "UNICOM" => 0.5,
        _ => 0.5,
    }
}

/// How far a service usually carries (km).
fn service_range_km(svc: &str) -> f64 {
    match svc {
        "GND" | "CLD" => 25.0,
        "RAMP" => 15.0,
        "OPS" => 40.0,
        "TWR" | "CTAF" | "UNICOM" | "AFIS" => 50.0,
        "APP" | "DEP" => 120.0,
        "FSS" => 150.0,
        "CTR" => 250.0,
        _ => 60.0,
    }
}

/// Continuous broadcasts and channels not worth a feed.
fn skipped(svc: &str) -> bool {
    matches!(svc, "ATIS" | "AWOS" | "ASOS" | "VOLMET" | "EMERG" | "MIL")
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    data: Vec<Row>,
    #[serde(default)]
    total: u64,
    #[serde(default)]
    limit: u64,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    id: String,
    #[serde(default)]
    frequency_hz: f64,
    #[serde(default)]
    service: Option<String>,
    #[serde(default)]
    modulation: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    callsign: Option<String>,
    #[serde(default)]
    streamable: Option<bool>,
    #[serde(default)]
    distance_km: Option<f64>,
    #[serde(default)]
    facility: Option<Facility>,
    #[serde(default)]
    facility_id: Option<String>,
}

#[derive(Clone, Deserialize)]
struct Facility {
    #[serde(default)]
    ident: Option<String>,
}

/// Fetch the VHF frequencies within `radius_nm` of (lat, lon). Blocking.
pub fn fetch(api: &str, lat: f64, lon: f64, radius_nm: u32) -> anyhow::Result<Vec<Row>> {
    let url = format!("{}/v1/atc/frequencies", api.trim_end_matches('/'));
    let mut rows = Vec::new();
    for page in 1..=20u32 {
        let resp = ureq::get(&url)
            .timeout(Duration::from_secs(20))
            .query("near", &format!("{lat:.3},{lon:.3}"))
            .query("radius_nm", &radius_nm.to_string())
            .query("band", "VHF")
            .query("status", "active")
            .query("limit", "500")
            .query("page", &page.to_string())
            .set("Accept", "application/json")
            .set("User-Agent", concat!("xng/", env!("CARGO_PKG_VERSION")))
            .call();
        let body = match resp {
            Ok(r) => r.into_string()?,
            Err(ureq::Error::Status(code, r)) => {
                anyhow::bail!(
                    "{url}: HTTP {code} {}",
                    r.into_string().unwrap_or_default().chars().take(200).collect::<String>()
                )
            }
            Err(e) => return Err(e.into()),
        };
        let p: Page = serde_json::from_str(&body)?;
        let n = p.data.len() as u64;
        rows.extend(p.data);
        if n == 0 || page as u64 * p.limit.max(1) >= p.total {
            break;
        }
    }
    Ok(rows)
}

/// Parse one page of the list endpoint (tests).
pub fn parse_rows(body: &str) -> anyhow::Result<Vec<Row>> {
    Ok(serde_json::from_str::<Page>(body)?.data)
}

fn label(r: &Row, svc: &str) -> String {
    let ident = r
        .facility
        .as_ref()
        .and_then(|f| f.ident.clone())
        .or_else(|| r.facility_id.as_ref().and_then(|f| f.rsplit(':').next().map(str::to_string)))
        .unwrap_or_default();
    let mut text = format!("{ident} {svc}").trim().to_string();
    if let Some(n) = r.name.as_ref().or(r.callsign.as_ref()).filter(|n| !n.is_empty()) {
        if !text.to_uppercase().contains(&n.to_uppercase()) {
            text.push_str(&format!(" ({n})"));
        }
    }
    // Commas split --labels lists.
    text.replace(',', " ").chars().take(48).collect()
}

/// Channels worth feeding, one per carrier (the best-scoring entry),
/// sorted by frequency.
pub fn candidates(rows: &[Row], services: Option<&[String]>) -> Vec<Pick> {
    let mut best: Vec<Pick> = Vec::new();
    for r in rows {
        let hz = r.frequency_hz.round() as u64;
        let svc = r.service.clone().unwrap_or_default().to_ascii_uppercase();
        if !(AIRBAND_LO..AIRBAND_HI).contains(&hz)
            || !r.modulation.as_deref().unwrap_or("AM").eq_ignore_ascii_case("AM")
            || r.streamable == Some(false)
            || skipped(&svc)
            || services.is_some_and(|s| !s.iter().any(|x| x.eq_ignore_ascii_case(&svc)))
        {
            continue;
        }
        let range = service_range_km(&svc);
        let score = service_weight(&svc) * (-r.distance_km.unwrap_or(range) / range).exp();
        let p = Pick {
            freq_hz: hz,
            id: r.id.clone(),
            label: label(r, &svc),
            service: svc,
            distance_km: r.distance_km,
            score,
        };
        match best.iter_mut().find(|b| b.freq_hz == hz) {
            Some(b) if b.score < p.score => *b = p,
            Some(_) => {}
            None => best.push(p),
        }
    }
    best.sort_by_key(|p| p.freq_hz);
    best
}

/// How much each further channel of a service is worth relative to the
/// one before it, so a cluster of CTAFs doesn't outrank a tower plus
/// approach.
fn decay(svc: &str) -> f64 {
    if matches!(svc, "CTAF" | "UNICOM" | "GND" | "CLD") {
        0.5
    } else {
        0.8
    }
}

/// The best `max_channels` of `chans` and their value: greedily the
/// channel adding the most (its score times its service's decay for each
/// channel of that service already taken). Each service's marginal values
/// only fall, so greedy is optimal here.
fn best_subset<'a>(chans: &[&'a Pick], max_channels: usize) -> (Vec<&'a Pick>, f64) {
    let mut left: Vec<&Pick> = chans.to_vec();
    let mut taken: Vec<&Pick> = Vec::new();
    let mut total = 0.0;
    while taken.len() < max_channels && !left.is_empty() {
        let gain = |p: &Pick| {
            let n = taken.iter().filter(|t| t.service == p.service).count() as i32;
            p.score * decay(&p.service).powi(n)
        };
        let (i, g) = left
            .iter()
            .enumerate()
            .map(|(i, p)| (i, gain(p)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .expect("left is not empty");
        total += g;
        taken.push(left.swap_remove(i));
    }
    (taken, total)
}

/// The slice of the band (`span_hz` wide) whose best `max_channels`
/// channels score highest, sorted by frequency.
pub fn pick_window(cands: &[Pick], span_hz: u64, max_channels: usize) -> Vec<Pick> {
    let mut best: Vec<&Pick> = Vec::new();
    let mut best_total = 0.0;
    for (i, lo) in cands.iter().enumerate() {
        let inside: Vec<&Pick> = cands[i..].iter().filter(|p| p.freq_hz - lo.freq_hz <= span_hz).collect();
        let (taken, total) = best_subset(&inside, max_channels);
        if total > best_total {
            best = taken;
            best_total = total;
        }
    }
    let mut out: Vec<Pick> = best.into_iter().cloned().collect();
    out.sort_by_key(|p| p.freq_hz);
    out
}

/// Keep the center this far from any carrier (the DC spike).
const DC_CLEAR_HZ: u64 = 10_000;
/// How far the center may move off the middle to clear a carrier; the
/// window is narrowed by twice this so every channel stays in the span.
const CENTER_SLACK_HZ: u64 = 25_000;

/// A center between the outer channels, as near the middle as possible,
/// at least [`DC_CLEAR_HZ`] from every carrier (else the clearest spot)
/// and within [`CENTER_SLACK_HZ`] of the middle.
fn center_for(channels: &[u64]) -> u64 {
    let lo = *channels.iter().min().expect("channels");
    let hi = *channels.iter().max().expect("channels");
    let mid = (lo + hi) / 2;
    let clearance = |c: u64| channels.iter().map(|f| f.abs_diff(c)).min().unwrap_or(u64::MAX);
    let mut best = (mid, clearance(mid));
    for step in 0..=(CENTER_SLACK_HZ / 1_000) {
        for c in [mid + step * 1_000, mid.saturating_sub(step * 1_000)] {
            let cl = clearance(c);
            if cl >= DC_CLEAR_HZ {
                return c;
            }
            if cl > best.1 {
                best = (c, cl);
            }
        }
    }
    best.0
}

/// Plan a capture at `sample_rate`: the best window within 75% of the
/// bandwidth (the edges of an RTL-SDR's passband are poor), centered off
/// any channel's carrier. With `center_hz` (the user's -c), only channels
/// within that capture are considered and the center is kept.
pub fn plan(
    rows: &[Row],
    sample_rate: f64,
    max_channels: usize,
    services: Option<&[String]>,
    center_hz: Option<u64>,
) -> Option<Plan> {
    plan_from(candidates(rows, services), sample_rate, max_channels, center_hz)
}

/// Plans for `sdrs` captures at `sample_rate`, best first: each SDR takes
/// the best window of the channels the ones before it left, so no channel
/// is fed twice. Stops early when nothing is left.
pub fn plan_many(
    rows: &[Row],
    sample_rate: f64,
    max_channels: usize,
    services: Option<&[String]>,
    sdrs: usize,
) -> Vec<Plan> {
    let mut cands = candidates(rows, services);
    let mut plans = Vec::new();
    for _ in 0..sdrs {
        let Some(p) = plan_from(cands.clone(), sample_rate, max_channels, None) else {
            break;
        };
        cands.retain(|c| !p.channels.iter().any(|t| t.freq_hz == c.freq_hz));
        plans.push(p);
    }
    plans
}

/// How wide a slice of the band one capture at `sample_rate` picks
/// channels from when xng chooses the center.
pub fn window_hz(sample_rate: f64) -> u64 {
    ((sample_rate * 0.75) as u64).saturating_sub(2 * CENTER_SLACK_HZ)
}

fn plan_from(mut cands: Vec<Pick>, sample_rate: f64, max_channels: usize, center_hz: Option<u64>) -> Option<Plan> {
    let span = (sample_rate * 0.75) as u64;
    let window = match center_hz {
        Some(c) => {
            // Inside that capture, and clear of the center's DC spike.
            cands.retain(|p| p.freq_hz.abs_diff(c) <= span / 2 && p.freq_hz.abs_diff(c) >= DC_CLEAR_HZ);
            span
        }
        // Room for the center to move off a carrier.
        None => span.saturating_sub(2 * CENTER_SLACK_HZ),
    };
    let channels = pick_window(&cands, window, max_channels.max(1));
    if channels.is_empty() {
        return None;
    }
    let hz: Vec<u64> = channels.iter().map(|p| p.freq_hz).collect();
    Some(Plan { center_hz: center_hz.unwrap_or_else(|| center_for(&hz)), channels })
}

/// Where the receiver is and what to look for.
#[derive(Clone, Debug)]
pub struct Query {
    pub api: String,
    pub lat: f64,
    pub lon: f64,
    pub radius_nm: u32,
    pub sample_rate: f64,
    pub max_channels: usize,
    pub services: Vec<String>,
    pub center_hz: Option<u64>,
}

/// Fetch and plan. Blocking. Errors when the catalog has nothing to feed.
pub fn resolve(q: &Query) -> anyhow::Result<Plan> {
    anyhow::ensure!(
        (-90.0..=90.0).contains(&q.lat) && (-180.0..=180.0).contains(&q.lon),
        "invalid receiver position {},{}",
        q.lat,
        q.lon
    );
    let rows = fetch(&q.api, q.lat, q.lon, q.radius_nm)?;
    let services = (!q.services.is_empty()).then_some(q.services.as_slice());
    plan(&rows, q.sample_rate, q.max_channels, services, q.center_hz).ok_or_else(|| {
        anyhow::anyhow!(
            "no ATC channels to feed within {} nm of {:.3},{:.3} ({} catalog entries); \
             try a larger --atc-nearby-radius-nm or pass --channels yourself",
            q.radius_nm,
            q.lat,
            q.lon,
            rows.len()
        )
    })
}

/// One line per pick, for logs and `xng atc nearby`.
pub fn describe(p: &Pick) -> String {
    let km = p.distance_km.map(|d| format!("{:>6.1} km", d)).unwrap_or_else(|| "      ? km".into());
    format!("{:.3}  {km}  {:<5} {:<34} {}", p.freq_hz as f64 / 1e6, p.service, p.label, p.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, mhz: f64, svc: &str, km: f64) -> Row {
        Row {
            id: id.into(),
            frequency_hz: mhz * 1e6,
            service: Some(svc.into()),
            modulation: Some("AM".into()),
            name: None,
            callsign: None,
            streamable: Some(true),
            distance_km: Some(km),
            facility: Some(Facility { ident: id.split(':').nth(1).map(str::to_string) }),
            facility_id: None,
        }
    }

    #[test]
    fn picks_the_busiest_slice() {
        let rows = vec![
            row("airport:KSFO:TWR:120500", 120.5, "TWR", 2.0),
            row("airport:KSFO:GND:121800", 121.8, "GND", 2.0),
            row("airport:KSFO:ATIS:118850", 118.85, "ATIS", 2.0),
            row("airport:KOAK:TWR:118300", 118.3, "TWR", 18.0),
            row("tracon:NCT:APP:135650", 135.65, "APP", 30.0),
            row("airport:KXYZ:CTAF:122800", 122.8, "CTAF", 40.0),
        ];
        let p = plan(&rows, 2_400_000.0, 8, None, None).unwrap();
        let ids: Vec<&str> = p.channels.iter().map(|c| c.id.as_str()).collect();
        // 120.5 + 121.8 (+ CTAF 122.8 still fits 1.8 MHz from 121.0..) beat
        // OAK alone; ATIS is never picked.
        assert!(ids.contains(&"airport:KSFO:TWR:120500"), "{ids:?}");
        assert!(ids.contains(&"airport:KSFO:GND:121800"), "{ids:?}");
        assert!(!ids.iter().any(|i| i.contains("ATIS")));
        let lo = p.channels.first().unwrap().freq_hz;
        let hi = p.channels.last().unwrap().freq_hz;
        assert!(hi - lo <= 1_800_000);
        assert!(p.center_hz >= lo && p.center_hz <= hi);
        assert!(!p.channels.iter().any(|c| c.freq_hz == p.center_hz));
        assert_eq!(p.channels[0].label, "KSFO TWR");
    }

    #[test]
    fn filters_and_dedupes() {
        let mut no = row("airport:EGLL:TWR:118500", 118.5, "TWR", 1.0);
        no.streamable = Some(false);
        let far = row("airport:KAAA:TWR:119100", 119.1, "TWR", 45.0);
        let near = row("airport:KBBB:TWR:119100", 119.1, "TWR", 5.0);
        let c = candidates(&[no, far, near], None);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].id, "airport:KBBB:TWR:119100");
        let only_gnd = candidates(&[row("a:X:TWR:1", 119.1, "TWR", 1.0)], Some(&["GND".to_string()]));
        assert!(only_gnd.is_empty());
        assert!(plan(&[], 2_400_000.0, 8, None, None).is_none());
        // With -c, only channels inside that capture.
        let rows = [row("a:A:TWR:1", 120.5, "TWR", 1.0), row("a:B:TWR:2", 127.0, "TWR", 1.0)];
        let p = plan(&rows, 2_400_000.0, 8, None, Some(126_500_000)).unwrap();
        assert_eq!(p.center_hz, 126_500_000);
        assert_eq!(p.channels.len(), 1);
        assert_eq!(p.channels[0].freq_hz, 127_000_000);
    }

    #[test]
    fn subset_counts_the_service_decay() {
        // Two towers by raw score lose to approach + the better tower.
        let rows = [
            row("a:A:APP:1", 118.0, "APP", 0.0),
            row("a:B:TWR:2", 118.1, "TWR", 0.0),
            row("a:C:TWR:3", 118.2, "TWR", 1.0),
        ];
        let p = plan(&rows, 2_400_000.0, 2, None, None).unwrap();
        let ids: Vec<&str> = p.channels.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["a:A:APP:1", "a:B:TWR:2"]);
    }

    #[test]
    fn center_clears_every_carrier_and_keeps_the_span() {
        // The middle and middle+25 kHz are both carriers.
        let c = center_for(&[120_000_000, 120_025_000, 120_050_000]);
        assert!([120_000_000u64, 120_025_000, 120_050_000].iter().all(|f| f.abs_diff(c) >= DC_CLEAR_HZ), "{c}");
        // Channels spanning the whole window stay within half the span.
        let rows = [
            row("a:A:TWR:1", 120.0, "TWR", 0.0),
            row("a:B:TWR:2", 120.9, "TWR", 0.0),
            row("a:C:TWR:3", 121.8, "TWR", 0.0),
        ];
        let p = plan(&rows, 2_400_000.0, 8, None, None).unwrap();
        for ch in &p.channels {
            assert!(ch.freq_hz.abs_diff(p.center_hz) <= 900_000, "{} vs {}", ch.freq_hz, p.center_hz);
            assert!(ch.freq_hz.abs_diff(p.center_hz) >= DC_CLEAR_HZ);
        }
    }

    #[test]
    fn several_sdrs_split_the_band_without_repeats() {
        let rows = [
            row("a:A:TWR:1", 119.4, "TWR", 1.0),
            row("a:A:GND:2", 120.1, "GND", 1.0),
            row("a:B:APP:3", 125.3, "APP", 5.0),
            row("a:B:APP:4", 126.0, "APP", 5.0),
            row("a:C:TWR:5", 125.7, "TWR", 9.0),
            row("a:D:CTR:6", 132.0, "CTR", 60.0),
        ];
        let plans = plan_many(&rows, 2_400_000.0, 8, None, 2);
        assert_eq!(plans.len(), 2);
        let mut seen: Vec<u64> = plans.iter().flat_map(|p| p.channels.iter().map(|c| c.freq_hz)).collect();
        let n = seen.len();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), n, "a channel was planned twice");
        for p in &plans {
            for c in &p.channels {
                assert!(c.freq_hz.abs_diff(p.center_hz) <= 900_000);
            }
        }
        // The 125-126 MHz cluster scores highest, so the first SDR gets it.
        assert!(plans[0].channels.iter().any(|c| c.id == "a:B:APP:3"));
        // More SDRs than windows: stops when the channels run out.
        assert_eq!(plan_many(&rows[..1], 2_400_000.0, 8, None, 3).len(), 1);
    }

    #[test]
    fn parses_the_list_endpoint() {
        let body = r#"{"data":[{"id":"airport:KSFO:TWR:120500","facilityId":"airport:KSFO","frequencyHz":120500000,
            "service":"TWR","modulation":"AM","name":"SFO TWR","streamable":true,"distanceKm":2.1,"distanceNm":1.13,
            "facility":{"id":"airport:KSFO","type":"airport","ident":"KSFO","name":"San Francisco Intl","country":"US"}}],
            "total":1,"page":1,"limit":500}"#;
        let rows = parse_rows(body).unwrap();
        let c = candidates(&rows, None);
        assert_eq!(c[0].freq_hz, 120_500_000);
        assert_eq!(c[0].label, "KSFO TWR");
    }
}
