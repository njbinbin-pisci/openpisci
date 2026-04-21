use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
/// Screen capture tool with Vision AI support (Windows only)
/// Supports full screen, specific window, region, and multi-monitor capture.
use pisci_kernel::agent::tool::{ImageData, Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

pub struct ScreenTool;

#[async_trait]
impl Tool for ScreenTool {
    fn name(&self) -> &str {
        "screen_capture"
    }

    fn description(&self) -> &str {
        "Capture a screenshot of the full screen, a specific window, or a screen region. \
         Returns a base64-encoded PNG image that Vision AI can analyze. \
         Use 'find_element' action to visually locate UI elements when UIA fails. \
         Use 'list_monitors' to discover available displays."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["capture", "capture_window", "capture_region", "list_monitors", "find_element"],
                    "description": "capture: full screen; capture_window: specific window by title; capture_region: x/y/width/height; list_monitors: enumerate displays; find_element: capture + describe for Vision AI"
                },
                "window_title": {
                    "type": "string",
                    "description": "Window title (partial match) for capture_window"
                },
                "x": {
                    "type": "integer",
                    "description": "Region left X for capture_region"
                },
                "y": {
                    "type": "integer",
                    "description": "Region top Y for capture_region"
                },
                "width": {
                    "type": "integer",
                    "description": "Region width for capture_region"
                },
                "height": {
                    "type": "integer",
                    "description": "Region height for capture_region"
                },
                "monitor_index": {
                    "type": "integer",
                    "description": "Monitor index (0-based) for capture (default: primary)"
                },
                "element_description": {
                    "type": "string",
                    "description": "Description of the UI element to find (for find_element action)"
                },
                "format": {
                    "type": "string",
                    "enum": ["png", "jpeg"],
                    "description": "Image format (default: jpeg for smaller size)"
                },
                "quality": {
                    "type": "integer",
                    "description": "JPEG quality 1-100 (default: 75)"
                },
                "grid": {
                    "type": "boolean",
                    "description": "Overlay a coordinate grid on the screenshot. Grid lines every 100px with coordinate labels. For capture_window the labels show absolute screen coordinates so they can be used directly with uia click/drag. Useful for Vision AI to precisely locate UI elements."
                },
                "grid_spacing": {
                    "type": "integer",
                    "description": "Grid line spacing in pixels (default: 100)"
                }
            },
            "required": ["action"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let action = match input["action"].as_str() {
            Some(a) => a,
            None => return Ok(ToolResult::err("Missing required parameter: action")),
        };

        match action {
            "list_monitors" => self.list_monitors(),
            "capture" => self.capture_full(&input),
            "capture_window" => self.capture_window(&input),
            "capture_region" => self.capture_region(&input),
            "find_element" => self.capture_full(&input), // capture + return image for Vision AI
            _ => Ok(ToolResult::err(format!("Unknown action: {}", action))),
        }
    }
}

impl ScreenTool {
    // ─── Monitor enumeration ─────────────────────────────────────────────────

    fn list_monitors(&self) -> Result<ToolResult> {
        use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
        use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, MONITORINFOEXW};
        use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
        use windows::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetAncestor, GetWindowRect, GetWindowTextW, IsIconic, IsWindowVisible,
            GA_ROOT,
        };

        // Step 1: enumerate monitors
        struct MonitorInfo {
            rect: RECT,
            primary: bool,
            index: usize,
        }

        unsafe extern "system" fn monitor_enum_proc(
            hmonitor: windows::Win32::Graphics::Gdi::HMONITOR,
            _hdc: windows::Win32::Graphics::Gdi::HDC,
            _lprect: *mut RECT,
            lparam: LPARAM,
        ) -> BOOL {
            let list = &mut *(lparam.0
                as *mut Vec<(windows::Win32::Graphics::Gdi::HMONITOR, MonitorInfo)>);
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(hmonitor, &mut info.monitorInfo as *mut _ as *mut _).as_bool() {
                let idx = list.len();
                list.push((
                    hmonitor,
                    MonitorInfo {
                        rect: info.monitorInfo.rcMonitor,
                        primary: info.monitorInfo.dwFlags & 1 != 0,
                        index: idx,
                    },
                ));
            }
            BOOL(1)
        }

        let mut monitor_list: Vec<(windows::Win32::Graphics::Gdi::HMONITOR, MonitorInfo)> =
            Vec::new();
        unsafe {
            let _ = EnumDisplayMonitors(
                None,
                None,
                Some(monitor_enum_proc),
                LPARAM(&mut monitor_list as *mut _ as isize),
            );
        }

        // Step 2: enumerate visible top-level windows and assign to monitors
        struct WinData {
            // (monitor_handle, title, rect)
            windows: Vec<(windows::Win32::Graphics::Gdi::HMONITOR, String, RECT)>,
        }

        unsafe extern "system" fn win_enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let data = &mut *(lparam.0 as *mut WinData);
            // Only visible, non-minimized top-level windows with a title
            if !IsWindowVisible(hwnd).as_bool() {
                return BOOL(1);
            }
            if IsIconic(hwnd).as_bool() {
                return BOOL(1);
            }
            // Must be a root window (no owner/parent that is also a window)
            let root = GetAncestor(hwnd, GA_ROOT);
            if root != hwnd {
                return BOOL(1);
            }

            let mut buf = [0u16; 256];
            let len = GetWindowTextW(hwnd, &mut buf);
            if len <= 0 {
                return BOOL(1);
            }
            let title = String::from_utf16_lossy(&buf[..len as usize]);
            if title.trim().is_empty() {
                return BOOL(1);
            }

            let mut rect = std::mem::zeroed::<RECT>();
            if GetWindowRect(hwnd, &mut rect).is_err() {
                return BOOL(1);
            }
            // Skip tiny/offscreen windows
            if rect.right - rect.left < 10 || rect.bottom - rect.top < 10 {
                return BOOL(1);
            }

            let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            data.windows.push((hmon, title, rect));
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

    // ─── Full screen capture ─────────────────────────────────────────────────

    pub(crate) fn capture_full(&self, input: &Value) -> Result<ToolResult> {
        use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
        use windows::Win32::Graphics::Gdi::{
            CreateDCA, DeleteDC, EnumDisplayMonitors, GetMonitorInfoW, MONITORINFOEXW,
        };

        let monitor_index = input["monitor_index"].as_u64().unwrap_or(0) as usize;

        // Enumerate monitors to find the target monitor's physical rect.
        // GetMonitorInfoW returns physical pixel coordinates (screen coordinates).
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

        // Use "DISPLAY" DC which covers the full virtual desktop in physical pixel coordinates.
        // This is the same coordinate space as SM_CXVIRTUALSCREEN / SendInput+VIRTUALDESK.
        unsafe {
            let display_name = windows::core::s!("DISPLAY");
            let hdc = CreateDCA(display_name, None, None, None);
            let pixels = self.capture_dc_region(hdc, x, y, width, height)?;
            let _ = DeleteDC(hdc);
            self.encode_and_return_with_offset(&pixels, width as u32, height as u32, input, x, y)
        }
    }

    // ─── Window capture ───────────────────────────────────────────────────────

    fn capture_window(&self, input: &Value) -> Result<ToolResult> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            EnumWindows, FindWindowW, GetWindowRect, GetWindowTextW, IsWindowVisible,
        };

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
        let mut rect = unsafe { std::mem::zeroed::<windows::Win32::Foundation::RECT>() };
        unsafe {
            GetWindowRect(hwnd, &mut rect).map_err(|e| anyhow::anyhow!("{}", e))?;
        }

        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        if w <= 0 || h <= 0 {
            return Ok(ToolResult::err("Window has zero size"));
        }

        use windows::Win32::Graphics::Gdi::{
            BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
            ReleaseDC, SelectObject, SRCCOPY,
        };

        unsafe {
            let hdc_win = GetDC(hwnd);
            let mem_dc = CreateCompatibleDC(hdc_win);
            let bitmap = CreateCompatibleBitmap(hdc_win, w, h);
            let old_bmp = SelectObject(mem_dc, bitmap);

            BitBlt(mem_dc, 0, 0, w, h, hdc_win, 0, 0, SRCCOPY)?;

            let pixels = self.read_bitmap_pixels(mem_dc, bitmap, w, h)?;

            SelectObject(mem_dc, old_bmp);
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(mem_dc);
            ReleaseDC(hwnd, hdc_win);

            self.encode_and_return_with_offset(
                &pixels, w as u32, h as u32, input, rect.left, rect.top,
            )
        }
    }

    // ─── Region capture ───────────────────────────────────────────────────────

    fn capture_region(&self, input: &Value) -> Result<ToolResult> {
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

        use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC};
        use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;

        unsafe {
            let hwnd = GetDesktopWindow();
            let hdc = GetDC(hwnd);
            let pixels = self.capture_dc_region(hdc, x, y, w, h)?;
            ReleaseDC(hwnd, hdc);
            self.encode_and_return_with_offset(&pixels, w as u32, h as u32, input, x, y)
        }
    }

    // ─── Internal helpers ─────────────────────────────────────────────────────

    unsafe fn capture_dc_region(
        &self,
        hdc: windows::Win32::Graphics::Gdi::HDC,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Result<Vec<u8>> {
        use windows::Win32::Graphics::Gdi::{
            BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
            SelectObject, SRCCOPY,
        };

        let mem_dc = CreateCompatibleDC(hdc);
        let bitmap = CreateCompatibleBitmap(hdc, width, height);
        let old_bmp = SelectObject(mem_dc, bitmap);

        BitBlt(mem_dc, 0, 0, width, height, hdc, x, y, SRCCOPY)?;

        let pixels = self.read_bitmap_pixels(mem_dc, bitmap, width, height)?;

        SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(mem_dc);

        Ok(pixels)
    }

    unsafe fn read_bitmap_pixels(
        &self,
        mem_dc: windows::Win32::Graphics::Gdi::HDC,
        bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
        width: i32,
        height: i32,
    ) -> Result<Vec<u8>> {
        use windows::Win32::Graphics::Gdi::{
            GetDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        };

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

        // Convert BGRA → RGBA
        for chunk in pixels.chunks_exact_mut(4) {
            chunk.swap(0, 2);
        }

        Ok(pixels)
    }

    #[allow(dead_code)]
    fn encode_and_return(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        input: &Value,
    ) -> Result<ToolResult> {
        self.encode_and_return_with_offset(rgba, width, height, input, 0, 0)
    }

    /// Encode pixels to image, optionally overlay a coordinate grid, and return as ToolResult.
    /// `origin_x/origin_y`: screen coordinates of the image's top-left corner.
    /// When grid=true, labels show absolute screen coords (origin + image offset).
    fn encode_and_return_with_offset(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        input: &Value,
        origin_x: i32,
        origin_y: i32,
    ) -> Result<ToolResult> {
        let format = input["format"].as_str().unwrap_or("jpeg");
        let quality = input["quality"].as_u64().unwrap_or(75) as u8;
        let draw_grid = input["grid"].as_bool().unwrap_or(false);
        let grid_spacing = input["grid_spacing"].as_u64().unwrap_or(200).max(50) as u32;

        tracing::info!(
            "screen_capture: {}x{} origin=({},{}) grid={} spacing={}",
            width,
            height,
            origin_x,
            origin_y,
            draw_grid,
            grid_spacing
        );

        let mut img = image::RgbaImage::from_raw(width, height, rgba.to_vec())
            .ok_or_else(|| anyhow::anyhow!("Failed to create image from pixel data"))?;

        if draw_grid {
            self.draw_coordinate_grid(&mut img, origin_x, origin_y, grid_spacing);
            // Save a debug copy as PNG so we can inspect the grid visually
            #[cfg(debug_assertions)]
            {
                let debug_path = std::env::temp_dir().join("pisci_grid_debug.png");
                let _ = img.save(&debug_path);
                tracing::info!(
                    "screen_capture: grid image saved to {}",
                    debug_path.display()
                );
            }
        }

        let (encoded, media_type) = match format {
            "png" => {
                use image::ImageEncoder;
                let mut buf = Vec::new();
                let encoder = image::codecs::png::PngEncoder::new(&mut buf);
                encoder.write_image(img.as_raw(), width, height, image::ColorType::Rgba8.into())?;
                (buf, "image/png")
            }
            _ => {
                let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
                let mut buf = Vec::new();
                let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
                use image::ImageEncoder;
                encoder.write_image(rgb.as_raw(), width, height, image::ColorType::Rgb8.into())?;
                (buf, "image/jpeg")
            }
        };

        let b64 = base64::engine::general_purpose::STANDARD.encode(&encoded);
        let size_kb = encoded.len() / 1024;

        #[cfg(target_os = "windows")]
        let coord_note = {
            let grid_note = if draw_grid {
                format!(
                    " Grid overlay: lines every {}px. Labels show absolute physical screen coordinates \
                     (directly usable for uia click/drag_drop x/y parameters — no conversion needed).",
                    grid_spacing
                )
            } else {
                " Tip: use grid=true to overlay coordinate labels for precise element location. \
                 Use list_monitors to see all displays and window positions."
                    .to_string()
            };
            format!(
                "\nScreen coords: image origin at ({},{}) in physical screen coordinates. \
                 Coordinates are physical pixels (same system as uia click/drag_drop). \
                 Use list_monitors to discover monitor layout and window positions.{}",
                origin_x, origin_y, grid_note,
            )
        };
        #[cfg(not(target_os = "windows"))]
        let coord_note = String::new();

        let image_data = if media_type == "image/png" {
            ImageData::png(b64)
        } else {
            ImageData::jpeg(b64)
        };

        Ok(ToolResult::ok(format!(
            "Screenshot: {}x{} px, {} KB ({}){}",
            width, height, size_kb, media_type, coord_note
        ))
        .with_image(image_data))
    }

    /// Draw a coordinate grid on an RGBA image.
    /// Grid lines are semi-transparent white/black. Labels show absolute screen coords.
    fn draw_coordinate_grid(
        &self,
        img: &mut image::RgbaImage,
        origin_x: i32,
        origin_y: i32,
        spacing: u32,
    ) {
        let (w, h) = img.dimensions();

        // Find the first grid line >= 0 in image space
        let first_x = if origin_x <= 0 {
            ((-origin_x) as u32 / spacing) * spacing
        } else {
            let rem = origin_x as u32 % spacing;
            if rem == 0 {
                0
            } else {
                spacing - rem
            }
        };
        let first_y = if origin_y <= 0 {
            ((-origin_y) as u32 / spacing) * spacing
        } else {
            let rem = origin_y as u32 % spacing;
            if rem == 0 {
                0
            } else {
                spacing - rem
            }
        };

        // Draw vertical lines
        let mut ix = first_x;
        while ix < w {
            for iy in 0..h {
                blend_pixel(img, ix, iy, [255, 255, 255, 60]);
                if ix > 0 {
                    blend_pixel(img, ix - 1, iy, [0, 0, 0, 30]);
                }
            }
            ix += spacing;
        }

        // Draw horizontal lines
        let mut iy = first_y;
        while iy < h {
            for ix2 in 0..w {
                blend_pixel(img, ix2, iy, [255, 255, 255, 60]);
                if iy > 0 {
                    blend_pixel(img, ix2, iy - 1, [0, 0, 0, 30]);
                }
            }
            iy += spacing;
        }

        // Draw coordinate labels at grid intersections
        let mut lx = first_x;
        while lx < w {
            let mut ly = first_y;
            while ly < h {
                let screen_x = origin_x + lx as i32;
                let screen_y = origin_y + ly as i32;
                let label = format!("{},{}", screen_x, screen_y);
                draw_label(img, lx + 2, ly + 2, &label);
                ly += spacing;
            }
            lx += spacing;
        }
    }
}

/// Alpha-blend a color onto a pixel (src-over).
fn blend_pixel(img: &mut image::RgbaImage, x: u32, y: u32, src: [u8; 4]) {
    if x >= img.width() || y >= img.height() {
        return;
    }
    let dst = img.get_pixel(x, y);
    let a = src[3] as u32;
    let ia = 255 - a;
    let r = (src[0] as u32 * a + dst[0] as u32 * ia) / 255;
    let g = (src[1] as u32 * a + dst[1] as u32 * ia) / 255;
    let b = (src[2] as u32 * a + dst[2] as u32 * ia) / 255;
    img.put_pixel(x, y, image::Rgba([r as u8, g as u8, b as u8, 255]));
}

/// Draw a coordinate label using a scaled-up bitmap font.
/// Scale=4 means each pixel becomes a 4×4 block → 20×28 px per char, readable after LLM compression.
fn draw_label(img: &mut image::RgbaImage, x: u32, y: u32, text: &str) {
    const SCALE: u32 = 4;
    let char_w = 5 * SCALE + SCALE; // 5 cols + 1 gap
    let char_h = 7 * SCALE + SCALE; // 7 rows + 1 pad
    let pad = SCALE;
    let text_w = text.len() as u32 * char_w + pad * 2;
    let text_h = char_h + pad * 2;
    // Dark semi-transparent background
    for dy in 0..text_h {
        for dx in 0..text_w {
            blend_pixel(img, x + dx, y + dy, [0, 0, 0, 200]);
        }
    }
    // Draw each character
    for (i, ch) in text.chars().enumerate() {
        let cx = x + pad + i as u32 * char_w;
        let cy = y + pad;
        draw_char_scaled(img, cx, cy, ch, [255, 255, 0, 255], SCALE);
    }
}

/// Minimal 5×7 bitmap font for digits, comma, minus sign — rendered at SCALE×SCALE blocks.
fn draw_char_scaled(
    img: &mut image::RgbaImage,
    x: u32,
    y: u32,
    ch: char,
    color: [u8; 4],
    scale: u32,
) {
    let bitmap: u64 = match ch {
        '0' => 0b_01110_10001_10011_10101_11001_10001_01110,
        '1' => 0b_00100_01100_00100_00100_00100_00100_01110,
        '2' => 0b_01110_10001_00001_00010_00100_01000_11111,
        '3' => 0b_11111_00010_00100_00010_00001_10001_01110,
        '4' => 0b_00010_00110_01010_10010_11111_00010_00010,
        '5' => 0b_11111_10000_11110_00001_00001_10001_01110,
        '6' => 0b_00110_01000_10000_11110_10001_10001_01110,
        '7' => 0b_11111_00001_00010_00100_01000_01000_01000,
        '8' => 0b_01110_10001_10001_01110_10001_10001_01110,
        '9' => 0b_01110_10001_10001_01111_00001_00010_01100,
        ',' => 0b_00000_00000_00000_00000_00110_00110_00100,
        '-' => 0b_00000_00000_00000_11111_00000_00000_00000,
        ' ' => 0,
        _ => 0b_01110_10001_10001_11111_10001_10001_10001,
    };
    for row in 0..7u32 {
        for col in 0..5u32 {
            let bit_pos = 34 - (row * 5 + col);
            if (bitmap >> bit_pos) & 1 == 1 {
                // Fill a scale×scale block for each lit pixel
                for dy in 0..scale {
                    for dx in 0..scale {
                        blend_pixel(img, x + col * scale + dx, y + row * scale + dy, color);
                    }
                }
            }
        }
    }
}
