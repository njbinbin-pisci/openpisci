#[cfg(target_os = "windows")]
pub fn get_dpi_scale() -> f64 {
    use windows::Win32::UI::HiDpi::{
        GetDpiForSystem, SetProcessDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let dpi = GetDpiForSystem();
        dpi as f64 / 96.0
    }
}

#[cfg(target_os = "windows")]
pub fn get_monitor_dpi(x: i32, y: i32) -> f64 {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONEAREST};
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

    unsafe {
        let point = POINT { x, y };
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut dpi_x = 0u32;
        let mut dpi_y = 0u32;
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok() {
            dpi_x as f64 / 96.0
        } else {
            1.0
        }
    }
}

#[cfg(target_os = "windows")]
pub fn logical_to_physical(x: i32, y: i32, scale: f64) -> (i32, i32) {
    ((x as f64 * scale) as i32, (y as f64 * scale) as i32)
}

#[cfg(target_os = "windows")]
pub fn physical_to_logical(x: i32, y: i32, scale: f64) -> (i32, i32) {
    ((x as f64 / scale) as i32, (y as f64 / scale) as i32)
}

#[cfg(not(target_os = "windows"))]
pub fn get_dpi_scale() -> f64 {
    1.0
}

#[cfg(not(target_os = "windows"))]
pub fn get_monitor_dpi(_x: i32, _y: i32) -> f64 {
    1.0
}

#[cfg(not(target_os = "windows"))]
pub fn logical_to_physical(x: i32, y: i32, _scale: f64) -> (i32, i32) {
    (x, y)
}

#[cfg(not(target_os = "windows"))]
pub fn physical_to_logical(x: i32, y: i32, _scale: f64) -> (i32, i32) {
    (x, y)
}
