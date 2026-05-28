//! UIA mouse calibration store.
//!
//! Cross-platform skeleton; the Windows build adds the real
//! `screen_fingerprint` + `apply()` helpers used by `tools::uia`. Other
//! platforms ship a stub so the crate still compiles.
//!
//! ## Why this exists
//!
//! Even with `PerMonitorV2` DPI awareness declared in
//! `windows-app-manifest.xml`, a small fraction of users still see
//! residual click drift — typically inside VMware/Parallels guests,
//! over RDP sessions, on mixed-DPI multi-monitor setups, or with
//! exotic absolute-positioning pointer drivers (e.g. graphics
//! tablets). The user-facing calibration flow in the Debug → UIA
//! panel captures a linear fit per monitor and persists it here so the
//! next `uia.click(x, y)` / `uia.drag_drop(...)` call automatically
//! compensates.
//!
//! ## Cache invalidation
//!
//! The persisted file embeds a "fingerprint" that hashes
//! everything that could invalidate the fit:
//!   - the full virtual-desktop bounding box,
//!   - each monitor's rect, plus its X / Y dot-per-inch,
//!   - the total monitor count.
//!
//! When the user changes resolution, plugs in a monitor, changes the
//! DPI scaling, etc., the fingerprint changes and the previous
//! calibration is silently dropped — every call returns the identity
//! transform until the user re-runs calibration. This is the contract
//! the spec calls for ("缓存在用户屏幕dpi等发生变化时失效").
//!
//! ## Cross-platform note
//!
//! Several helpers in this file are only consumed by `tools::uia`,
//! which is itself `#[cfg(target_os = "windows")]`. On Linux / macOS
//! they appear "dead". We don't want to scatter `cfg` attributes
//! everywhere because the Tauri commands in
//! `commands/platform/calibration.rs` are registered on every platform
//! (they just return "only supported on Windows"). The simplest fix is
//! to silence the dead-code lint on non-Windows builds at the module
//! level.

#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A linear transform applied to physical mouse coordinates:
/// `corrected = (raw - origin_in_monitor) * scale + offset + origin_in_monitor`.
///
/// `monitor_index` is the index returned by `EnumDisplayMonitors`
/// (matching `screen_capture` action `list_monitors`); `monitor_rect`
/// is the physical rect at fit time and is used for hit-testing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorCalibration {
    pub monitor_index: usize,
    pub monitor_rect: [i32; 4], // [left, top, right, bottom] physical pixels
    pub scale_x: f64,
    pub offset_x: f64,
    pub scale_y: f64,
    pub offset_y: f64,
    /// RMS of the residual (in physical pixels) after the fit; surfaced
    /// to the user so they can decide whether to retry.
    pub residual_rms_px: f64,
    /// Number of (user_target, pisci_actual) pairs that fed the fit.
    pub sample_count: usize,
    /// ISO-8601 timestamp.
    pub calibrated_at: String,
}

/// Persisted file format. The fingerprint is what gates validity; if
/// the user changes any monitor / DPI parameter this no longer matches
/// and the file is treated as absent.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CalibrationFile {
    pub fingerprint: String,
    pub monitors: Vec<MonitorCalibration>,
    /// Schema version. Increment when the on-disk layout changes so we
    /// can refuse to load older formats instead of crashing.
    #[serde(default = "default_version")]
    pub version: u32,
}

fn default_version() -> u32 {
    1
}

const SCHEMA_VERSION: u32 = 1;

/// Where the calibration file lives. The app-data directory varies per
/// platform; we let the caller pass it in to keep this module free of
/// Tauri imports.
pub fn calibration_file_path(app_data_dir: &std::path::Path) -> PathBuf {
    app_data_dir.join("uia_calibration.json")
}

/// Best-effort load. Returns `None` when the file is missing,
/// unreadable, of an unknown schema, or when the embedded fingerprint
/// doesn't match the current display layout (in which case the file is
/// silently discarded by [`apply_to_point`] callers).
pub fn load_file(path: &std::path::Path) -> Option<CalibrationFile> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed: CalibrationFile = serde_json::from_str(&raw).ok()?;
    if parsed.version != SCHEMA_VERSION {
        tracing::warn!(
            "uia_calibration: ignoring file with schema v{} (expected v{})",
            parsed.version,
            SCHEMA_VERSION
        );
        return None;
    }
    Some(parsed)
}

pub fn save_file(path: &std::path::Path, file: &CalibrationFile) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut to_write = file.clone();
    to_write.version = SCHEMA_VERSION;
    let raw = serde_json::to_string_pretty(&to_write)
        .map_err(std::io::Error::other)?;
    std::fs::write(path, raw)
}

pub fn delete_file(path: &std::path::Path) -> std::io::Result<()> {
    if path.exists() {
        std::fs::remove_file(path)
    } else {
        Ok(())
    }
}

// ─── Windows-specific helpers ────────────────────────────────────────

/// Snapshot of the current monitor layout used both as the
/// fingerprint seed and as the source-of-truth when rendering the
/// calibration overlay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorSnapshot {
    pub index: usize,
    pub primary: bool,
    pub rect: [i32; 4], // [left, top, right, bottom] physical pixels
    pub dpi_x: u32,
    pub dpi_y: u32,
    pub scale_percent: u32,
    /// Friendly device name like "DISPLAY1".
    pub device: String,
}

/// Full snapshot the UI consumes via the `uia_calibration_status`
/// command. `virtual_screen` mirrors `SM_*VIRTUALSCREEN`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationStatus {
    pub virtual_screen: [i32; 4], // [vx, vy, vx+vw, vy+vh]
    pub monitors: Vec<MonitorSnapshot>,
    pub fingerprint: String,
    /// Whether the on-disk calibration matches the current layout.
    pub is_valid: bool,
    /// Active per-monitor calibration entries (only set when `is_valid`).
    pub monitors_calibrated: Vec<MonitorCalibration>,
    pub file_path: String,
}

#[cfg(target_os = "windows")]
pub fn current_snapshot() -> CalibrationStatus {
    let virtual_screen = windows_helpers::virtual_screen_rect();
    let monitors = windows_helpers::enumerate_monitors_with_dpi();
    let fingerprint = build_fingerprint(&virtual_screen, &monitors);
    CalibrationStatus {
        virtual_screen,
        monitors,
        fingerprint,
        is_valid: false,
        monitors_calibrated: Vec::new(),
        file_path: String::new(),
    }
}

#[cfg(not(target_os = "windows"))]
pub fn current_snapshot() -> CalibrationStatus {
    CalibrationStatus {
        virtual_screen: [0, 0, 0, 0],
        monitors: Vec::new(),
        fingerprint: String::new(),
        is_valid: false,
        monitors_calibrated: Vec::new(),
        file_path: String::new(),
    }
}

/// Hash everything that can plausibly invalidate the fit. Deliberately
/// includes the full per-monitor rect + DPI tuple so that any one of
/// (resolution change, monitor unplug, DPI scale change, monitor
/// rearrangement) silently invalidates the saved file.
pub fn build_fingerprint(virtual_screen: &[i32; 4], monitors: &[MonitorSnapshot]) -> String {
    let mut parts = Vec::with_capacity(monitors.len() + 1);
    parts.push(format!(
        "vs:{},{},{},{}",
        virtual_screen[0], virtual_screen[1], virtual_screen[2], virtual_screen[3]
    ));
    for m in monitors {
        parts.push(format!(
            "m{}:{},{},{},{}|dpi:{}x{}|sc:{}",
            m.index, m.rect[0], m.rect[1], m.rect[2], m.rect[3], m.dpi_x, m.dpi_y, m.scale_percent,
        ));
    }
    parts.push(format!("count:{}", monitors.len()));
    let joined = parts.join("||");
    // Cheap stable hash — we don't need cryptographic strength, just
    // change-detection. Use a 64-bit DJB-style fold to keep the JSON
    // short and human-inspectable.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in joined.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("v{}-{:016x}", SCHEMA_VERSION, h)
}

/// Apply the calibration to a single physical point. Returns the
/// adjusted point when a matching monitor entry exists; otherwise the
/// input is returned unchanged so callers can use this unconditionally
/// without special-casing the no-calibration path.
pub fn apply_to_point(file: &CalibrationFile, x: i32, y: i32) -> (i32, i32) {
    let Some(entry) = file
        .monitors
        .iter()
        .find(|m| point_in_rect(x, y, m.monitor_rect))
    else {
        return (x, y);
    };

    let [l, t, _r, _b] = entry.monitor_rect;
    // Convert to monitor-local coords so that the linear fit's
    // intercept doesn't have to swallow the monitor origin.
    let local_x = (x - l) as f64;
    let local_y = (y - t) as f64;
    let corrected_local_x = entry.scale_x * local_x + entry.offset_x;
    let corrected_local_y = entry.scale_y * local_y + entry.offset_y;
    let cx = (corrected_local_x + l as f64).round() as i32;
    let cy = (corrected_local_y + t as f64).round() as i32;
    (cx, cy)
}

fn point_in_rect(x: i32, y: i32, rect: [i32; 4]) -> bool {
    let [l, t, r, b] = rect;
    x >= l && x < r && y >= t && y < b
}

/// Ordinary least-squares fit of a 1-D linear model
/// `actual = scale * target + offset`. Returns `(scale, offset,
/// rms_residual)`. For a single sample we fall back to the identity
/// transform; for two we fit the line exactly.
pub fn fit_linear(targets: &[f64], actuals: &[f64]) -> (f64, f64, f64) {
    let n = targets.len().min(actuals.len());
    if n == 0 {
        return (1.0, 0.0, 0.0);
    }
    if n == 1 {
        // Pure translation: offset such that actual = scale*target + offset.
        return (1.0, actuals[0] - targets[0], 0.0);
    }

    let n_f = n as f64;
    let sum_t: f64 = targets.iter().take(n).sum();
    let sum_a: f64 = actuals.iter().take(n).sum();
    let mean_t = sum_t / n_f;
    let mean_a = sum_a / n_f;

    let mut num = 0.0;
    let mut den = 0.0;
    for i in 0..n {
        let dt = targets[i] - mean_t;
        let da = actuals[i] - mean_a;
        num += dt * da;
        den += dt * dt;
    }

    let (scale, offset) = if den.abs() < 1e-9 {
        (1.0, mean_a - mean_t)
    } else {
        let s = num / den;
        (s, mean_a - s * mean_t)
    };

    let mut sq = 0.0;
    for i in 0..n {
        let predicted = scale * targets[i] + offset;
        let resid = actuals[i] - predicted;
        sq += resid * resid;
    }
    let rms = (sq / n_f).sqrt();
    (scale, offset, rms)
}

/// Build a calibration entry given the 5 sample pairs collected on a
/// single monitor. Targets and actuals are in **physical screen
/// coordinates** (not monitor-local).
///
/// The fit itself is done in monitor-local space (matching
/// `apply_to_point`), then the resulting linear transform is
/// expressed as `corrected_local = scale * raw_local + offset`, so
/// `Pisci(corrected)` will land on `User(target)`.
///
/// Why fit `target = scale * actual + offset` (not the other way
/// around)? At calibration time the agent calls `uia.click(target)`
/// and we *observe* `actual`. So the actual->target map is what
/// "undoes" the drift: feeding `corrected = transform(target)` into
/// `uia.click` makes the cursor land on `target` because the same
/// drift takes corrected -> corrected + drift = target + drift_diff
/// — which is exactly what the fit removes.
pub fn fit_monitor_calibration(
    monitor_index: usize,
    monitor_rect: [i32; 4],
    user_targets: &[(i32, i32)],
    pisci_actuals: &[(i32, i32)],
) -> MonitorCalibration {
    let [l, t, _r, _b] = monitor_rect;
    let local_targets_x: Vec<f64> = user_targets.iter().map(|(x, _)| (x - l) as f64).collect();
    let local_targets_y: Vec<f64> = user_targets.iter().map(|(_, y)| (y - t) as f64).collect();
    let local_actuals_x: Vec<f64> = pisci_actuals.iter().map(|(x, _)| (x - l) as f64).collect();
    let local_actuals_y: Vec<f64> = pisci_actuals.iter().map(|(_, y)| (y - t) as f64).collect();

    // We want: when the agent asks to click `target`, send `corrected`
    // such that `actual(corrected) == target`. Empirically
    // `actual(raw) = drift(raw)`; assuming drift is well-approximated
    // by a line, fit `target = a * actual + b` and use that on the
    // requested coordinate. Algebraically this is equivalent to
    // inverting the observed map, but doing the fit this way avoids
    // numerical instability when `scale ≈ 1`.
    let (sx, ox, rms_x) = fit_linear(&local_actuals_x, &local_targets_x);
    let (sy, oy, rms_y) = fit_linear(&local_actuals_y, &local_targets_y);
    let rms = (rms_x * rms_x + rms_y * rms_y).sqrt();

    MonitorCalibration {
        monitor_index,
        monitor_rect,
        scale_x: sx,
        offset_x: ox,
        scale_y: sy,
        offset_y: oy,
        residual_rms_px: rms,
        sample_count: user_targets.len().min(pisci_actuals.len()),
        calibrated_at: chrono::Utc::now().to_rfc3339(),
    }
}

#[cfg(target_os = "windows")]
pub mod windows_helpers {
    use super::MonitorSnapshot;
    use windows::Win32::Foundation::{BOOL, LPARAM, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, MONITORINFOEXW};
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetSystemMetrics, MONITORINFOF_PRIMARY, SM_CXVIRTUALSCREEN,
        SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };

    pub fn virtual_screen_rect() -> [i32; 4] {
        unsafe {
            let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            [vx, vy, vx + vw, vy + vh]
        }
    }

    pub fn enumerate_monitors_with_dpi() -> Vec<MonitorSnapshot> {
        unsafe extern "system" fn cb(
            hmon: windows::Win32::Graphics::Gdi::HMONITOR,
            _hdc: windows::Win32::Graphics::Gdi::HDC,
            _lprect: *mut RECT,
            lparam: LPARAM,
        ) -> BOOL {
            let list = &mut *(lparam.0 as *mut Vec<MonitorSnapshot>);

            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if !GetMonitorInfoW(hmon, &mut info.monitorInfo as *mut _ as *mut _).as_bool() {
                return BOOL(1);
            }
            let r = info.monitorInfo.rcMonitor;
            let primary = (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;

            let mut dpi_x: u32 = 96;
            let mut dpi_y: u32 = 96;
            let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
            let scale_percent = ((dpi_x as f64 / 96.0) * 100.0).round() as u32;

            // device is a UTF-16 fixed buffer; strip trailing NULs.
            let device_raw = &info.szDevice;
            let end = device_raw
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(device_raw.len());
            let device = String::from_utf16_lossy(&device_raw[..end]);

            list.push(MonitorSnapshot {
                index: list.len(),
                primary,
                rect: [r.left, r.top, r.right, r.bottom],
                dpi_x,
                dpi_y,
                scale_percent,
                device,
            });
            BOOL(1)
        }

        let mut monitors: Vec<MonitorSnapshot> = Vec::new();
        unsafe {
            let _ = EnumDisplayMonitors(
                None,
                None,
                Some(cb),
                LPARAM(&mut monitors as *mut _ as isize),
            );
        }
        monitors
    }

    /// Wraps `GetCursorPos` so the calibration runner can read the
    /// physical cursor position after each real click.
    pub fn cursor_position() -> Option<(i32, i32)> {
        unsafe {
            let mut p = POINT::default();
            if GetCursorPos(&mut p).is_ok() {
                Some((p.x, p.y))
            } else {
                None
            }
        }
    }
}

// ─── In-memory active calibration cache ──────────────────────────────
//
// The `uia` tool needs to consult the calibration on every click /
// drag, and we don't want to hit the disk + re-parse JSON each time.
// We cache the file once at process start and reload it whenever a
// calibration command rewrites the file.

use once_cell::sync::Lazy;
use std::sync::RwLock;

#[derive(Debug, Default)]
struct CalibrationCache {
    /// `None` when not yet initialised or when calibration is absent / invalid.
    file: Option<CalibrationFile>,
    /// The fingerprint of the live monitor layout the last time we
    /// loaded the file. If this no longer matches the current layout
    /// we drop the cached entry.
    cached_fingerprint: Option<String>,
    file_path: Option<PathBuf>,
}

static CACHE: Lazy<RwLock<CalibrationCache>> =
    Lazy::new(|| RwLock::new(CalibrationCache::default()));

/// Initialise (or refresh) the cache from disk. Safe to call any
/// number of times. When the live fingerprint mismatches the on-disk
/// fingerprint the cached entry is cleared so subsequent
/// [`apply_active_calibration`] calls return the identity.
pub fn refresh_cache(app_data_dir: &std::path::Path) {
    let path = calibration_file_path(app_data_dir);
    let snapshot = current_snapshot();
    let live_fp = snapshot.fingerprint.clone();
    let loaded = load_file(&path);
    let mut guard = CACHE.write().unwrap();
    guard.file_path = Some(path);
    guard.cached_fingerprint = Some(live_fp.clone());
    guard.file = match loaded {
        Some(f) if f.fingerprint == live_fp => Some(f),
        Some(_) => {
            tracing::info!(
                "uia_calibration: cached file fingerprint mismatch — calibration ignored until re-run"
            );
            None
        }
        None => None,
    };
}

/// Tell the cache to remember a freshly-saved calibration file
/// without re-reading from disk. Called from the finalize command.
pub fn set_cached(file: CalibrationFile) {
    let mut guard = CACHE.write().unwrap();
    guard.cached_fingerprint = Some(file.fingerprint.clone());
    guard.file = Some(file);
}

/// Drop the cached entry (called after the user clears calibration).
pub fn clear_cache() {
    let mut guard = CACHE.write().unwrap();
    guard.file = None;
}

/// Return the active calibration file, performing a fingerprint
/// recheck against the current monitor layout. Returns `None` when no
/// valid calibration is in effect.
pub fn active_calibration() -> Option<CalibrationFile> {
    let guard = CACHE.read().unwrap();
    let file = guard.file.as_ref()?.clone();
    let cached_fp = guard.cached_fingerprint.as_deref();
    // If the recorded fingerprint matches what we last saw, trust it.
    // We avoid recomputing the live fingerprint on every click — that
    // would mean N EnumDisplayMonitors calls per second. The cache is
    // refreshed by `refresh_cache` from the window-event hook.
    if Some(file.fingerprint.as_str()) == cached_fp {
        Some(file)
    } else {
        None
    }
}

/// Apply the active calibration (if any) to a physical point. This is
/// the hot path called from `uia::click`, `uia::drag_drop`, etc.
pub fn apply_active_calibration(x: i32, y: i32) -> (i32, i32) {
    match active_calibration() {
        Some(file) => apply_to_point(&file, x, y),
        None => (x, y),
    }
}
