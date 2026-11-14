//! xng desktop: a Tauri shell around the xng decode engine.
//!
//! All decoding happens in-process through `xng::engine` — the same code
//! path, decoders and outputs as the `xng` CLI. This crate only translates
//! between the UI and that engine.

mod request;
mod session;

use request::{ResolvedTune, SessionRequest};
use serde::Serialize;
use session::{Event, Running};
use std::sync::Mutex;
use tauri::ipc::Channel;
use tauri::State;

#[derive(Default)]
struct AppState {
    running: Mutex<Option<Running>>,
}

/// SoapySDR loads its driver modules on first use and that is not safe to
/// do from two threads at once, so everything that enumerates, probes or
/// opens a device goes through this lock.
static SDR: Mutex<()> = Mutex::new(());

fn sdr_lock() -> std::sync::MutexGuard<'static, ()> {
    SDR.lock().unwrap_or_else(|e| e.into_inner())
}

/// Errors cross the IPC boundary as plain strings (shown in the UI).
type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ModeInfo {
    id: &'static str,
    plan_rate: f64,
    plan_channels: Vec<u64>,
    channel_rate: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: &'static str,
    backends: Vec<&'static str>,
    cores: usize,
    modes: Vec<ModeInfo>,
    os: &'static str,
}

/// Static facts the UI needs at launch: version, compiled-in SDR backends,
/// and every mode's built-in frequency plan.
#[tauri::command]
fn app_info() -> AppInfo {
    let mut backends = Vec::new();
    if cfg!(feature = "soapy") {
        backends.push("soapysdr");
    }
    if cfg!(feature = "airspy") {
        backends.push("airspy");
    }
    if cfg!(feature = "airspyhf") {
        backends.push("airspyhf");
    }
    let modes = xng_types::Mode::native_modes()
        .iter()
        .map(|&m| {
            let (plan_rate, plan_channels) = xng::commands::scan::plan(m);
            ModeInfo {
                id: m.as_str(),
                plan_rate,
                plan_channels,
                channel_rate: xng::commands::scan::channel_rate(m),
            }
        })
        .collect();
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        backends,
        cores: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        modes,
        os: std::env::consts::OS,
    }
}

/// Every SDR the compiled-in backends can see.
#[tauri::command]
async fn list_devices(filter: Option<String>) -> CmdResult<Vec<xng::commands::devices::DeviceEntry>> {
    tauri::async_runtime::spawn_blocking(move || {
        let _sdr = sdr_lock();
        xng::commands::devices::list(filter.as_deref().unwrap_or("")).map_err(err)
    })
    .await
    .map_err(err)?
}

/// What a request would tune to (auto-filling blanks from the mode plan),
/// without starting anything.
#[tauri::command]
async fn resolve_tune(request: SessionRequest) -> CmdResult<ResolvedTune> {
    tauri::async_runtime::spawn_blocking(move || {
        let _sdr = sdr_lock();
        let (mode, sample_rate, center_hz, channels_hz) = request::resolve(&request).map_err(err)?;
        Ok(ResolvedTune { mode: mode.as_str().to_owned(), sample_rate, center_hz, channels_hz })
    })
    .await
    .map_err(err)?
}

/// Start decoding. Any running session is stopped first.
#[tauri::command]
async fn start_session(
    state: State<'_, AppState>,
    request: SessionRequest,
    on_event: Channel<Event>,
) -> CmdResult<()> {
    let prev = state.running.lock().unwrap().take();
    if let Some(prev) = prev {
        tauri::async_runtime::spawn_blocking(move || prev.stop()).await.map_err(err)?;
    }
    let running = tauri::async_runtime::spawn_blocking(move || {
        let _sdr = sdr_lock();
        Running::start(&request, on_event)
    })
        .await
        .map_err(err)?
        .map_err(err)?;
    *state.running.lock().unwrap() = Some(running);
    Ok(())
}

/// Stop decoding and close the source.
#[tauri::command]
async fn stop_session(state: State<'_, AppState>) -> CmdResult<()> {
    let prev = state.running.lock().unwrap().take();
    if let Some(prev) = prev {
        tauri::async_runtime::spawn_blocking(move || prev.stop()).await.map_err(err)?;
    }
    Ok(())
}

/// Write the current map picture to `path` as GeoJSON, GPX or KML.
#[tauri::command]
async fn export_entities(state: State<'_, AppState>, format: String, path: String) -> CmdResult<()> {
    let body = {
        let guard = state.running.lock().unwrap();
        let running = guard.as_ref().ok_or("Nothing is running to export")?;
        running.with_dashboard(|d| match format.as_str() {
            "geojson" => Ok(d.export_geojson()),
            "gpx" => Ok(d.export_gpx()),
            "kml" => Ok(d.export_kml()),
            "aircraft" => Ok(d.aircraft_json()),
            other => Err(format!("Unknown export format {other}")),
        })?
    };
    std::fs::write(&path, body).map_err(err)
}

/// Size the decode worker pool (as `--decode-threads`). Only the first
/// call before any decoding takes effect; returns whether it did.
#[tauri::command]
fn set_decode_threads(threads: usize) -> bool {
    xng::runtime::set_decode_threads(threads)
}

pub fn run() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
    tracing::info!("xng desktop v{}", env!("CARGO_PKG_VERSION"));

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            app_info,
            list_devices,
            resolve_tune,
            start_session,
            stop_session,
            export_entities,
            set_decode_threads,
        ])
        .on_window_event(|window, event| {
            // Close the SDR cleanly when the window goes away.
            if let tauri::WindowEvent::Destroyed = event {
                use tauri::Manager;
                if let Some(state) = window.try_state::<AppState>() {
                    if let Some(r) = state.running.lock().unwrap().take() {
                        r.stop();
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running xng desktop");
}
