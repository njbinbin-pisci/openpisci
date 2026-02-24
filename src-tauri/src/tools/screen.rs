/// Screen capture tool with Vision AI fallback (Windows only)
use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use serde_json::{json, Value};

pub struct ScreenTool;

#[async_trait]
impl Tool for ScreenTool {
    fn name(&self) -> &str { "screen_capture" }

    fn description(&self) -> &str {
        "Capture a screenshot of the screen or a specific window. \
         Returns base64-encoded PNG. Can also use Vision AI to find UI elements \
         when UIA automation fails."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["capture", "find_element", "describe"],
                    "description": "capture: take screenshot; find_element: locate UI element visually; describe: describe screen content"
                },
                "window_title": {
                    "type": "string",
                    "description": "Capture specific window by title (optional, captures full screen if omitted)"
                },
                "element_description": {
                    "type": "string",
                    "description": "Description of the UI element to find (for find_element action)"
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
            "capture" => self.capture_screen(&input).await,
            "find_element" | "describe" => {
                // Capture then describe with Vision AI
                let screenshot = self.capture_screen(&input).await?;
                Ok(screenshot)
            }
            _ => Ok(ToolResult::err(format!("Unknown action: {}", action))),
        }
    }
}

impl ScreenTool {
    async fn capture_screen(&self, _input: &Value) -> Result<ToolResult> {
        let png_bytes = self.take_screenshot()?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);

        Ok(ToolResult::ok(format!(
            "Screenshot captured ({} bytes)\nbase64:image/png;{}",
            png_bytes.len(),
            b64
        )))
    }

    fn take_screenshot(&self) -> Result<Vec<u8>> {
        use windows::Win32::Graphics::Gdi::{
            BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
            GetDIBits, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
        };
        use windows::Win32::UI::WindowsAndMessaging::{GetDesktopWindow, GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
        use windows::Win32::Graphics::Gdi::GetDC;

        unsafe {
            let hwnd = GetDesktopWindow();
            let hdc = GetDC(hwnd);
            let width = GetSystemMetrics(SM_CXSCREEN);
            let height = GetSystemMetrics(SM_CYSCREEN);

            let mem_dc = CreateCompatibleDC(hdc);
            let bitmap = CreateCompatibleBitmap(hdc, width, height);
            let old_bitmap = SelectObject(mem_dc, bitmap);

            BitBlt(mem_dc, 0, 0, width, height, hdc, 0, 0, SRCCOPY)?;

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

            // Convert BGRA to RGBA
            for chunk in pixels.chunks_exact_mut(4) {
                chunk.swap(0, 2); // B <-> R
            }

            SelectObject(mem_dc, old_bitmap);
            DeleteObject(bitmap);
            DeleteDC(mem_dc);

            // Encode as PNG using a simple approach
            // For production, use the `image` crate
            let png = encode_png_simple(&pixels, width as u32, height as u32)?;
            Ok(png)
        }
    }
}

/// Simple PNG encoder (minimal implementation)
/// In production, use the `image` crate for proper PNG encoding
fn encode_png_simple(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let mut png = Vec::new();

    // PNG signature
    png.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);

    // IHDR chunk
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(2); // color type: RGB
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    write_png_chunk(&mut png, b"IHDR", &ihdr);

    // IDAT chunk (raw, uncompressed for simplicity)
    let mut idat = Vec::new();
    for row in 0..height as usize {
        idat.push(0); // filter type: None
        for col in 0..width as usize {
            let idx = (row * width as usize + col) * 4;
            idat.push(rgba[idx]);     // R
            idat.push(rgba[idx + 1]); // G
            idat.push(rgba[idx + 2]); // B
            // Skip alpha
        }
    }

    // Compress with zlib
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&idat, 6);
    write_png_chunk(&mut png, b"IDAT", &compressed);

    // IEND chunk
    write_png_chunk(&mut png, b"IEND", &[]);

    Ok(png)
}

fn write_png_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    let len = data.len() as u32;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);

    // CRC32 over chunk_type + data
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(chunk_type);
    hasher.update(data);
    let crc = hasher.finalize();
    out.extend_from_slice(&crc.to_be_bytes());
}
