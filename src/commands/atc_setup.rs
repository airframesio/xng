//! `xng atc setup`: plan an ATC feed for this station and write it as a
//! station file. Works out where the station is (`--lat`/`--lon`, else a
//! rough IP lookup), finds the SDRs to use, picks the busiest nearby ATC
//! channels from the Airframes catalog and packs them onto the SDRs (each
//! takes the best slice of the band the ones before it left), then writes a
//! station TOML that `xng station` runs.

use crate::airband::atc_auth::{self, DEFAULT_API};
use crate::airband::atc_nearby::{self, Plan};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What `xng atc setup` was asked for.
#[derive(clap::Args, Clone, Debug)]
pub struct SetupArgs {
    /// Station latitude (default: a rough IP lookup)
    #[arg(long, allow_hyphen_values = true)]
    pub lat: Option<f64>,
    /// Station longitude (default: a rough IP lookup)
    #[arg(long, allow_hyphen_values = true)]
    pub lon: Option<f64>,
    /// SDR to use (repeat for several), as `xng devices` lists it, e.g.
    /// driver=rtlsdr,serial=00000001. Default: every SDR found
    #[arg(long = "sdr")]
    pub sdrs: Vec<String>,
    /// Use at most this many of the SDRs found
    #[arg(long)]
    pub max_sdrs: Option<usize>,
    /// SDR sample rate (Hz). 2400000 covers about 1.75 MHz per SDR;
    /// 2048000 covers about 1.5 MHz but is steadier on busy USB hubs
    #[arg(short = 'r', long, default_value_t = 2_400_000.0)]
    pub sample_rate: f64,
    /// Search radius (nautical miles)
    #[arg(long, default_value_t = 30)]
    pub radius_nm: u32,
    /// Channels per SDR, at most
    #[arg(long, default_value_t = 8)]
    pub max_per_sdr: usize,
    /// Only these services, e.g. TWR,GND,APP
    #[arg(long, value_delimiter = ',')]
    pub services: Vec<String>,
    /// Name listeners see for this station
    #[arg(long)]
    pub station_name: Option<String>,
    /// Station id (default ATC-<hostname>)
    #[arg(long)]
    pub station_id: Option<String>,
    /// Tuner gain in dB (default: automatic)
    #[arg(long)]
    pub gain: Option<f64>,
    /// Published position is rounded to this many meters
    #[arg(long, default_value_t = 100)]
    pub location_precision_m: u32,
    /// Don't show your account name next to these feeds
    #[arg(long)]
    pub hide_username: bool,
    /// Local dashboard address (airband page at /airband)
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub http: String,
    /// Where to write the station file (default ~/.config/xng/atc-station.toml)
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,
    /// Replace an existing station file
    #[arg(long)]
    pub force: bool,
    /// Print the plan and the station file, write nothing
    #[arg(long)]
    pub dry_run: bool,
    /// Airframes API base URL (default: the sign-in's, else https://api.airframes.io)
    #[arg(long)]
    pub api: Option<String>,
    /// Credentials file from `xng atc login` (default ~/.config/xng/atc.json)
    #[arg(long)]
    pub credentials: Option<PathBuf>,
}

pub fn run(a: SetupArgs) -> anyhow::Result<()> {
    let creds = a.credentials.clone().unwrap_or_else(atc_auth::default_path);
    let signed_in = atc_auth::load(&creds)
        .map(|c| c.refresh_token.is_some())
        .unwrap_or(false);
    let api = a.api.clone().unwrap_or_else(|| {
        atc_auth::load(&creds)
            .ok()
            .and_then(|c| c.api)
            .unwrap_or_else(|| DEFAULT_API.to_string())
    });

    let (lat, lon) = match (a.lat, a.lon) {
        (Some(lat), Some(lon)) => {
            println!("position:  {lat:.4},{lon:.4}");
            (lat, lon)
        }
        (None, None) => {
            let (lat, lon, src) = locate_by_ip()?;
            println!("position:  {lat:.4},{lon:.4} (rough, from an IP lookup at {src})");
            println!("           pass --lat and --lon for your antenna's real position");
            (lat, lon)
        }
        _ => anyhow::bail!("give both --lat and --lon, or neither"),
    };
    anyhow::ensure!(
        (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon),
        "invalid position {lat},{lon}"
    );

    let mut sdrs = if a.sdrs.is_empty() {
        find_sdrs()?
    } else {
        a.sdrs.clone()
    };
    if let Some(n) = a.max_sdrs {
        sdrs.truncate(n.max(1));
    }
    anyhow::ensure!(
        !sdrs.is_empty(),
        "no SDRs found; plug one in and check `xng devices`, or pass --sdr"
    );
    check_serials(&sdrs)?;
    println!("SDRs:      {}", sdrs.join("  "));

    let rows = atc_nearby::fetch(&api, lat, lon, a.radius_nm)?;
    let services: Vec<String> = a.services.iter().map(|s| s.trim().to_ascii_uppercase()).collect();
    let services = (!services.is_empty()).then_some(services.as_slice());
    let plans = atc_nearby::plan_many(&rows, a.sample_rate, a.max_per_sdr.max(1), services, sdrs.len());
    anyhow::ensure!(
        !plans.is_empty(),
        "no ATC channels to feed within {} nm of {lat:.3},{lon:.3} ({} catalog entries); try a larger --radius-nm",
        a.radius_nm,
        rows.len()
    );
    println!(
        "found:     {} catalog entries within {} nm, {:.2} MHz per SDR at {:.3} MS/s",
        rows.len(),
        a.radius_nm,
        atc_nearby::window_hz(a.sample_rate) as f64 / 1e6,
        a.sample_rate / 1e6
    );
    for (sdr, p) in sdrs.iter().zip(&plans) {
        println!();
        println!(
            "{sdr}: center {:.3} MHz, {} channels",
            p.center_hz as f64 / 1e6,
            p.channels.len()
        );
        for c in &p.channels {
            println!("  {}", atc_nearby::describe(c));
        }
    }
    if plans.len() < sdrs.len() {
        println!();
        println!(
            "{} of the {} SDRs have nothing left to feed; they are not used",
            sdrs.len() - plans.len(),
            sdrs.len()
        );
    }
    if plans.len() > 1 && a.sample_rate > 2_048_000.0 {
        println!();
        println!(
            "Several RTL-SDRs at {:.1} MS/s share the USB bus. Plug them in directly or into",
            a.sample_rate / 1e6
        );
        println!("a powered hub. If the log shows overflows or USB timeouts, run setup again with -r 2048000.");
    }

    let station_id = a.station_id.clone().unwrap_or_else(default_station_id);
    let station_name = a.station_name.clone().unwrap_or_else(|| station_id.clone());
    let toml = station_toml(&StationFile {
        station_id: &station_id,
        station_name: &station_name,
        http: &a.http,
        precision_m: a.location_precision_m,
        hide_username: a.hide_username,
        sample_rate: a.sample_rate,
        gain: a.gain,
        lat,
        lon,
        sessions: sdrs.iter().map(String::as_str).zip(plans.iter()).collect(),
    });

    let path = a
        .output
        .clone()
        .unwrap_or_else(|| atc_auth::config_dir().join("atc-station.toml"));
    println!();
    if a.dry_run {
        println!("--- {} (not written) ---", path.display());
        print!("{toml}");
        return Ok(());
    }
    write_new(&path, &toml, a.force)?;
    println!("Wrote {}.", path.display());
    println!();
    if !signed_in {
        println!("Sign in once:   xng atc login");
    }
    println!("Start feeding:  xng station {}", shell_path(&path));
    println!("Run setup again to pick again; the catalog gains channels over time.");
    Ok(())
}

/// The SDRs `xng devices` lists, as short selectors (driver and serial).
fn find_sdrs() -> anyhow::Result<Vec<String>> {
    let found = crate::commands::devices::list("")?;
    Ok(found.iter().map(|d| short_selector(&d.selector)).collect())
}

/// `driver=rtlsdr,label=...,serial=00000001,backend=soapy` ->
/// `driver=rtlsdr,serial=00000001`. Selectors without a serial are kept.
fn short_selector(sel: &str) -> String {
    let kv: Vec<(&str, &str)> = sel
        .split(',')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();
    let get = |k: &str| kv.iter().find(|(kk, _)| *kk == k).map(|(_, v)| *v);
    match (get("driver"), get("serial")) {
        (Some(d), Some(s)) if !s.is_empty() => format!("driver={d},serial={s}"),
        _ => sel.to_string(),
    }
}

/// Two RTL-SDRs with the same serial can't be told apart: xng would open
/// the same one twice. Most ship as 00000001.
fn check_serials(sdrs: &[String]) -> anyhow::Result<()> {
    let mut seen = std::collections::HashSet::new();
    for s in sdrs {
        anyhow::ensure!(
            seen.insert(s.to_ascii_lowercase()),
            "two SDRs share the selector {s}. Give each RTL-SDR its own serial (with only that one \
             plugged in: rtl_eeprom -s 00000002), then run setup again"
        );
    }
    Ok(())
}

fn default_station_id() -> String {
    let host = std::fs::read_to_string("/etc/hostname")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        })
        .unwrap_or_default();
    let host: String = host
        .trim()
        .split('.')
        .next()
        .unwrap_or("")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '-'
            }
        })
        .take(20)
        .collect();
    if host.is_empty() {
        "ATC-XNG".into()
    } else {
        format!("ATC-{host}")
    }
}

/// A rough position from the public IP address. City level at best.
fn locate_by_ip() -> anyhow::Result<(f64, f64, &'static str)> {
    let get = |url: &str| -> anyhow::Result<serde_json::Value> {
        Ok(ureq::get(url)
            .timeout(Duration::from_secs(8))
            .set("Accept", "application/json")
            .set("User-Agent", concat!("xng/", env!("CARGO_PKG_VERSION")))
            .call()?
            .into_string()
            .map(|b| serde_json::from_str(&b))??)
    };
    if let Ok(j) = get("https://ipinfo.io/json") {
        if let Some((lat, lon)) = j["loc"].as_str().and_then(parse_lat_lon) {
            return Ok((lat, lon, "ipinfo.io"));
        }
    }
    if let Ok(j) = get("https://ipapi.co/json/") {
        if let (Some(lat), Some(lon)) = (j["latitude"].as_f64(), j["longitude"].as_f64()) {
            return Ok((lat, lon, "ipapi.co"));
        }
    }
    anyhow::bail!("could not work out where this station is; pass --lat and --lon")
}

fn parse_lat_lon(s: &str) -> Option<(f64, f64)> {
    let (a, b) = s.split_once(',')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

struct StationFile<'a> {
    station_id: &'a str,
    station_name: &'a str,
    http: &'a str,
    precision_m: u32,
    hide_username: bool,
    sample_rate: f64,
    gain: Option<f64>,
    lat: f64,
    lon: f64,
    sessions: Vec<(&'a str, &'a Plan)>,
}

fn q(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

fn mhz(hz: u64) -> String {
    let s = format!("{:.6}", hz as f64 / 1e6);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn station_toml(f: &StationFile) -> String {
    let mut o = String::new();
    o.push_str("# Written by `xng atc setup`. Run it again to pick channels again.\n");
    o.push_str("# Start with: xng station <this file>\n");
    o.push_str(&format!("station-id = {}\n\n", q(f.station_id)));
    o.push_str("[outputs]\n");
    o.push_str(&format!(
        "http = {}            # dashboard; airband page at /airband\n\n",
        q(f.http)
    ));
    o.push_str("[outputs.airband.atc]\n");
    o.push_str(&format!("station-name = {}\n", q(f.station_name)));
    o.push_str(&format!("location-precision-m = {}\n", f.precision_m));
    o.push_str(&format!("hide-username = {}\n", f.hide_username));
    o.push_str("receiver = { type = \"rtlsdr\" }\n");
    for (sdr, plan) in &f.sessions {
        o.push_str("\n[[session]]\n");
        o.push_str(&format!("sdr = {}\n", q(sdr)));
        o.push_str("mode = \"airband\"\n");
        o.push_str(&format!("sample-rate = {}\n", f.sample_rate as u64));
        o.push_str(&format!("center = {}\n", q(&mhz(plan.center_hz))));
        if let Some(g) = f.gain {
            o.push_str(&format!("gain = {g}\n"));
        }
        o.push_str(&format!("receiver-pos = \"{:.4},{:.4}\"\n", f.lat, f.lon));
        o.push_str("\n[session.airband.defaults]\nsquelch = \"auto:9\"\nhang-ms = 300\nafc-max-hz = 1500\n");
        for c in &plan.channels {
            o.push_str("\n[[session.airband.channel]]\n");
            o.push_str(&format!("freq = {}\n", q(&mhz(c.freq_hz))));
            o.push_str(&format!("label = {}\n", q(&c.label)));
            o.push_str(&format!("atc-frequency = {}\n", q(&c.id)));
        }
    }
    o
}

/// Write `body` to `path`, refusing to replace a file unless `force`.
fn write_new(path: &Path, body: &str, force: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        force || !path.exists(),
        "{} already exists; pass --force to replace it, or -o for another file",
        path.display()
    );
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, body)?;
    Ok(())
}

fn shell_path(p: &Path) -> String {
    let s = p.display().to_string();
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-~".contains(c)) {
        s
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::airband::atc_nearby::Pick;

    #[test]
    fn selectors_keep_driver_and_serial() {
        assert_eq!(
            short_selector("driver=rtlsdr, label=Generic RTL2832U :: 00000002, serial=00000002,backend=soapy"),
            "driver=rtlsdr,serial=00000002"
        );
        assert_eq!(short_selector("driver=airspy,serial=A0B1"), "driver=airspy,serial=A0B1");
        assert_eq!(short_selector("backend=soapy"), "backend=soapy");
    }

    #[test]
    fn duplicate_serials_are_refused() {
        let two = vec![
            "driver=rtlsdr,serial=00000001".to_string(),
            "driver=rtlsdr,serial=00000001".to_string(),
        ];
        assert!(check_serials(&two).is_err());
        assert!(check_serials(&two[..1]).is_ok());
    }

    #[test]
    fn writes_a_station_file_xng_station_reads() {
        let p1 = Plan {
            center_hz: 120_300_000,
            channels: vec![
                Pick::for_test(119_500_000, "airport:KSAC:TWR:119500", "KSAC TWR"),
                Pick::for_test(120_900_000, "tracon:NCT:APP:120900", "NCT APP \"Sacramento\""),
            ],
        };
        let p2 = Plan {
            center_hz: 125_850_000,
            channels: vec![Pick::for_test(125_700_000, "airport:KSMF:TWR:125700", "KSMF TWR")],
        };
        let body = station_toml(&StationFile {
            station_id: "ATC-PI",
            station_name: "Cameron Park",
            http: "127.0.0.1:8080",
            precision_m: 100,
            hide_username: false,
            sample_rate: 2_048_000.0,
            gain: None,
            lat: 38.68,
            lon: -120.98,
            sessions: vec![
                ("driver=rtlsdr,serial=00000001", &p1),
                ("driver=rtlsdr,serial=00000002", &p2),
            ],
        });
        let v: toml::Value = toml::from_str(&body).expect("valid TOML");
        let sessions = v["session"].as_array().unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0]["center"].as_str(), Some("120.3"));
        assert_eq!(sessions[0]["sample-rate"].as_integer(), Some(2_048_000));
        let ch = sessions[0]["airband"]["channel"].as_array().unwrap();
        assert_eq!(ch[1]["label"].as_str(), Some("NCT APP \"Sacramento\""));
        assert_eq!(ch[0]["atc-frequency"].as_str(), Some("airport:KSAC:TWR:119500"));
        assert_eq!(
            v["outputs"]["airband"]["atc"]["station-name"].as_str(),
            Some("Cameron Park")
        );
        // And `xng station` accepts it.
        let dir = std::env::temp_dir().join(format!("xng-atc-setup-load-{}", std::process::id()));
        let path = dir.join("atc-station.toml");
        write_new(&path, &body, true).unwrap();
        let st = crate::commands::station::load(&path).expect("xng station reads it");
        assert_eq!(st.sessions.len(), 2);
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(mhz(119_500_000), "119.5");
        assert_eq!(mhz(118_025_000), "118.025");
    }

    #[test]
    fn written_files_are_not_replaced_without_force() {
        let dir = std::env::temp_dir().join(format!("xng-atc-setup-{}", std::process::id()));
        let path = dir.join("atc-station.toml");
        write_new(&path, "a", false).unwrap();
        assert!(write_new(&path, "b", false).is_err());
        write_new(&path, "c", true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "c");
        let _ = std::fs::remove_dir_all(dir);
    }
}
