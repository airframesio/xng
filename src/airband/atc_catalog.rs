//! Match airband channels to the Airframes ATC frequency catalog.
//!
//! Listeners find feeds through the catalog (`airport:KSFO:TWR:120500`), so
//! every fed channel should name its catalog entry. Instead of making the
//! user look keys up, the feeder asks api-server's
//! `GET /v1/atc/frequencies/match` for each tuned carrier near the station
//! and takes the best candidate when it is clear (the same rule the ingest
//! server applies to a bare `freq_hz`). When several entries share a
//! carrier nearby, the channel's label breaks the tie ("KSFO Tower" picks
//! the KSFO tower entry). A configured `atc-frequency` always wins.
//!
//! The lookup also brings the entry's name, used as the feed label when the
//! channel has none, and its streaming policy, so a frequency the network
//! does not allow is skipped here rather than rejected (which on a scan feed
//! would reject every frequency with it).

use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

/// One catalog entry returned by the match endpoint.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub freq_hz: u64,
    pub service: String,
    pub name: Option<String>,
    pub callsign: Option<String>,
    pub facility_ident: Option<String>,
    pub facility_name: Option<String>,
    pub streamable: bool,
    pub policy_reason: Option<String>,
    pub distance_km: f64,
    pub confidence: f64,
}

impl Candidate {
    /// Short listener label, "KSFO TWR (SAN FRANCISCO TOWER)" (the same form
    /// atc-feed-nearby.py writes into station files).
    pub fn label(&self) -> String {
        let ident =
            self.facility_ident.clone().unwrap_or_else(|| self.id.split(':').nth(1).unwrap_or_default().to_string());
        let mut text = format!("{ident} {}", self.service).trim().to_string();
        if let Some(n) = self.name.as_ref().or(self.callsign.as_ref()).filter(|n| !n.is_empty()) {
            if !text.to_uppercase().contains(&n.to_uppercase()) {
                text.push_str(&format!(" ({n})"));
            }
        }
        text.chars().take(48).collect()
    }
}

/// What the catalog says about one channel.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    /// A clear match.
    Matched(Candidate),
    /// The match is clear but the network does not allow streaming it.
    NotAllowed(Candidate),
    /// Several entries fit about equally; the label did not settle it.
    Ambiguous(Vec<Candidate>),
    /// No catalog entry on this carrier near the station.
    NoMatch,
}

/// Pick a candidate from a ranked list (best first). Clear means the only
/// candidate, or confidence >= 0.5 and at least 0.2 ahead of the next, as
/// aggregator-atc accepts a bare frequency. Otherwise words of `hint` (the
/// channel label) that name the facility, the service or the entry pick one.
pub fn pick(cands: &[Candidate], hint: Option<&str>) -> Resolution {
    let Some(first) = cands.first() else {
        return Resolution::NoMatch;
    };
    let clear = cands.len() == 1 || (first.confidence >= 0.5 && first.confidence - cands[1].confidence >= 0.2);
    let chosen = if clear {
        Some(first)
    } else {
        hint.and_then(|h| {
            let words = words(h);
            let mut scored: Vec<(f64, &Candidate)> = cands.iter().map(|c| (hint_score(c, &words), c)).collect();
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            match scored.as_slice() {
                [(s, c), rest @ ..] if *s > 0.0 && rest.first().is_none_or(|(n, _)| s > n) => Some(*c),
                _ => None,
            }
        })
    };
    match chosen {
        Some(c) if c.streamable => Resolution::Matched(c.clone()),
        Some(c) => Resolution::NotAllowed(c.clone()),
        None => Resolution::Ambiguous(cands.iter().take(5).cloned().collect()),
    }
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.to_ascii_uppercase()).collect()
}

/// Words that name each service in labels people write.
fn service_words(service: &str) -> &'static [&'static str] {
    match service {
        "TWR" => &["TWR", "TOWER", "TWR1", "TWR2"],
        "GND" => &["GND", "GROUND"],
        "APP" => &["APP", "APCH", "APPROACH", "ARR", "ARRIVAL"],
        "DEP" => &["DEP", "DEPARTURE"],
        "CLD" => &["CLD", "CD", "DEL", "DELIVERY", "CLEARANCE", "CLNC"],
        "CTR" => &["CTR", "CENTER", "CENTRE", "ARTCC", "CONTROL"],
        "ATIS" => &["ATIS"],
        "CTAF" => &["CTAF"],
        "UNICOM" => &["UNICOM"],
        "RAMP" => &["RAMP", "APRON"],
        "AFIS" => &["AFIS", "INFO", "INFORMATION"],
        "FSS" => &["FSS", "RADIO"],
        _ => &[],
    }
}

fn hint_score(c: &Candidate, words: &[String]) -> f64 {
    let mut score = 0.0;
    if let Some(ident) = &c.facility_ident {
        let ident = ident.to_ascii_uppercase();
        // US/Canada/Pacific airports are often written without the K/C/P.
        let short = (ident.len() == 4 && ident.starts_with(['K', 'C', 'P'])).then(|| ident[1..].to_string());
        if words.iter().any(|w| *w == ident || Some(w) == short.as_ref()) {
            score += 2.0;
        }
    }
    let svc = c.service.to_ascii_uppercase();
    if words.iter().any(|w| *w == svc || service_words(&svc).contains(&w.as_str())) {
        score += 1.0;
    }
    let mut named = Vec::new();
    for n in [&c.name, &c.callsign, &c.facility_name].into_iter().flatten() {
        named.extend(self::words(n).into_iter().filter(|w| w.len() >= 3));
    }
    score += 0.25 * words.iter().filter(|w| w.len() >= 3 && named.contains(w)).count() as f64;
    score
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MatchResponse {
    #[serde(default)]
    candidates: Vec<MatchCandidate>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MatchCandidate {
    frequency: MatchFrequency,
    #[serde(default)]
    distance_km: f64,
    #[serde(default)]
    confidence: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MatchFrequency {
    id: String,
    #[serde(default)]
    frequency_hz: f64,
    #[serde(default)]
    service: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    callsign: Option<String>,
    #[serde(default)]
    streamable: Option<bool>,
    #[serde(default)]
    stream_policy_reason: Option<String>,
    #[serde(default)]
    facility: Option<MatchFacility>,
}

#[derive(Deserialize)]
struct MatchFacility {
    #[serde(default)]
    ident: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// Parse a `/v1/atc/frequencies/match` response body.
pub fn parse_match(body: &str) -> anyhow::Result<Vec<Candidate>> {
    let r: MatchResponse = serde_json::from_str(body)?;
    Ok(r.candidates
        .into_iter()
        .map(|c| {
            let f = c.frequency;
            Candidate {
                id: f.id,
                freq_hz: f.frequency_hz.round() as u64,
                service: f.service.unwrap_or_default(),
                name: f.name,
                callsign: f.callsign,
                facility_ident: f.facility.as_ref().and_then(|x| x.ident.clone()),
                facility_name: f.facility.and_then(|x| x.name),
                streamable: f.streamable.unwrap_or(true),
                policy_reason: f.stream_policy_reason,
                distance_km: c.distance_km,
                confidence: c.confidence,
            }
        })
        .collect())
}

/// Ask api-server for the catalog entries on `hz` near (lat, lon), best
/// first. Blocking.
pub fn fetch(api: &str, hz: u64, lat: f64, lon: f64) -> anyhow::Result<Vec<Candidate>> {
    let url = format!("{}/v1/atc/frequencies/match", api.trim_end_matches('/'));
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(15))
        .query("hz", &hz.to_string())
        .query("lat", &format!("{lat:.4}"))
        .query("lon", &format!("{lon:.4}"))
        .query("limit", "10")
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
    parse_match(&body)
}

/// Catalog matches kept across reconnects, by carrier.
#[derive(Default)]
pub struct Matches {
    pub by_freq: HashMap<u64, Resolution>,
}

impl Matches {
    /// Look up every channel that has no configured key and no answer yet.
    /// A failed lookup is retried on the next call; the channel is then fed
    /// with its carrier only and the ingest server matches it.
    pub fn fill(&mut self, api: &str, lat: f64, lon: f64, chans: &[(u64, Option<String>)]) {
        for (hz, hint) in chans {
            if self.by_freq.contains_key(hz) {
                continue;
            }
            let mhz = *hz as f64 / 1e6;
            match fetch(api, *hz, lat, lon) {
                Ok(cands) => {
                    let r = pick(&cands, hint.as_deref());
                    match &r {
                        Resolution::Matched(c) => {
                            tracing::info!("atc catalog: {mhz:.3} MHz is {} ({:.0} km)", c.id, c.distance_km)
                        }
                        Resolution::NotAllowed(c) => tracing::warn!(
                            "atc catalog: {mhz:.3} MHz is {}, which may not be streamed ({}); not feeding it",
                            c.id,
                            c.policy_reason.as_deref().unwrap_or("stream policy")
                        ),
                        Resolution::Ambiguous(cs) => tracing::warn!(
                            "atc catalog: {mhz:.3} MHz could be {}; label the channel with the airport and service \
                             (\"KSFO Tower\") or set atc-frequency",
                            cs.iter()
                                .map(|c| format!("{} ({:.0} km)", c.id, c.distance_km))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        Resolution::NoMatch => {
                            tracing::info!("atc catalog: no ATC frequency at {mhz:.3} MHz near the station")
                        }
                    }
                    self.by_freq.insert(*hz, r);
                }
                Err(e) => tracing::warn!("atc catalog: looking up {mhz:.3} MHz failed: {e:#}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, svc: &str, ident: &str, conf: f64) -> Candidate {
        Candidate {
            id: id.into(),
            freq_hz: 120_500_000,
            service: svc.into(),
            facility_ident: Some(ident.into()),
            streamable: true,
            confidence: conf,
            ..Default::default()
        }
    }

    #[test]
    fn clear_matches_are_taken() {
        let one = [cand("airport:KSFO:TWR:120500", "TWR", "KSFO", 0.3)];
        assert!(matches!(pick(&one, None), Resolution::Matched(c) if c.id == "airport:KSFO:TWR:120500"));
        let ahead = [cand("a:1", "TWR", "KSFO", 0.7), cand("a:2", "TWR", "KOAK", 0.3)];
        assert!(matches!(pick(&ahead, None), Resolution::Matched(c) if c.id == "a:1"));
        assert_eq!(pick(&[], None), Resolution::NoMatch);
    }

    #[test]
    fn labels_break_ties() {
        let tie =
            [cand("airport:KOAK:GND:121900", "GND", "KOAK", 0.45), cand("airport:KSJC:TWR:121900", "TWR", "KSJC", 0.4)];
        assert!(matches!(pick(&tie, None), Resolution::Ambiguous(v) if v.len() == 2));
        assert!(matches!(pick(&tie, Some("SJC Tower")), Resolution::Matched(c) if c.id.contains("KSJC")));
        assert!(matches!(pick(&tie, Some("Oakland ground")), Resolution::Matched(c) if c.id.contains("KOAK")));
        // A label that fits both equally settles nothing.
        assert!(matches!(pick(&tie, Some("Bay Area")), Resolution::Ambiguous(_)));
    }

    #[test]
    fn disallowed_frequencies_are_reported() {
        let mut c = cand("airport:EGLL:TWR:118500", "TWR", "EGLL", 0.9);
        c.streamable = false;
        assert!(matches!(pick(&[c], None), Resolution::NotAllowed(_)));
    }

    #[test]
    fn parses_the_api_response() {
        let body = r#"{"hz":120500000,"candidates":[{"frequency":{"id":"airport:KSFO:TWR:120500",
            "facilityId":"airport:KSFO","frequencyHz":120500000,"service":"TWR","name":"SFO TWR","callsign":null,
            "streamable":true,"streamPolicyReason":null,
            "facility":{"id":"airport:KSFO","type":"airport","ident":"KSFO","name":"San Francisco Intl","country":"US"}},
            "distanceKm":2.1,"distanceNm":1.13,"score":0.9,"confidence":0.97}]}"#;
        let c = parse_match(body).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].id, "airport:KSFO:TWR:120500");
        assert_eq!(c[0].freq_hz, 120_500_000);
        assert_eq!(c[0].facility_ident.as_deref(), Some("KSFO"));
        assert!(c[0].streamable);
        assert_eq!(c[0].label(), "KSFO TWR");
        let named = Candidate { name: Some("NORCAL APP".into()), service: "APP".into(), ..c[0].clone() };
        assert_eq!(named.label(), "KSFO APP (NORCAL APP)");
        assert!(matches!(pick(&c, None), Resolution::Matched(_)));
    }
}
