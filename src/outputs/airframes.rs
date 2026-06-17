//! Per-mode Airframes feeding.
//!
//! Routes each decoded message to the Airframes ingest for *its* mode, in
//! that ingest's native wire format, under a per-mode / per-session station
//! id. This is the legacy per-port push path; asf-2.0 (the multiplexed
//! gRPC/QUIC feed) is a separate, independent path under the canonical
//! station ident and does NOT go through this router.
//!
//! Verified public ingests (2026-06):
//! - ACARS  UDP  `feed.airframes.io:5550`  (acarsdec flat JSON)
//! - VDL2   UDP  `:5552` / TCP `:5553`      (dumpvdl2 `decoded:json`)
//! - HFDL   UDP  `:5556`                    (dumphfdl `decoded:json`)
//! - AIS    HTTP `:5599`                    (AIS-Catcher `PROTOCOL AIRFRAMES`)
//!
//! IMSL / IRDM / STD-C / Aero-C / ADS-B have no public per-port ingest —
//! those modes reach Airframes via asf-2.0 only.
//!
//! Only the ACARS serializer is implemented so far (the live path); VDL2,
//! HFDL and AIS serializers slot into the same router as they land
//! (`FEED-2.1`/`2.2`/`2.3`), gated on each mode's decode being complete
//! enough to fill its native format.

use std::collections::HashSet;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use xng_types::{Message, Mode};

/// Public Airframes ingest host.
pub const AIRFRAMES_HOST: &str = "feed.airframes.io";

/// Transport for an Airframes ingest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proto {
    Udp,
    /// Reserved for the VDL2 TCP ingest (:5553); not emitted until FEED-2.1.
    #[allow(dead_code)]
    Tcp,
    Http,
}

/// A resolved Airframes ingest endpoint plus the station id to stamp.
#[derive(Clone, Debug)]
pub struct AirframesTarget {
    pub host: String,
    pub port: u16,
    pub proto: Proto,
    pub station_id: String,
}

impl AirframesTarget {
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// One routing rule: messages of `mode` from `sdr_id` (None = any SDR) feed
/// to `target`.
#[derive(Clone, Debug)]
pub struct Route {
    pub sdr_id: Option<String>,
    pub mode: Mode,
    pub target: AirframesTarget,
}

/// The per-session Airframes feed routing table for a process.
#[derive(Clone, Debug, Default)]
pub struct AirframesRouter {
    pub routes: Vec<Route>,
}

impl AirframesRouter {
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    /// Resolve the feed target for a message: prefer an exact `(sdr_id, mode)`
    /// match (per-session routes from a station config), then a wildcard
    /// (`sdr_id = None`) match on mode (single-session CLI routes).
    pub fn resolve(&self, mode: Mode, sdr_id: Option<&str>) -> Option<&AirframesTarget> {
        if let Some(id) = sdr_id {
            if let Some(r) = self
                .routes
                .iter()
                .find(|r| r.mode == mode && r.sdr_id.as_deref() == Some(id))
            {
                return Some(&r.target);
            }
        }
        self.routes
            .iter()
            .find(|r| r.mode == mode && r.sdr_id.is_none())
            .map(|r| &r.target)
    }
}

/// The default public Airframes ingest for a mode, or `None` if that mode has
/// no per-port ingest (feed it via asf-2.0 instead).
pub fn default_endpoint(mode: Mode) -> Option<(&'static str, u16, Proto)> {
    match mode {
        Mode::AcarsPoa => Some((AIRFRAMES_HOST, 5550, Proto::Udp)),
        Mode::Vdl2 => Some((AIRFRAMES_HOST, 5552, Proto::Udp)),
        Mode::Hfdl => Some((AIRFRAMES_HOST, 5556, Proto::Udp)),
        Mode::Ais => Some((AIRFRAMES_HOST, 5599, Proto::Http)),
        _ => None,
    }
}

/// Whether xng can currently serialize this mode into its Airframes native
/// format. Modes without a serializer route to asf-2.0 only (for now).
pub fn has_serializer(mode: Mode) -> bool {
    matches!(mode, Mode::AcarsPoa)
}

/// The station-id suffix Airframes expects per mode (for `auto-suffix`).
pub fn mode_suffix(mode: Mode) -> Option<&'static str> {
    match mode {
        Mode::AcarsPoa => Some("ACARS"),
        Mode::Vdl2 => Some("VDL2"),
        Mode::Hfdl => Some("HFDL"),
        Mode::Ais => Some("AIS"),
        Mode::AeroL | Mode::AeroC => Some("IMSL"),
        Mode::Iridium => Some("IRDM"),
        Mode::StdC => Some("STDC"),
        Mode::Adsb => Some("ADSB"),
        Mode::Uat => Some("UAT"),
        Mode::Sarsat => Some("SARSAT"),
        Mode::Dsc => Some("DSC"),
        Mode::Navtex => Some("NAVTEX"),
        Mode::Sonde => Some("SONDE"),
        Mode::AdsL => Some("ADSL"),
        Mode::Atcs => Some("ATCS"),
        Mode::Extern => None,
    }
}

const KNOWN_SUFFIXES: &[&str] = &[
    "ACARS", "VDL2", "HFDL", "AIS", "IMSL", "IRDM", "STDC", "ADSB", "UAT", "SARSAT", "DSC",
    "NAVTEX", "SONDE", "ADSL", "ATCS",
];

/// Derive a per-mode station id from a base by stripping a trailing known
/// mode suffix (if any) and appending this mode's suffix:
/// `KE-KSEA` + acars → `KE-KSEA-ACARS`; `KE-KSEA-ACARS` + vdl2 → `KE-KSEA-VDL2`.
/// Bases that don't end in a known suffix are left intact and appended to.
pub fn auto_suffix(base: &str, mode: Mode) -> String {
    let Some(suffix) = mode_suffix(mode) else {
        return base.to_string();
    };
    let stem = strip_known_suffix(base);
    format!("{stem}-{suffix}")
}

fn strip_known_suffix(s: &str) -> &str {
    if let Some((stem, last)) = s.rsplit_once('-') {
        // Tolerate a trailing instance number, e.g. `-ACARS1`.
        let alpha = last.trim_end_matches(|c: char| c.is_ascii_digit());
        if KNOWN_SUFFIXES.contains(&alpha) {
            return stem;
        }
    }
    s
}

/// Build a router for a single CLI session feeding `--feed-airframes`: the
/// station id is used verbatim (no auto-suffix), with one wildcard route per
/// mode that has a serializer.
pub fn cli_router(feed: bool, station_id: &str, modes: &[Mode]) -> AirframesRouter {
    if !feed {
        return AirframesRouter::default();
    }
    let routes = modes
        .iter()
        .filter_map(|&mode| {
            if !has_serializer(mode) {
                return None;
            }
            let (host, port, proto) = default_endpoint(mode)?;
            Some(Route {
                sdr_id: None,
                mode,
                target: AirframesTarget {
                    host: host.to_string(),
                    port,
                    proto,
                    station_id: station_id.to_string(),
                },
            })
        })
        .collect();
    AirframesRouter { routes }
}

/// Serialize a message into its mode's native Airframes datagram payload,
/// stamping `station_id`. Returns `None` when the body doesn't match the mode
/// or no serializer exists yet.
fn serialize_datagram(msg: &Message, mode: Mode, station_id: &str) -> Option<Vec<u8>> {
    match mode {
        Mode::AcarsPoa => crate::outputs::acarsdec_json::format_acarsdec_with_station(msg, Some(station_id))
            .map(|v| v.to_string().into_bytes()),
        // FEED-2.1 dumpvdl2 decoded:json, FEED-2.2 dumphfdl decoded:json land here.
        _ => None,
    }
}

/// Consume the bus and feed each message to its mode's Airframes ingest.
pub async fn run(mut rx: broadcast::Receiver<Arc<Message>>, router: AirframesRouter) -> std::io::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let mut sent: u64 = 0;
    let mut warned: HashSet<&'static str> = HashSet::new();
    loop {
        match rx.recv().await {
            Ok(msg) => {
                let sdr_id = msg.source.sdr.as_ref().map(|s| s.id.as_str());
                let Some(target) = router.resolve(msg.mode, sdr_id) else {
                    continue;
                };
                match target.proto {
                    Proto::Udp => {
                        if let Some(bytes) = serialize_datagram(&msg, msg.mode, &target.station_id) {
                            match socket.send_to(&bytes, target.endpoint()).await {
                                Ok(_) => sent += 1,
                                Err(e) => {
                                    tracing::warn!("airframes udp send to {} failed: {e}", target.endpoint())
                                }
                            }
                        }
                    }
                    Proto::Tcp | Proto::Http => {
                        // AIS HTTP (:5599) lands in FEED-2.3; until then, skip
                        // with a single notice so it's not silently ignored.
                        if let Some(name) = mode_suffix(msg.mode) {
                            if warned.insert(name) {
                                tracing::warn!(
                                    "airframes {name} feed ({:?} {}) not yet implemented; feed via asf-2.0",
                                    target.proto,
                                    target.endpoint()
                                );
                            }
                        }
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("airframes output lagged, dropped {n} messages")
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    tracing::info!("airframes output: {sent} message(s) sent");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_suffix_appends_and_swaps() {
        assert_eq!(auto_suffix("KE-KSEA", Mode::AcarsPoa), "KE-KSEA-ACARS");
        assert_eq!(auto_suffix("KE-KSEA-ACARS", Mode::Vdl2), "KE-KSEA-VDL2");
        assert_eq!(auto_suffix("KE-KSEA-ACARS1", Mode::Hfdl), "KE-KSEA-HFDL");
        // unknown trailing segment is preserved (operator's base kept intact)
        assert_eq!(auto_suffix("KE-KSEA-TEST", Mode::AcarsPoa), "KE-KSEA-TEST-ACARS");
    }

    #[test]
    fn resolve_prefers_exact_session_then_wildcard() {
        let r = AirframesRouter {
            routes: vec![
                Route {
                    sdr_id: Some("rtl0".into()),
                    mode: Mode::AcarsPoa,
                    target: AirframesTarget {
                        host: "h".into(),
                        port: 5550,
                        proto: Proto::Udp,
                        station_id: "EXACT".into(),
                    },
                },
                Route {
                    sdr_id: None,
                    mode: Mode::AcarsPoa,
                    target: AirframesTarget {
                        host: "h".into(),
                        port: 5550,
                        proto: Proto::Udp,
                        station_id: "WILD".into(),
                    },
                },
            ],
        };
        assert_eq!(r.resolve(Mode::AcarsPoa, Some("rtl0")).unwrap().station_id, "EXACT");
        assert_eq!(r.resolve(Mode::AcarsPoa, Some("other")).unwrap().station_id, "WILD");
        assert_eq!(r.resolve(Mode::AcarsPoa, None).unwrap().station_id, "WILD");
        assert!(r.resolve(Mode::Vdl2, Some("rtl0")).is_none());
    }

    #[test]
    fn cli_router_only_routes_serializable_modes() {
        let r = cli_router(true, "XX-TEST", &[Mode::AcarsPoa, Mode::Vdl2, Mode::Hfdl]);
        // Only ACARS has a serializer today.
        assert_eq!(r.routes.len(), 1);
        assert_eq!(r.routes[0].mode, Mode::AcarsPoa);
        assert_eq!(r.routes[0].target.port, 5550);
        assert!(cli_router(false, "XX-TEST", &[Mode::AcarsPoa]).is_empty());
    }

    #[test]
    fn endpoints_match_verified_ports() {
        assert_eq!(default_endpoint(Mode::AcarsPoa), Some((AIRFRAMES_HOST, 5550, Proto::Udp)));
        assert_eq!(default_endpoint(Mode::Vdl2), Some((AIRFRAMES_HOST, 5552, Proto::Udp)));
        assert_eq!(default_endpoint(Mode::Hfdl), Some((AIRFRAMES_HOST, 5556, Proto::Udp)));
        assert_eq!(default_endpoint(Mode::Ais), Some((AIRFRAMES_HOST, 5599, Proto::Http)));
        // No public per-port ingest → asf-2.0 only.
        assert_eq!(default_endpoint(Mode::Iridium), None);
        assert_eq!(default_endpoint(Mode::StdC), None);
        assert_eq!(default_endpoint(Mode::Adsb), None);
    }
}
