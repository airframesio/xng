//! xng desktop: a Tauri shell around the xng decode engine.
//!
//! All decoding happens in-process through `xng::engine` — the same code
//! path, decoders and outputs as the `xng` CLI. This crate only translates
//! between the UI and that engine.

mod request;
mod session;
mod tools;

use request::{ResolvedTune, SessionRequest};
use serde::Serialize;
use session::{Event, Running};
use tools::{ToolEvent, ToolRequest, ToolTask};
use std::sync::Mutex;
use tauri::ipc::Channel;
use tauri::State;

#[derive(Default)]
struct AppState {
    running: Mutex<Option<Running>>,
    /// The map picture of the current or most recent session. It is kept
    /// after Stop so the final state can still be exported.
    dashboard: Mutex<Option<std::sync::Arc<Mutex<xng::outputs::http::Dashboard>>>>,
    /// A scan or survey in progress. It owns the radio while it runs.
    tool: Mutex<Option<ToolTask>>,
    /// The stop flag of a tool that `start_tool` is still clearing the way
    /// for. It is not in `tool` yet, so a stop asked for now is set here
    /// and the tool starts with it already in force.
    tool_pending: Mutex<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    /// Set while a session is opening its source. It is not in `running`
    /// yet, so a tool started now could not stop it and would end up
    /// sharing the radio with it.
    starting: std::sync::atomic::AtomicBool,
    /// Set once the window is gone, so a session still opening its radio
    /// is stopped as soon as it comes up instead of being kept.
    closing: std::sync::atomic::AtomicBool,
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
    stop_tool_task(&state).await?;
    state.starting.store(true, std::sync::atomic::Ordering::SeqCst);
    let opened = tauri::async_runtime::spawn_blocking(move || {
        let _sdr = sdr_lock();
        Running::start(&request, on_event)
    })
    .await;
    // Publish the session (or the failure) before tools are let in again.
    let running = match opened {
        Ok(Ok(running)) => running,
        failed => {
            state.starting.store(false, std::sync::atomic::Ordering::SeqCst);
            return Err(match failed {
                Ok(Err(e)) => err(e),
                Err(e) => err(e),
                Ok(Ok(_)) => unreachable!(),
            });
        }
    };
    let mut slot = state.running.lock().unwrap();
    state.starting.store(false, std::sync::atomic::Ordering::SeqCst);
    if state.closing.load(std::sync::atomic::Ordering::Relaxed) {
        drop(slot);
        running.stop();
        return Err("The window was closed".into());
    }
    *state.dashboard.lock().unwrap() = Some(running.dashboard());
    *slot = Some(running);
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

/// Stop any scan or survey and wait for it to let go of the radio.
async fn stop_tool_task(state: &State<'_, AppState>) -> CmdResult<()> {
    let prev = state.tool.lock().unwrap().take();
    if let Some(prev) = prev {
        tauri::async_runtime::spawn_blocking(move || prev.stop()).await.map_err(err)?;
    }
    Ok(())
}

/// Run `xng scan` or `xng survey`. Progress and the final report arrive on
/// `on_event`. A running session or tool is stopped first: they would be
/// holding the radio.
#[tauri::command]
async fn start_tool(
    state: State<'_, AppState>,
    request: ToolRequest,
    on_event: Channel<ToolEvent>,
) -> CmdResult<()> {
    if state.starting.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("A session is still starting. Wait for it, then stop it to run this.".into());
    }
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    *state.tool_pending.lock().unwrap() = Some(cancel.clone());
    let started = async {
        let prev = state.running.lock().unwrap().take();
        if let Some(prev) = prev {
            tauri::async_runtime::spawn_blocking(move || prev.stop()).await.map_err(err)?;
        }
        stop_tool_task(&state).await?;
        ToolTask::start(request, on_event, sdr_lock, cancel).map_err(err)
    }
    .await;
    // Publish the task and retire the pending flag under one lock, so a
    // stop always finds one of the two.
    let mut slot = state.tool.lock().unwrap();
    *state.tool_pending.lock().unwrap() = None;
    let task = started?;
    if state.closing.load(std::sync::atomic::Ordering::Relaxed) {
        drop(slot);
        task.stop();
        return Err("The window was closed".into());
    }
    *slot = Some(task);
    Ok(())
}

/// Ask the running scan or survey to finish early. It reports what it has
/// measured so far through its event channel; this returns at once.
#[tauri::command]
fn stop_tool(state: State<'_, AppState>) {
    let mut slot = state.tool.lock().unwrap();
    if let Some(task) = slot.as_ref() {
        task.cancel();
        if task.finished() {
            *slot = None;
        }
    }
    // A tool on its way up is stopped too, whether or not an earlier one
    // is still in the slot.
    if let Some(cancel) = state.tool_pending.lock().unwrap().as_ref() {
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Write the current map picture to `path` as GeoJSON, GPX or KML.
#[tauri::command]
async fn export_entities(state: State<'_, AppState>, format: String, path: String) -> CmdResult<()> {
    let dashboard = state.dashboard.lock().unwrap().clone().ok_or("Start a session before exporting")?;
    let body = {
        let d = dashboard.lock().unwrap();
        match format.as_str() {
            "geojson" => d.export_geojson(),
            "gpx" => d.export_gpx(),
            "kml" => d.export_kml(),
            "aircraft" => d.aircraft_json(),
            other => return Err(format!("Unknown export format {other}")),
        }
    };
    std::fs::write(&path, body).map_err(err)
}

/// Save text the UI produced (a scan or survey report) to a path the user
/// picked in a save dialog.
#[tauri::command]
async fn write_text(path: String, contents: String) -> CmdResult<()> {
    std::fs::write(&path, contents).map_err(err)
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
            start_tool,
            stop_tool,
            export_entities,
            write_text,
            set_decode_threads,
        ])
        .on_window_event(|window, event| {
            // Close the SDR cleanly when the window goes away.
            if let tauri::WindowEvent::Destroyed = event {
                use tauri::Manager;
                if let Some(state) = window.try_state::<AppState>() {
                    state.closing.store(true, std::sync::atomic::Ordering::Relaxed);
                    if let Some(r) = state.running.lock().unwrap().take() {
                        r.stop();
                    }
                    if let Some(t) = state.tool.lock().unwrap().take() {
                        t.stop();
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running xng desktop");
}
