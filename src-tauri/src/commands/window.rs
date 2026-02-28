/// Window management commands for the minimal overlay mode.
///
/// Two-window architecture:
///   main    — 1200×800 main application window
///   overlay — 280×56 transparent always-on-top HUD strip

use tauri::{AppHandle, Manager};
use tracing::info;

// ─── Theme-based window border color (Windows 11+) ──────────────────────────

/// Set the main window title bar and border color to match the app theme.
/// violet → purple (#7c6af7), gold → gold (#c9a84c).
/// Windows 11+ only; no-op on older Windows or non-Windows.
#[tauri::command]
pub async fn set_window_theme_border(app: AppHandle, theme: String) -> Result<(), String> {
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, theme);
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR};
        use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

        // COLORREF = 0x00BBGGRR
        let color: u32 = match theme.as_str() {
            "violet" => 0x00F76A7C, // #7c6af7
            "gold"   => 0x004CA8C9, // #c9a84c
            _        => return Ok(()),
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
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &color as *const _ as *const _, 4);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &color as *const _ as *const _, 4);
        }
        info!("Set window theme border: {}", theme);
        Ok(())
    }
}

/// Switch to minimal overlay mode: hide the main window, show the floating ball.
#[tauri::command]
pub async fn enter_minimal_mode(app: AppHandle) -> Result<(), String> {
    let main = app.get_webview_window("main")
        .ok_or("Main window not found")?;
    let overlay = app.get_webview_window("overlay")
        .ok_or("Overlay window not found")?;

    main.hide().map_err(|e| e.to_string())?;
    overlay.show().map_err(|e| e.to_string())?;
    overlay.set_always_on_top(true).map_err(|e| e.to_string())?;

    info!("Entered minimal mode");
    Ok(())
}

/// Exit minimal overlay mode: hide the HUD strip, show and focus the main window.
#[tauri::command]
pub async fn exit_minimal_mode(app: AppHandle) -> Result<(), String> {
    let main = app.get_webview_window("main")
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
    let overlay = app.get_webview_window("overlay")
        .ok_or("Overlay window not found")?;

    overlay
        .set_position(tauri::PhysicalPosition::new(x, y))
        .map_err(|e| e.to_string())
}
