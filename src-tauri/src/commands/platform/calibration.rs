//! Tauri commands that drive the manual UIA mouse-calibration flow.
//!
//! The user-visible flow lives in `src/components/Debug/index.tsx`
//! (UIA panel). The high-level state machine is:
//!
//! 1. `uia_calibration_status` — UI loads to display monitors + DPI
//!    plus existing calibration validity.
//! 2. `uia_calibration_open_overlay(monitor_index)` — backend creates a
//!    full-screen always-on-top borderless window on that monitor and
//!    points the webview at `index.html?calibration=<monitor_index>`,
//!    which renders the 5 numbered circles.
//! 3. User clicks each circle in order; the frontend collects the
//!    physical screen coordinates of each click.
//! 4. `uia_calibration_run_phase2(monitor_index, user_points)` — backend
//!    re-issues those clicks through `uia::click` with calibration
//!    *bypassed*, then reads back the actual cursor position via
//!    `GetCursorPos`. Streams progress events along the way.
//! 5. `uia_calibration_finalize(monitor_index, user_points,
//!    pisci_actuals)` — fits the linear model, persists to disk,
//!    refreshes the in-memory cache.
//! 6. `uia_calibration_close_overlay` — tears down the window.
//!
//! All commands are no-ops on non-Windows hosts so the same Tauri
//! handler list can be registered everywhere.

use crate::store::AppState;
use crate::tools::calibration::{self, CalibrationFile, CalibrationStatus, MonitorCalibration};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
// `Emitter` is only used inside the Windows Phase 2 driver; importing
// it unconditionally would warn on Linux / macOS, where the impl is a
// thin error-returning stub.
#[cfg(target_os = "windows")]
use tauri::Emitter;

// ─── Status / read commands ──────────────────────────────────────────

/// Return the current monitor layout, the live screen fingerprint, and
/// whether a saved calibration is still valid (fingerprint matches).
#[tauri::command]
pub async fn uia_calibration_status(app: AppHandle) -> Result<CalibrationStatus, String> {
    // Re-read the disk file + recompute the live fingerprint so that
    // resolution / DPI changes since the last `refresh_cache_from_app`
    // call are reflected here. Cheap: O(monitor count).
    let app_data_dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
    calibration::refresh_cache(&app_data_dir);

    let snapshot = calibration::current_snapshot();
    let file_path = calibration::calibration_file_path(&app_data_dir);
    let file = calibration::load_file(&file_path);

    let (is_valid, monitors_calibrated) = match file {
        Some(f) if f.fingerprint == snapshot.fingerprint => (true, f.monitors.clone()),
        _ => (false, Vec::new()),
    };

    Ok(CalibrationStatus {
        virtual_screen: snapshot.virtual_screen,
        monitors: snapshot.monitors,
        fingerprint: snapshot.fingerprint,
        is_valid,
        monitors_calibrated,
        file_path: file_path.to_string_lossy().into_owned(),
    })
}

/// Delete the saved calibration and clear the in-memory cache.
#[tauri::command]
pub async fn uia_calibration_clear(app: AppHandle) -> Result<(), String> {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
    let path = calibration::calibration_file_path(&app_data_dir);
    calibration::delete_file(&path).map_err(|e| e.to_string())?;
    calibration::clear_cache();
    Ok(())
}

// ─── Overlay management (Windows only does the real work) ────────────

const OVERLAY_LABEL: &str = "uia_calibration_overlay";

/// Open a borderless always-on-top full-screen overlay window on the
/// given monitor. The window URL is `index.html?calibration=<idx>`,
/// which the React entry point routes to `<CalibrationOverlay/>`.
#[tauri::command]
pub async fn uia_calibration_open_overlay(
    app: AppHandle,
    monitor_index: usize,
) -> Result<OverlayInfo, String> {
    open_overlay_impl(app, monitor_index).await
}

#[cfg(target_os = "windows")]
async fn open_overlay_impl(app: AppHandle, monitor_index: usize) -> Result<OverlayInfo, String> {
    use tauri::{PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};

    // Close any previous overlay first to avoid stacking.
    if let Some(existing) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = existing.close();
        // Give Tauri a tick to actually release the label before reusing it.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    }

    let snapshot = calibration::current_snapshot();
    let monitor = snapshot
        .monitors
        .get(monitor_index)
        .ok_or_else(|| format!("Monitor index {} out of range", monitor_index))?
        .clone();
    let [l, t, r, b] = monitor.rect;
    let width = (r - l).max(1) as u32;
    let height = (b - t).max(1) as u32;

    let url = format!("index.html?calibration={}", monitor_index);
    let window = WebviewWindowBuilder::new(&app, OVERLAY_LABEL, WebviewUrl::App(url.into()))
        .title("Pisci UIA Calibration")
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .visible(false)
        .build()
        .map_err(|e| format!("Failed to create overlay window: {e}"))?;

    // Position + size in *physical* pixels — these match the monitor
    // rect exactly because the process is PerMonitorV2-aware.
    window
        .set_position(PhysicalPosition::new(l, t))
        .map_err(|e| e.to_string())?;
    window
        .set_size(PhysicalSize::new(width, height))
        .map_err(|e| e.to_string())?;
    window.show().map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())?;

    Ok(OverlayInfo {
        monitor_index,
        monitor_rect: monitor.rect,
        dpi_x: monitor.dpi_x,
        dpi_y: monitor.dpi_y,
        scale_percent: monitor.scale_percent,
    })
}

#[cfg(not(target_os = "windows"))]
async fn open_overlay_impl(_app: AppHandle, _monitor_index: usize) -> Result<OverlayInfo, String> {
    Err("UIA calibration overlay is only available on Windows.".to_string())
}

#[tauri::command]
pub async fn uia_calibration_close_overlay(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = window.close();
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayInfo {
    pub monitor_index: usize,
    pub monitor_rect: [i32; 4],
    pub dpi_x: u32,
    pub dpi_y: u32,
    pub scale_percent: u32,
}

// ─── Phase 2 — Pisci performs the same clicks ────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Phase2Request {
    // Both fields are consumed only inside `#[cfg(target_os =
    // "windows")] run_phase2_impl`; on other platforms the request is
    // ignored. Allow `dead_code` so the cross-platform build stays
    // clean without scattering cfg attrs on every field.
    #[allow(dead_code)]
    pub monitor_index: usize,
    /// 5 physical-pixel `(x, y)` pairs as the user clicked them.
    #[allow(dead_code)]
    pub user_points: Vec<[i32; 2]>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Phase2Result {
    /// Per-target click report; same length and order as `user_points`.
    pub samples: Vec<ClickSample>,
    pub cancelled: bool,
    pub timed_out: bool,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClickSample {
    pub target: [i32; 2],
    /// `None` when this target was never reached (cancelled / timed out).
    pub actual: Option<[i32; 2]>,
    /// Euclidean distance in physical pixels.
    pub distance_px: Option<f64>,
    pub error: Option<String>,
}

/// Run Phase 2: for each target in `user_points`, ask the UIA mouse
/// helper to click the raw physical pixel (calibration bypassed),
/// then sample `GetCursorPos` 80 ms later to record where the cursor
/// actually landed. Streams `uia_calibration_phase2_progress` events to
/// the UI between clicks, and respects a global ESC cancel + 180 s
/// timeout.
#[tauri::command]
pub async fn uia_calibration_run_phase2(
    app: AppHandle,
    state: State<'_, AppState>,
    request: Phase2Request,
) -> Result<Phase2Result, String> {
    let _ = state;
    run_phase2_impl(app, request).await
}

#[cfg(target_os = "windows")]
async fn run_phase2_impl(app: AppHandle, request: Phase2Request) -> Result<Phase2Result, String> {
    use crate::tools::calibration::windows_helpers;
    use pisci_kernel::agent::tool::{Tool, ToolContext, ToolSettings};
    use serde_json::json;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    let started = std::time::Instant::now();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    register_phase2_cancel(cancel_flag.clone()).await;

    let mut samples: Vec<ClickSample> = Vec::with_capacity(request.user_points.len());
    let uia = crate::tools::uia::UiaTool;
    let ctx = ToolContext {
        session_id: "uia_calibration_phase2".to_string(),
        workspace_root: std::env::temp_dir(),
        bypass_permissions: true,
        settings: Arc::new(ToolSettings::default()),
        max_iterations: Some(1),
        memory_owner_id: "pisci".to_string(),
        pool_session_id: None,
        cancel: cancel_flag.clone(),
    };

    let timeout = std::time::Duration::from_secs(180);
    let mut timed_out = false;
    let mut cancelled = false;

    for (i, point) in request.user_points.iter().enumerate() {
        if cancel_flag.load(Ordering::SeqCst) {
            cancelled = true;
            break;
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            break;
        }

        let target = *point;
        let click_input = json!({
            "action": "click",
            "x": target[0],
            "y": target[1],
            "_skip_calibration": true,
        });

        let _ = app.emit(
            "uia_calibration_phase2_progress",
            serde_json::json!({
                "index": i,
                "total": request.user_points.len(),
                "phase": "clicking",
                "target": target,
            }),
        );

        let click_result = uia.call(click_input, &ctx).await;
        // Settle delay so the OS finishes processing the synthetic click
        // and the cursor has visibly stopped moving before we sample it.
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;

        match click_result {
            Ok(_) => match windows_helpers::cursor_position() {
                Some((ax, ay)) => {
                    let dx = (ax - target[0]) as f64;
                    let dy = (ay - target[1]) as f64;
                    let distance = (dx * dx + dy * dy).sqrt();
                    let sample = ClickSample {
                        target,
                        actual: Some([ax, ay]),
                        distance_px: Some(distance),
                        error: None,
                    };
                    samples.push(sample.clone());
                    let _ = app.emit(
                        "uia_calibration_phase2_progress",
                        serde_json::json!({
                            "index": i,
                            "total": request.user_points.len(),
                            "phase": "measured",
                            "target": target,
                            "actual": [ax, ay],
                            "distance_px": distance,
                        }),
                    );
                }
                None => {
                    samples.push(ClickSample {
                        target,
                        actual: None,
                        distance_px: None,
                        error: Some("GetCursorPos failed".to_string()),
                    });
                }
            },
            Err(e) => {
                samples.push(ClickSample {
                    target,
                    actual: None,
                    distance_px: None,
                    error: Some(format!("uia.click failed: {e}")),
                });
            }
        }

        // 500 ms between clicks so the visual countdown / state updates
        // remain readable and the OS-level click queue stays empty.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    clear_phase2_cancel().await;

    Ok(Phase2Result {
        samples,
        cancelled,
        timed_out,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[cfg(not(target_os = "windows"))]
async fn run_phase2_impl(_app: AppHandle, _request: Phase2Request) -> Result<Phase2Result, String> {
    Err("UIA calibration is only available on Windows.".to_string())
}

/// Allow the frontend (or the ESC keystroke handler) to cancel the
/// in-flight Phase 2 loop.
/// Signal an in-flight Phase 2 calibration loop to stop (no-op if idle).
pub async fn cancel_phase2_if_running() {
    if let Some(flag) = take_phase2_cancel().await {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tauri::command]
pub async fn uia_calibration_cancel_phase2() -> Result<(), String> {
    cancel_phase2_if_running().await;
    Ok(())
}

// In-memory single-slot cancel flag for the active Phase 2 run.
// `register_phase2_cancel` / `clear_phase2_cancel` are only invoked
// from the Windows phase-2 driver; on other platforms the cancel
// command still compiles (it just no-ops if no flag was registered).
use once_cell::sync::Lazy;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

static PHASE2_CANCEL: Lazy<TokioMutex<Option<Arc<AtomicBool>>>> =
    Lazy::new(|| TokioMutex::new(None));

#[cfg(target_os = "windows")]
async fn register_phase2_cancel(flag: Arc<AtomicBool>) {
    let mut guard = PHASE2_CANCEL.lock().await;
    *guard = Some(flag);
}

#[cfg(target_os = "windows")]
async fn clear_phase2_cancel() {
    let mut guard = PHASE2_CANCEL.lock().await;
    *guard = None;
}

async fn take_phase2_cancel() -> Option<Arc<AtomicBool>> {
    let guard = PHASE2_CANCEL.lock().await;
    guard.clone()
}

// ─── Finalize: fit + persist ─────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct FinalizeRequest {
    pub monitor_index: usize,
    pub user_points: Vec<[i32; 2]>,
    pub pisci_actuals: Vec<[i32; 2]>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FinalizeResult {
    pub monitor: MonitorCalibration,
    pub file_path: String,
}

#[tauri::command]
pub async fn uia_calibration_finalize(
    app: AppHandle,
    request: FinalizeRequest,
) -> Result<FinalizeResult, String> {
    if request.user_points.is_empty() {
        return Err("No user points to fit".to_string());
    }
    if request.user_points.len() != request.pisci_actuals.len() {
        return Err(format!(
            "user_points/pisci_actuals length mismatch: {} vs {}",
            request.user_points.len(),
            request.pisci_actuals.len()
        ));
    }

    let snapshot = calibration::current_snapshot();
    let monitor_snapshot = snapshot
        .monitors
        .get(request.monitor_index)
        .ok_or_else(|| format!("Monitor index {} out of range", request.monitor_index))?
        .clone();

    let user_pairs: Vec<(i32, i32)> = request.user_points.iter().map(|p| (p[0], p[1])).collect();
    let actual_pairs: Vec<(i32, i32)> =
        request.pisci_actuals.iter().map(|p| (p[0], p[1])).collect();

    let fit = calibration::fit_monitor_calibration(
        request.monitor_index,
        monitor_snapshot.rect,
        &user_pairs,
        &actual_pairs,
    );

    // Load any existing file so calibrations on other monitors are preserved.
    let app_data_dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
    let file_path = calibration::calibration_file_path(&app_data_dir);
    let mut file = match calibration::load_file(&file_path) {
        Some(f) if f.fingerprint == snapshot.fingerprint => f,
        _ => CalibrationFile {
            fingerprint: snapshot.fingerprint.clone(),
            monitors: Vec::new(),
            version: 1,
        },
    };

    file.monitors
        .retain(|m| m.monitor_index != request.monitor_index);
    file.monitors.push(fit.clone());
    file.fingerprint = snapshot.fingerprint.clone();

    calibration::save_file(&file_path, &file).map_err(|e| e.to_string())?;
    calibration::set_cached(file);

    Ok(FinalizeResult {
        monitor: fit,
        file_path: file_path.to_string_lossy().into_owned(),
    })
}

// ─── Refresh-on-startup hook ─────────────────────────────────────────

/// Called from the app bootstrap so the cache is hot the first time
/// `uia::click` runs. Idempotent.
pub fn refresh_cache_from_app(app: &AppHandle) {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
    calibration::refresh_cache(&app_data_dir);
}
