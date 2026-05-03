use anyhow::Result;
use pisci_kernel::agent::tool::ToolResult;
use serde_json::Value;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDCA, DeleteDC, DeleteObject,
    EnumDisplayMonitors, GetDIBits, GetDC, GetMonitorInfoW, MonitorFromWindow,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    MONITORINFOEXW, MONITOR_DEFAULTTONEAREST, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetAncestor, GetDesktopWindow, GetWindowRect, GetWindowTextW,
    IsIconic, IsWindowVisible, GA_ROOT,
};
use windows::core::PCWSTR;

pub async fn list_monitors() -> Result<ToolResult> {
    // Step 1: enumerate monitors
    struct MonitorInfo {
        rect: RECT,
        primary: bool,
        index: usize,
    }

    unsafe extern "system" fn mon_enum(
        hmon: windows::Win32::Graphics::Gdi::HMONITOR,
        _hdc: windows::Win32::Graphics::Gdi::HDC,
        _lprect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let list = &mut *(lparam.0 as *mut Vec<(windows::Win32::Graphics::Gdi::HMONITOR, MonitorInfo)>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmon, &mut info.monitorInfo as *mut _ as *mut _).as_bool() {
            let primary = (info.monitorInfo.dwFlags & 1) != 0;
            list.push((
                hmon,
                MonitorInfo {
                    rect: info.monitorInfo.rcMonitor,
                    primary,
                    index: list.len(),
                },
            ));
        }
        BOOL(1)
    }

    let mut monitor_list: Vec<(windows::Win32::Graphics::Gdi::HMONITOR, MonitorInfo)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(mon_enum),
            LPARAM(&mut monitor_list as *mut _ as isize),
        );
    }

    // Step 2: enumerate windows and map each to its nearest monitor
    struct WinData {
        windows: Vec<(
            windows::Win32::Graphics::Gdi::HMONITOR,
            String,
            RECT,
        )>,
    }

    unsafe extern "system" fn win_enum_proc(
        hwnd: HWND,
        lparam: LPARAM,
    ) -> BOOL {
        let data = &mut *(lparam.0 as *mut WinData);
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return BOOL(1);
        }
        let root = GetAncestor(hwnd, GA_ROOT);
        if root != hwnd {
            return BOOL(1); // only top-level
        }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buf);
        if len == 0 {
            return BOOL(1);
        }
        let title = String::from_utf16_lossy(&buf[..len as usize]);
        let mut r = std::mem::zeroed::<RECT>();
        if GetWindowRect(hwnd, &mut r).is_ok() {
            let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            data.windows.push((hmon, title, r));
        }
        BOOL(1)
    }

    let mut win_data = WinData {
        windows: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(
            Some(win_enum_proc),
            LPARAM(&mut win_data as *mut _ as isize),
        );
    }

    // Step 3: build output
    let mut lines: Vec<String> = Vec::new();
    for (hmon, mi) in &monitor_list {
        let r = &mi.rect;
        let primary_tag = if mi.primary { " [PRIMARY]" } else { "" };
        lines.push(format!(
            "Monitor {} (index={}): {}x{} at ({},{}){}",
            mi.index,
            mi.index,
            r.right - r.left,
            r.bottom - r.top,
            r.left,
            r.top,
            primary_tag
        ));
        lines.push("  Windows on this monitor:".to_string());
        let wins_on: Vec<_> = win_data
            .windows
            .iter()
            .filter(|(wmon, _, _)| *wmon == *hmon)
            .collect();
        if wins_on.is_empty() {
            lines.push("    (none)".to_string());
        } else {
            for (_, title, wr) in &wins_on {
                lines.push(format!(
                    "    - \"{}\" at ({},{})-({}{})",
                    title, wr.left, wr.top, wr.right, wr.bottom
                ));
            }
        }
    }

    Ok(ToolResult::ok(format!(
        "Found {} monitor(s). Use monitor_index=N with action=capture to screenshot a specific display.\n\n{}",
        monitor_list.len(),
        lines.join("\n")
    )))
}

pub async fn capture_full(input: &Value) -> Result<ToolResult> {
    let monitor_index = input["monitor_index"].as_u64().unwrap_or(0) as usize;

    unsafe extern "system" fn mon_enum(
        _hmon: windows::Win32::Graphics::Gdi::HMONITOR,
        _hdc: windows::Win32::Graphics::Gdi::HDC,
        _lprect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let list = &mut *(lparam.0 as *mut Vec<RECT>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(_hmon, &mut info.monitorInfo as *mut _ as *mut _).as_bool() {
            list.push(info.monitorInfo.rcMonitor);
        }
        BOOL(1)
    }

    let mut rects: Vec<RECT> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(mon_enum),
            LPARAM(&mut rects as *mut _ as isize),
        );
    }

    let rect = rects.get(monitor_index).copied().unwrap_or_else(|| {
        rects.first().copied().unwrap_or(RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        })
    });

    let x = rect.left;
    let y = rect.top;
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;

    tracing::info!(
        "capture_full: monitor_index={} physical rect=({},{})+({}x{})",
        monitor_index,
        x,
        y,
        width,
        height
    );

    unsafe {
        let display_name = windows::core::s!("DISPLAY");
        let hdc = CreateDCA(display_name, None, None, None);
        let pixels = capture_dc_region(hdc, x, y, width, height)?;
        let _ = DeleteDC(hdc);
        super::encode_and_return_with_offset(&pixels, width as u32, height as u32, input, x, y)
    }
}

pub async fn capture_window(input: &Value) -> Result<ToolResult> {
    let title = match input["window_title"].as_str() {
        Some(t) => t,
        None => return Ok(ToolResult::err("capture_window requires window_title")),
    };

    // Try exact match first
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let exact_hwnd = unsafe { FindWindowW(PCWSTR::null(), PCWSTR(wide.as_ptr())) }.ok();

    let hwnd = if let Some(h) = exact_hwnd {
        h
    } else {
        // Partial match via EnumWindows
        struct SearchData {
            title: String,
            hwnd: HWND,
        }
        unsafe extern "system" fn enum_proc(h: HWND, lparam: LPARAM) -> BOOL {
            let data = &mut *(lparam.0 as *mut SearchData);
            if !IsWindowVisible(h).as_bool() {
                return BOOL(1);
            }
            let mut buf = [0u16; 512];
            let len = GetWindowTextW(h, &mut buf);
            if len > 0 {
                let name = String::from_utf16_lossy(&buf[..len as usize]);
                if name.to_lowercase().contains(&data.title.to_lowercase()) {
                    data.hwnd = h;
                    return BOOL(0);
                }
            }
            BOOL(1)
        }
        let mut search = SearchData {
            title: title.to_string(),
            hwnd: HWND(std::ptr::null_mut()),
        };
        unsafe {
            let _ = EnumWindows(
                Some(enum_proc),
                LPARAM(&mut search as *mut SearchData as isize),
            );
        }
        if search.hwnd.0.is_null() {
            return Ok(ToolResult::err(format!("Window '{}' not found", title)));
        }
        search.hwnd
    };

    // Get window rect and capture
    let mut rect = unsafe { std::mem::zeroed::<RECT>() };
    unsafe {
        GetWindowRect(hwnd, &mut rect).map_err(|e| anyhow::anyhow!("{}", e))?;
    }

    let w = rect.right - rect.left;
    let h = rect.bottom - rect.top;
    if w <= 0 || h <= 0 {
        return Ok(ToolResult::err("Window has zero size"));
    }

    unsafe {
        let hdc_win = GetDC(hwnd);
        let mem_dc = CreateCompatibleDC(hdc_win);
        let bitmap = CreateCompatibleBitmap(hdc_win, w, h);
        let old_bmp = SelectObject(mem_dc, bitmap);

        BitBlt(mem_dc, 0, 0, w, h, hdc_win, 0, 0, SRCCOPY)?;

        let pixels = read_bitmap_pixels(mem_dc, bitmap, w, h)?;

        SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(hwnd, hdc_win);

        super::encode_and_return_with_offset(
            &pixels, w as u32, h as u32, input, rect.left, rect.top,
        )
    }
}

pub async fn capture_region(input: &Value) -> Result<ToolResult> {
    let x = input["x"].as_i64().unwrap_or(0) as i32;
    let y = input["y"].as_i64().unwrap_or(0) as i32;
    let w = match input["width"].as_i64() {
        Some(v) if v > 0 => v as i32,
        _ => return Ok(ToolResult::err("capture_region requires width > 0")),
    };
    let h = match input["height"].as_i64() {
        Some(v) if v > 0 => v as i32,
        _ => return Ok(ToolResult::err("capture_region requires height > 0")),
    };

    unsafe {
        let hwnd = GetDesktopWindow();
        let hdc = GetDC(hwnd);
        let pixels = capture_dc_region(hdc, x, y, w, h)?;
        ReleaseDC(hwnd, hdc);
        super::encode_and_return_with_offset(&pixels, w as u32, h as u32, input, x, y)
    }
}

// ─── Internal helpers ─────────────────────────────────────────────────────

unsafe fn capture_dc_region(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<Vec<u8>> {
    let mem_dc = CreateCompatibleDC(hdc);
    let bitmap = CreateCompatibleBitmap(hdc, width, height);
    let old_bmp = SelectObject(mem_dc, bitmap);

    BitBlt(mem_dc, 0, 0, width, height, hdc, x, y, SRCCOPY)?;

    let pixels = read_bitmap_pixels(mem_dc, bitmap, width, height)?;

    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(mem_dc);

    Ok(pixels)
}

unsafe fn read_bitmap_pixels(
    mem_dc: windows::Win32::Graphics::Gdi::HDC,
    bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
    width: i32,
    height: i32,
) -> Result<Vec<u8>> {
    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [Default::default()],
    };

    let buf_size = (width * height * 4) as usize;
    let mut pixels = vec![0u8; buf_size];

    GetDIBits(
        mem_dc,
        bitmap,
        0,
        height as u32,
        Some(pixels.as_mut_ptr() as *mut _),
        &mut bmi,
        DIB_RGB_COLORS,
    );

    // Convert BGRA -> RGBA
    for chunk in pixels.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }

    Ok(pixels)
}
