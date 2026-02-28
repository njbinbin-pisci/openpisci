/// Screen capture tool with Vision AI support (Windows only)
/// Supports full screen, specific window, region, and multi-monitor capture.
use crate::agent::tool::{ImageData, Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use serde_json::{json, Value};

pub struct ScreenTool;

#[async_trait]
impl Tool for ScreenTool {
    fn name(&self) -> &str { "screen_capture" }

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
                }
            },
            "required": ["action"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let action = match input["action"].as_str() {
            Some(a) => a,
            None => return Ok(ToolResult::err("Missing required parameter: action")),
        };

        match action {
            "list_monitors"  => self.list_monitors(),
            "capture"        => self.capture_full(&input),
            "capture_window" => self.capture_window(&input),
            "capture_region" => self.capture_region(&input),
            "find_element"   => self.capture_full(&input), // capture + return image for Vision AI
            _ => Ok(ToolResult::err(format!("Unknown action: {}", action))),
        }
    }
}

impl ScreenTool {
    // ─── Monitor enumeration ─────────────────────────────────────────────────

    fn list_monitors(&self) -> Result<ToolResult> {
        use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, MONITORINFOEXW};
        use windows::Win32::Foundation::{BOOL, LPARAM, RECT};

        struct MonitorInfo {
            monitors: Vec<String>,
        }

        unsafe extern "system" fn enum_proc(
            hmonitor: windows::Win32::Graphics::Gdi::HMONITOR,
            _hdc: windows::Win32::Graphics::Gdi::HDC,
            _lprect: *mut RECT,
            lparam: LPARAM,
        ) -> BOOL {
            let list = &mut *(lparam.0 as *mut Vec<String>);
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(hmonitor, &mut info.monitorInfo as *mut _ as *mut _).as_bool() {
                let r = info.monitorInfo.rcMonitor;
                let primary = if info.monitorInfo.dwFlags & 1 != 0 { " [PRIMARY]" } else { "" };
                list.push(format!(
                    "Monitor {}: {}x{} at ({},{}){}",
                    list.len(),
                    r.right - r.left,
                    r.bottom - r.top,
                    r.left, r.top,
                    primary
                ));
            }
            BOOL(1)
        }

        let mut monitors: Vec<String> = Vec::new();
        unsafe {
            EnumDisplayMonitors(
                None,
                None,
                Some(enum_proc),
                LPARAM(&mut monitors as *mut Vec<String> as isize),
            );
        }

        Ok(ToolResult::ok(format!(
            "Found {} monitor(s):\n{}",
            monitors.len(),
            monitors.join("\n")
        )))
    }

    // ─── Full screen capture ─────────────────────────────────────────────────

    pub(crate) fn capture_full(&self, input: &Value) -> Result<ToolResult> {
        use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC};
        use windows::Win32::UI::WindowsAndMessaging::{GetDesktopWindow, GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

        unsafe {
            let hwnd = GetDesktopWindow();
            let hdc = GetDC(hwnd);
            let width = GetSystemMetrics(SM_CXSCREEN);
            let height = GetSystemMetrics(SM_CYSCREEN);

            let pixels = self.capture_dc_region(hdc, 0, 0, width, height)?;
            ReleaseDC(hwnd, hdc);

            self.encode_and_return(&pixels, width as u32, height as u32, input)
        }
    }

    // ─── Window capture ───────────────────────────────────────────────────────

    fn capture_window(&self, input: &Value) -> Result<ToolResult> {
        use windows::Win32::UI::WindowsAndMessaging::{
            FindWindowW, EnumWindows, GetWindowTextW, IsWindowVisible, GetWindowRect,
        };
        use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
        use windows::core::PCWSTR;

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
            struct SearchData { title: String, hwnd: HWND }
            unsafe extern "system" fn enum_proc(h: HWND, lparam: LPARAM) -> BOOL {
                let data = &mut *(lparam.0 as *mut SearchData);
                if !IsWindowVisible(h).as_bool() { return BOOL(1); }
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
            let mut search = SearchData { title: title.to_string(), hwnd: HWND(std::ptr::null_mut()) };
            unsafe {
                let _ = EnumWindows(Some(enum_proc), LPARAM(&mut search as *mut SearchData as isize));
            }
            if search.hwnd.0.is_null() {
                return Ok(ToolResult::err(format!("Window '{}' not found", title)));
            }
            search.hwnd
        };

        // Get window rect and capture
        let mut rect = unsafe { std::mem::zeroed::<windows::Win32::Foundation::RECT>() };
        unsafe { GetWindowRect(hwnd, &mut rect).map_err(|e| anyhow::anyhow!("{}", e))?; }

        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        if w <= 0 || h <= 0 {
            return Ok(ToolResult::err("Window has zero size"));
        }

        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
            SelectObject, GetDC, ReleaseDC, BitBlt, SRCCOPY,
        };

        unsafe {
            let hdc_win = GetDC(hwnd);
            let mem_dc = CreateCompatibleDC(hdc_win);
            let bitmap = CreateCompatibleBitmap(hdc_win, w, h);
            let old_bmp = SelectObject(mem_dc, bitmap);

            BitBlt(mem_dc, 0, 0, w, h, hdc_win, 0, 0, SRCCOPY)?;

            let pixels = self.read_bitmap_pixels(mem_dc, bitmap, w, h)?;

            SelectObject(mem_dc, old_bmp);
            DeleteObject(bitmap);
            DeleteDC(mem_dc);
            ReleaseDC(hwnd, hdc_win);

            self.encode_and_return(&pixels, w as u32, h as u32, input)
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
            self.encode_and_return(&pixels, w as u32, h as u32, input)
        }
    }

    // ─── Internal helpers ─────────────────────────────────────────────────────

    unsafe fn capture_dc_region(
        &self,
        hdc: windows::Win32::Graphics::Gdi::HDC,
        x: i32, y: i32, width: i32, height: i32,
    ) -> Result<Vec<u8>> {
        use windows::Win32::Graphics::Gdi::{
            BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, SelectObject, SRCCOPY,
        };

        let mem_dc = CreateCompatibleDC(hdc);
        let bitmap = CreateCompatibleBitmap(hdc, width, height);
        let old_bmp = SelectObject(mem_dc, bitmap);

        BitBlt(mem_dc, 0, 0, width, height, hdc, x, y, SRCCOPY)?;

        let pixels = self.read_bitmap_pixels(mem_dc, bitmap, width, height)?;

        SelectObject(mem_dc, old_bmp);
        DeleteObject(bitmap);
        DeleteDC(mem_dc);

        Ok(pixels)
    }

    unsafe fn read_bitmap_pixels(
        &self,
        mem_dc: windows::Win32::Graphics::Gdi::HDC,
        bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
        width: i32,
        height: i32,
    ) -> Result<Vec<u8>> {
        use windows::Win32::Graphics::Gdi::{GetDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};

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

    fn encode_and_return(&self, rgba: &[u8], width: u32, height: u32, input: &Value) -> Result<ToolResult> {
        let format = input["format"].as_str().unwrap_or("jpeg");
        let quality = input["quality"].as_u64().unwrap_or(75) as u8;

        let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())
            .ok_or_else(|| anyhow::anyhow!("Failed to create image from pixel data"))?;

        let (encoded, media_type) = match format {
            "png" => {
                use image::ImageEncoder;
                let mut buf = Vec::new();
                let encoder = image::codecs::png::PngEncoder::new(&mut buf);
                encoder.write_image(img.as_raw(), width, height, image::ColorType::Rgba8.into())?;
                (buf, "image/png")
            }
            _ => {
                // JPEG (smaller, better for Vision API token usage)
                let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
                let mut buf = Vec::new();
                let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                    &mut buf,
                    quality,
                );
                use image::ImageEncoder;
                encoder.write_image(rgb.as_raw(), width, height, image::ColorType::Rgb8.into())?;
                (buf, "image/jpeg")
            }
        };

        let b64 = base64::engine::general_purpose::STANDARD.encode(&encoded);
        let size_kb = encoded.len() / 1024;

        let image_data = if media_type == "image/png" {
            ImageData::png(b64)
        } else {
            ImageData::jpeg(b64)
        };

        Ok(ToolResult::ok(format!(
            "Screenshot captured: {}x{} pixels, {} KB ({})",
            width, height, size_kb, media_type
        )).with_image(image_data))
    }
}
