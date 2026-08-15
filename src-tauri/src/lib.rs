//! ProcessObserver – Tauri v2 Library Entry Point
//!
//! This crate exposes all Tauri commands and manages the application lifecycle.

mod app_state;
mod elevation;
mod monitor;
mod session;

use app_state::AppState;
use elevation::check_elevated;
use monitor::metrics::ProcessMetrics;
use monitor::network::NetworkStats;
use session::{ExportFormat, SessionInfo};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tauri::{Emitter, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::Mutex;
use tokio::time::{interval, Duration};

// ─── Tauri Commands ───────────────────────────────────────

/// Returns whether the current process is running with Administrator privileges.
#[tauri::command]
fn is_elevated() -> bool {
    check_elevated()
}

/// Requests elevation by restarting the application as Administrator.
/// The pending monitoring configuration is carried across the restart.
/// Returns true if the restart was initiated.
#[tauri::command]
fn request_elevation(
    executable: String,
    interval_ms: u64,
    enable_network: bool,
    retention_secs: u64,
) -> bool {
    elevation::restart_as_admin(&executable, interval_ms, enable_network, retention_secs)
}

/// Returns monitoring settings carried over from an elevation restart, if any.
#[tauri::command]
fn get_restart_params() -> Option<elevation::RestartParams> {
    elevation::parse_restart_params()
}

/// Returns basic information about all currently known sessions.
#[tauri::command]
async fn get_sessions(state: State<'_, Arc<Mutex<AppState>>>) -> Result<Vec<SessionInfo>, String> {
    let app = state.lock().await;
    Ok(app.get_session_infos())
}

/// Starts a new monitoring session for the given executable name and interval (in ms).
/// Returns the session ID on success.
#[tauri::command]
async fn start_monitoring(
    state: State<'_, Arc<Mutex<AppState>>>,
    app_handle: tauri::AppHandle,
    executable: String,
    interval_ms: u64,
    enable_network: bool,
    retention_secs: u64,
) -> Result<String, String> {
    let session_id = {
        let mut app = state.lock().await;
        let sid = app.create_session(&executable, interval_ms, enable_network, retention_secs);
        sid
    };

    // Clone what we need for the spawned task
    let state_clone = Arc::clone(&state);
    let app_handle_clone = app_handle.clone();
    let exec = executable.clone();
    let sid_for_spawn = session_id.clone();

    tokio::spawn(async move {
        run_monitoring_loop(state_clone, app_handle_clone, sid_for_spawn, exec, interval_ms).await;
    });

    Ok(session_id)
}

/// Stops an active monitoring session by ID. Data remains viewable.
#[tauri::command]
async fn stop_monitoring(
    state: State<'_, Arc<Mutex<AppState>>>,
    session_id: String,
) -> Result<(), String> {
    let mut app = state.lock().await;
    app.stop_session(&session_id);
    Ok(())
}

/// Removes a session entirely (including all stored data).
#[tauri::command]
async fn remove_session(
    state: State<'_, Arc<Mutex<AppState>>>,
    session_id: String,
) -> Result<(), String> {
    let mut app = state.lock().await;
    app.remove_session(&session_id);
    Ok(())
}

/// Exports session data to CSV or JSON. Returns the file content as a string.
#[tauri::command]
async fn export_session_data(
    state: State<'_, Arc<Mutex<AppState>>>,
    session_id: String,
    format: String,
) -> Result<String, String> {
    let app = state.lock().await;
    let export_fmt = match format.to_lowercase().as_str() {
        "csv" => ExportFormat::Csv,
        "json" => ExportFormat::Json,
        _ => return Err(format!("Unsupported export format: {}", format)),
    };
    app.export_session(&session_id, export_fmt)
}

/// Opens a native save dialog and writes the exported session data to the
/// chosen file. Returns the written path, or `None` if the user cancelled.
#[tauri::command]
async fn export_session_to_file(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<Mutex<AppState>>>,
    session_id: String,
    format: String,
) -> Result<Option<String>, String> {
    let export_fmt = match format.to_lowercase().as_str() {
        "csv" => ExportFormat::Csv,
        "json" => ExportFormat::Json,
        _ => return Err(format!("Unsupported export format: {}", format)),
    };

    let (content, label) = {
        let app = state.lock().await;
        let content = app.export_session(&session_id, export_fmt)?;
        let label = app
            .get_session_infos()
            .into_iter()
            .find(|info| info.id == session_id)
            .map(|info| info.label)
            .unwrap_or_else(|| "session".to_string());
        (content, label)
    };

    let (filter_name, ext) = match export_fmt {
        ExportFormat::Csv => ("CSV files", "csv"),
        ExportFormat::Json => ("JSON files", "json"),
    };

    let safe_label: String = label
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();

    let file_path = app_handle
        .dialog()
        .file()
        .add_filter(filter_name, &[ext])
        .set_file_name(format!("{}.{}", safe_label, ext))
        .blocking_save_file();

    let Some(file_path) = file_path else {
        return Ok(None);
    };

    let path = file_path
        .into_path()
        .map_err(|e| format!("Invalid export path: {}", e))?;

    tokio::fs::write(&path, content)
        .await
        .map_err(|e| format!("Failed to write export file: {}", e))?;

    Ok(Some(path.to_string_lossy().to_string()))
}

/// Returns the full data snapshot for a specific session (for graph rendering).
#[tauri::command]
async fn get_session_data(
    state: State<'_, Arc<Mutex<AppState>>>,
    session_id: String,
) -> Result<session::SessionSnapshot, String> {
    let app = state.lock().await;
    app.get_session_snapshot(&session_id)
        .ok_or_else(|| format!("Session not found: {}", session_id))
}

/// Returns all process names currently running on the system (for autocomplete).
#[tauri::command]
async fn get_running_processes() -> Result<Vec<String>, String> {
    monitor::metrics::enumerate_process_names()
}

// ─── Monitoring Loop ──────────────────────────────────────

async fn run_monitoring_loop(
    state: Arc<Mutex<AppState>>,
    app_handle: tauri::AppHandle,
    session_id: String,
    executable: String,
    interval_ms: u64,
) {
    let mut ticker = interval(Duration::from_millis(interval_ms));

    // Cache of cumulative I/O counters per PID so we can compute per-second rates.
    let mut io_cache: HashMap<u32, (u64, u64, Instant)> = HashMap::new();
    // Cache of cumulative network byte counters per PID (ESTATS totals).
    let mut net_cache: HashMap<u32, (u64, u64, Instant)> = HashMap::new();

    loop {
        ticker.tick().await;

        // Check if this session is still active
        {
            let app = state.lock().await;
            if !app.is_session_active(&session_id) {
                break;
            }
        }

        // Resolve PIDs for the target executable
        let pids = match monitor::metrics::find_pids_by_name(&executable) {
            Ok(pids) if !pids.is_empty() => pids,
            _ => {
                // Process not found – emit a status event
                let _ = app_handle.emit(
                    "session-status",
                    serde_json::json!({
                        "sessionId": session_id,
                        "status": "process_not_found",
                        "message": format!("Process '{}' not found", executable),
                    }),
                );
                // Continue polling – the process may restart
                continue;
            }
        };

        // Aggregate metrics across all matching PIDs
        let mut cpu_total = 0.0f64;
        let mut ram_total = 0u64;
        let mut io_read_total = 0u64;      // bytes/sec (rate)
        let mut io_write_total = 0u64;     // bytes/sec (rate)
        let mut io_read_cumulative = 0u64; // bytes (total since start)
        let mut io_write_cumulative = 0u64;
        let mut net_recv_total = 0u64;
        let mut net_sent_total = 0u64;
        let mut net_recv_cumulative = 0u64;
        let mut net_sent_cumulative = 0u64;
        let mut net_conn_total = 0u32;

        let mut valid_pids: Vec<u32> = Vec::new();

        for &pid in &pids {
            match ProcessMetrics::collect(pid) {
                Ok(m) => {
                    cpu_total += m.cpu_percent;
                    ram_total += m.working_set_bytes;

                    // Compute per-second I/O rates from the cumulative counters.
                    let now = Instant::now();
                    let (read_rate, write_rate) = match io_cache.get(&pid) {
                        Some(&(prev_read, prev_write, prev_instant)) => {
                            let elapsed = now.duration_since(prev_instant).as_secs_f64();
                            if elapsed > 0.0 {
                                (
                                    m.io_read_bytes.saturating_sub(prev_read) as f64 / elapsed,
                                    m.io_write_bytes.saturating_sub(prev_write) as f64 / elapsed,
                                )
                            } else {
                                (0.0, 0.0)
                            }
                        }
                        None => (0.0, 0.0),
                    };
                    io_cache.insert(pid, (m.io_read_bytes, m.io_write_bytes, now));

                    io_read_total += read_rate.round() as u64;
                    io_write_total += write_rate.round() as u64;
                    io_read_cumulative += m.io_read_bytes;
                    io_write_cumulative += m.io_write_bytes;
                    valid_pids.push(pid);
                }
                Err(_) => {
                    // Individual PID may have exited between enumeration and query
                    continue;
                }
            }
        }

        if valid_pids.is_empty() {
            let _ = app_handle.emit(
                "session-status",
                serde_json::json!({
                    "sessionId": session_id,
                    "status": "process_terminated",
                    "message": "All monitored processes have exited",
                }),
            );
            // Still store a zero-reading so the graph shows the gap
        }

        // Collect network stats if enabled.
        // When elevated + network toggle ON, use WFP/ESTATS for true byte
        // counters; otherwise fall back to connection-counting.
        let net_enabled = {
            let app = state.lock().await;
            app.is_network_enabled(&session_id)
        };

        let is_elevated = check_elevated();

        if net_enabled {
            for &pid in &valid_pids {
                let use_wfp = net_enabled && is_elevated;
                if let Ok(ns) = NetworkStats::collect(pid, use_wfp) {
                    net_conn_total += ns.connection_count;
                    if ns.using_wfp {
                        // ESTATS returns cumulative byte counters; convert to a
                        // per-second rate using the previous sample for this PID.
                        net_recv_cumulative += ns.bytes_received;
                        net_sent_cumulative += ns.bytes_sent;

                        let now = Instant::now();
                        let (recv_rate, sent_rate) = match net_cache.get(&pid) {
                            Some(&(prev_recv, prev_sent, prev_instant)) => {
                                let elapsed = now.duration_since(prev_instant).as_secs_f64();
                                if elapsed > 0.0 {
                                    (
                                        ns.bytes_received.saturating_sub(prev_recv) as f64 / elapsed,
                                        ns.bytes_sent.saturating_sub(prev_sent) as f64 / elapsed,
                                    )
                                } else {
                                    (0.0, 0.0)
                                }
                            }
                            None => (0.0, 0.0),
                        };
                        net_cache.insert(pid, (ns.bytes_received, ns.bytes_sent, now));
                        net_recv_total += recv_rate.round() as u64;
                        net_sent_total += sent_rate.round() as u64;
                    } else {
                        // Fallback values are per-tick estimates, not cumulative.
                        net_recv_total += ns.bytes_received;
                        net_sent_total += ns.bytes_sent;
                        net_recv_cumulative += ns.bytes_received;
                        net_sent_cumulative += ns.bytes_sent;
                    }
                }
            }
        }

        let data_point = session::DataPoint {
            timestamp: chrono::Utc::now(),
            cpu_percent: (cpu_total * 100.0).round() / 100.0,
            ram_mb: (ram_total as f64 / (1024.0 * 1024.0) * 100.0).round() / 100.0,
            io_read_bytes_per_sec: io_read_total,
            io_write_bytes_per_sec: io_write_total,
            io_read_bytes_total: io_read_cumulative,
            io_write_bytes_total: io_write_cumulative,
            net_recv_bytes_per_sec: net_recv_total,
            net_sent_bytes_per_sec: net_sent_total,
            net_recv_bytes_total: net_recv_cumulative,
            net_sent_bytes_total: net_sent_cumulative,
            net_connection_count: net_conn_total,
            active_pids: valid_pids.clone(),
        };

        // Store the data point
        {
            let mut app = state.lock().await;
            app.push_data_point(&session_id, data_point.clone());
        }

        // Emit real-time data to the frontend
        let _ = app_handle.emit(
            "metrics-update",
            serde_json::json!({
                "sessionId": session_id,
                "dataPoint": data_point,
            }),
        );
    }
}

// ─── Tauri Application Setup ──────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::init();

    let app_state = Arc::new(Mutex::new(AppState::new()));

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            is_elevated,
            request_elevation,
            get_restart_params,
            get_sessions,
            start_monitoring,
            stop_monitoring,
            remove_session,
            export_session_data,
            export_session_to_file,
            get_session_data,
            get_running_processes,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ProcessObserver");
}
