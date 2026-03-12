//! Window management commands for the minimal overlay mode.
//!
//! Two-window architecture:
//!   main    - 1200x800 main application window
//!   overlay - 280x56 transparent always-on-top HUD strip

use crate::store::AppState;
use tauri::{AppHandle, Emitter, Manager, State};
use tracing::info;

const OVERLAY_MARGIN: i32 = 16;

fn get_overlay_size(app: &AppHandle) -> (i32, i32) {
    app.get_webview_window("overlay")
        .and_then(|overlay| overlay.outer_size().ok())
        .map(|size| (size.width as i32, size.height as i32))
        .unwrap_or((280, 56))
}

#[cfg(target_os = "windows")]
fn primary_work_area_bottom_right(app: &AppHandle) -> (i32, i32) {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETWORKAREA};

    let (overlay_w, overlay_h) = get_overlay_size(app);
    let mut rect = RECT::default();
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some((&mut rect as *mut RECT).cast()),
            Default::default(),
        )
    }
    .is_ok();

    if ok {
        let x = (rect.right - overlay_w - OVERLAY_MARGIN).max(rect.left);
        let y = (rect.bottom - overlay_h - OVERLAY_MARGIN).max(rect.top);
        (x, y)
    } else {
        (100, 100)
    }
}

#[cfg(not(target_os = "windows"))]
fn primary_work_area_bottom_right(app: &AppHandle) -> (i32, i32) {
    let (overlay_w, overlay_h) = get_overlay_size(app);
    (
        (1920 - overlay_w - OVERLAY_MARGIN).max(0),
        (1080 - overlay_h - OVERLAY_MARGIN).max(0),
    )
}

async fn persist_overlay_position(
    state: &State<'_, AppState>,
    x: i32,
    y: i32,
) -> Result<(), String> {
    let mut settings = state.settings.lock().await;
    settings.overlay_x = Some(x);
    settings.overlay_y = Some(y);
    settings.save().map_err(|e| e.to_string())
}

pub async fn enter_unattended_im_mode(
    app: &AppHandle,
    state: &crate::store::AppState,
) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or("Main window not found")?;
    let overlay = app
        .get_webview_window("overlay")
        .ok_or("Overlay window not found")?;

    let (x, y) = primary_work_area_bottom_right(app);
    overlay
        .set_position(tauri::PhysicalPosition::new(x, y))
        .map_err(|e| e.to_string())?;

    {
        let mut settings = state.settings.lock().await;
        settings.overlay_x = Some(x);
        settings.overlay_y = Some(y);
        settings.save().map_err(|e| e.to_string())?;
    }

    let _ = main.hide();
    overlay.show().map_err(|e| e.to_string())?;
    overlay.set_always_on_top(true).map_err(|e| e.to_string())?;

    info!("Entered unattended IM mode at ({}, {})", x, y);
    Ok(())
}

// ─── Theme-based window border color (Windows 11+) ──────────────────────────

/// Set the main window title bar and border color to match the app theme.
/// violet → purple (#7c6af7), gold → gold (#c9a84c).
/// Windows 11+ only; no-op on older Windows or non-Windows.
pub async fn apply_app_theme(app: &AppHandle, theme: &str) -> Result<(), String> {
    let theme = theme.trim();
    if theme != "violet" && theme != "gold" {
        return Err(format!("Unsupported theme '{}'", theme));
    }
    app.emit("app_theme_changed", theme.to_string())
        .map_err(|e| e.to_string())?;
    set_window_theme_border(app.clone(), theme.to_string()).await?;
    Ok(())
}

/// Broadcast an application theme change to the frontend and sync the native border color.
#[tauri::command]
pub async fn set_app_theme(app: AppHandle, theme: String) -> Result<(), String> {
    apply_app_theme(&app, &theme).await
}

/// Set the native window title bar and border color to match the app theme.
/// This does not change the frontend theme by itself.
#[tauri::command]
pub async fn set_window_theme_border(_app: AppHandle, theme: String) -> Result<(), String> {
    #[cfg(not(target_os = "windows"))]
    {
        let _ = theme;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Graphics::Dwm::{
            DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR,
        };
        use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

        // COLORREF = 0x00BBGGRR
        let color: u32 = match theme.as_str() {
            "violet" => 0x00F76A7C, // #7c6af7
            "gold" => 0x004CA8C9,   // #c9a84c
            _ => return Ok(()),
        };

        let title: Vec<u16> = "Pisci\0".encode_utf16().collect();
        let hwnd = match unsafe { FindWindowW(PCWSTR::null(), PCWSTR(title.as_ptr())) } {
            Ok(h) if !h.is_invalid() => h,
            _ => return Ok(()), // Window not found yet, ignore
        };
        if hwnd.is_invalid() {
            return Ok(());
        }

        unsafe {
            let _ =
                DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &color as *const _ as *const _, 4);
            let _ =
                DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &color as *const _ as *const _, 4);
        }
        info!("Set window theme border: {}", theme);
        Ok(())
    }
}

/// Switch to minimal overlay mode: hide the main window, show the floating HUD.
///
/// Position logic:
///   1. If a saved position exists in settings, restore it.
///   2. Otherwise, center the overlay relative to the main window's current position.
#[tauri::command]
pub async fn enter_minimal_mode(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or("Main window not found")?;
    let overlay = app
        .get_webview_window("overlay")
        .ok_or("Overlay window not found")?;

    // Determine overlay position
    let (ox, oy) = {
        let settings = state.settings.lock().await;
        if let (Some(x), Some(y)) = (settings.overlay_x, settings.overlay_y) {
            (x, y)
        } else {
            // First launch: center overlay at the bottom-center of the main window
            if let Ok(pos) = main.outer_position() {
                if let Ok(size) = main.outer_size() {
                    let cx = pos.x + (size.width as i32) / 2 - 140; // 280/2
                    let cy = pos.y + (size.height as i32) - 80; // near bottom
                    (cx.max(0), cy.max(0))
                } else {
                    (100, 100)
                }
            } else {
                (100, 100)
            }
        }
    };

    overlay
        .set_position(tauri::PhysicalPosition::new(ox, oy))
        .map_err(|e| e.to_string())?;

    main.hide().map_err(|e| e.to_string())?;
    overlay.show().map_err(|e| e.to_string())?;
    overlay.set_always_on_top(true).map_err(|e| e.to_string())?;

    info!("Entered minimal mode at ({}, {})", ox, oy);
    Ok(())
}

/// Exit minimal overlay mode: hide the HUD strip, show and focus the main window.
#[tauri::command]
pub async fn exit_minimal_mode(app: AppHandle) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or("Main window not found")?;

    // Hide overlay if it exists (best-effort — might not exist in dev mode)
    if let Some(overlay) = app.get_webview_window("overlay") {
        let _ = overlay.hide();
    }

    // Restore the main window: un-minimize if needed, then show and focus
    if main.is_minimized().unwrap_or(false) {
        let _ = main.unminimize();
    }
    main.show().map_err(|e| e.to_string())?;
    main.set_focus().map_err(|e| e.to_string())?;

    info!("Exited minimal mode");
    Ok(())
}

/// Move the overlay window to a specific screen position.
/// Called from the frontend drag handler.
#[tauri::command]
pub async fn set_overlay_position(app: AppHandle, x: i32, y: i32) -> Result<(), String> {
    let overlay = app
        .get_webview_window("overlay")
        .ok_or("Overlay window not found")?;

    overlay
        .set_position(tauri::PhysicalPosition::new(x, y))
        .map_err(|e| e.to_string())
}

/// Persist the overlay window position to settings so it survives restarts.
#[tauri::command]
pub async fn save_overlay_position(
    state: State<'_, AppState>,
    x: i32,
    y: i32,
) -> Result<(), String> {
    persist_overlay_position(&state, x, y).await
}
