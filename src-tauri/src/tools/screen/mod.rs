use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
/// Screen capture tool with Vision AI support (cross-platform)
/// Supports full screen, specific window, region, and multi-monitor capture.
use pisci_kernel::agent::tool::{ImageData, Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::fs;

pub struct ScreenTool;

#[async_trait]
impl Tool for ScreenTool {
    fn name(&self) -> &str {
        "screen_capture"
    }

    fn description(&self) -> &str {
        "Capture a screenshot of the full screen, a specific window, or a screen region. \
         Returns a base64-encoded image that Vision AI can analyze. \
         \
         Cross-platform UI automation workflow: \
            1. Take a screenshot with grid=true — this overlays a 100x100 pixel grid with coordinate labels \
                and marks the current mouse position with a crosshair \
         2. The Vision model identifies the target UI element in the image \
         3. Use desktop_automation.click(x, y) with the exact coordinates from the grid \
         \
         Use 'list_monitors' to discover available displays and their pixel extents. \
         Use 'find_element' to capture and ask Vision AI to locate a described element. \
            Tip: if the screenshot shows partial content, progress bars, skeleton loaders, or other obvious loading signals, treat that as a wait state: recapture using real elapsed time with exponential backoff (for example 1s, 2s, 4s, 8s, capped) before acting. \
            Tip: grid_spacing=50 gives even finer labels (default 100)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["capture", "capture_window", "capture_region", "list_monitors", "find_element", "screen_analyze"],
                    "description": "capture: full screen; capture_window: specific window by title; capture_region: x/y/width/height; list_monitors: enumerate displays; find_element: capture + return image for Vision AI; screen_analyze: capture and ask Vision AI what it sees (optionally guided by element_description)"
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
                    "description": "Overlay a coordinate grid on the screenshot. Grid lines default to every 100px with coordinate labels. Labels show absolute screen coordinates that can be used directly with desktop_automation click/drag/hotkey. The current mouse position is also marked with a crosshair. Essential for Vision AI to precisely locate and interact with UI elements."
                },
                "grid_spacing": {
                    "type": "integer",
                    "description": "Grid line spacing in pixels (default: 100)"
                },
                "output_path": {
                    "type": "string",
                    "description": "Optional absolute file path to persist the screenshot to disk (e.g. '/tmp/shot.png' or project workspace). The file is written BEFORE the tool returns, so the caller can immediately reference it via `app_control(action=\"artifact_submit\", path=output_path, artifact_type=\"image\")`. If omitted, no file is written and the image is returned only as base64."
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
            "list_monitors" => platform::list_monitors().await,
            "capture" => platform::capture_full(&input).await,
            "capture_window" => platform::capture_window(&input).await,
            "capture_region" => platform::capture_region(&input).await,
            "find_element" => platform::capture_full(&input).await, // capture + return image for Vision AI
            "screen_analyze" => platform::capture_full(&input).await, // capture + return image for Vision AI (guided optionally by element_description)
            _ => Ok(ToolResult::err(format!("Unknown action: {}", action))),
        }
    }
}

// ─── Platform-specific implementations ─────────────────────────────────────

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as platform;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

// ─── Shared encoding / grid helpers ────────────────────────────────────────

pub(crate) fn encode_and_return_with_cursor_offset(
    rgba: &[u8],
    width: u32,
    height: u32,
    input: &Value,
    origin_x: i32,
    origin_y: i32,
    cursor_pos: Option<(i32, i32)>,
) -> Result<ToolResult> {
    let format = input["format"].as_str().unwrap_or("jpeg");
    let quality = input["quality"].as_u64().unwrap_or(75) as u8;
    let draw_grid = input["grid"].as_bool().unwrap_or(false);
    let grid_spacing = input["grid_spacing"].as_u64().unwrap_or(100).max(50) as u32;

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
        draw_coordinate_grid(&mut img, origin_x, origin_y, grid_spacing);
        if let Some((cursor_x, cursor_y)) = cursor_pos {
            draw_cursor_crosshair(&mut img, origin_x, origin_y, cursor_x, cursor_y);
        }
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

    let coord_note = {
        #[cfg(target_os = "windows")]
        {
            let target_tool = "uia";
            let grid_note = if draw_grid {
                let cursor_note = cursor_pos
                    .map(|(cx, cy)| {
                        format!(
                            "\nCrosshair: the mouse pointer was at absolute screen coordinates ({},{}) when the screenshot was taken.",
                            cx, cy
                        )
                    })
                    .unwrap_or_default();
                format!(
                    "\n## Coordinate System (grid=true, spacing={}px)\n\
                    Image size: {}x{} px\n\
                    Origin (top-left): ({}, {}) in screen coordinates\n\
                    Grid lines and cursor crosshair are drawn as high-contrast XOR-style overlays to preserve underlying content.\n\
                    Coordinate labels are edge-aligned to reduce occlusion; labels are absolute screen pixels.\n\
                    To click an element: 1) visually identify it using grid labels 2) call {target_tool}.click(x, y) with the exact pixel from the label\n\
                    Tip: if the screenshot shows incomplete content, loading spinners, or progress bars, treat that as a wait state and recapture using real elapsed time with exponential backoff before acting.\n\
                    Tip: for drag operations, use {target_tool}.drag(x, y, to_x, to_y){}",
                    grid_spacing, width, height, origin_x, origin_y, cursor_note
                )
            } else {
                String::new()
            };
            if grid_note.is_empty() {
                format!(
                    "\nImage: {}x{} px at ({},{}). Tip: use grid=true to overlay coordinate labels for precise element location. Use list_monitors to see all displays.",
                    width, height, origin_x, origin_y
                )
            } else {
                format!(
                    "Screenshot: {}x{} px, {} KB ({})\nImage origin: ({},{}) in screen coordinates{}",
                    width, height, size_kb, media_type, origin_x, origin_y, grid_note
                )
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let target_tool = "desktop_automation";
            let grid_note = if draw_grid {
                let cursor_note = cursor_pos
                    .map(|(cx, cy)| {
                        format!(
                            "\nCrosshair: the mouse pointer was at absolute screen coordinates ({},{}) when the screenshot was taken.",
                            cx, cy
                        )
                    })
                    .unwrap_or_default();
                format!(
                    "\n## Coordinate System (grid=true, spacing={grid_sp}x)\n\
                    Image: {w}x{h} px | Origin: ({ox},{oy}) screen coords\n\
                    Grid lines and cursor crosshair are drawn as high-contrast XOR-style overlays to preserve underlying content.\n\
                    Coordinate labels are edge-aligned to reduce occlusion; labels are absolute screen pixels.\n\
                    → Find element via grid labels, then: {tt}.click(x, y) | {tt}.drag(x, y, to_x, to_y) | {tt}.hotkey(keys)\n\
                    Tip: if the screenshot shows incomplete content, loading spinners, or progress bars, treat that as a wait state and recapture using real elapsed time with exponential backoff before acting.{cursor_note}",
                    grid_sp = grid_spacing, w = width, h = height, ox = origin_x, oy = origin_y, tt = target_tool, cursor_note = cursor_note
                )
            } else {
                String::new()
            };
            if grid_note.is_empty() {
                format!(
                    "\nImage: {}x{} px at ({},{}). Tip: use grid=true for coordinate labels, then use desktop_automation.click(x,y).",
                    width, height, origin_x, origin_y
                )
            } else {
                format!(
                    "Screenshot: {}x{} px, {} KB ({})\nOrigin: ({},{}){}",
                    width, height, size_kb, media_type, origin_x, origin_y, grid_note
                )
            }
        }
    };

    let image_data = if media_type == "image/png" {
        ImageData::png(b64.clone())
    } else {
        ImageData::jpeg(b64.clone())
    };

    // ── Persist-to-disk path (opt-in via output_path) ──────────────────
    // Without this, the screenshot lives only in the LLM's tool result as
    // a base64 blob — there is no file to reference as an artifact. This
    // branch decodes the base64 and writes it to the requested path so
    // the caller can immediately submit it with
    // `app_control(action="artifact_submit", path=<output_path>, ...)`. 
    let save_note = if let Some(out_path) = input["output_path"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        let path = std::path::Path::new(out_path);
        let (ok, message) = match fs::create_dir_all(path.parent().unwrap_or_else(|| std::path::Path::new("."))) {
            Err(e) => (false, format!("could not create parent directory: {e}")),
            Ok(()) => match fs::write(path, &encoded) {
                Ok(()) => (true, out_path.to_string()),
                Err(e) => (false, format!("write failed: {e}")),
            },
        };
        if ok {
            format!("\nSaved to disk: {message} ({} KB). You MUST now call `app_control(action=\"artifact_submit\", artifact_name=\"<short label>\", path=\"{message}\", artifact_type=\"image\", content_summary=\"<1-line description>\")` so the file appears in the user's Artifacts panel.", size_kb)
        } else {
            format!("\nWARNING: failed to save screenshot to '{}': {}. The base64 image is still returned inline.", out_path, message)
        }
    } else {
        String::new()
    };

    Ok(ToolResult::ok(format!(
        "Screenshot: {}x{} px, {} KB ({}){}{}",
        width, height, size_kb, media_type, coord_note, save_note
    ))
    .with_image(image_data))
}

/// Draw a coordinate grid on an RGBA image.
/// Grid lines are semi-transparent white/black. Labels show absolute screen coords.
fn draw_coordinate_grid(img: &mut image::RgbaImage, origin_x: i32, origin_y: i32, spacing: u32) {
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
            xor_pixel(img, ix, iy);
            if ix + 1 < w && iy % 2 == 0 {
                xor_pixel(img, ix + 1, iy);
            }
        }
        ix += spacing;
    }

    // Draw horizontal lines
    let mut iy = first_y;
    while iy < h {
        for ix2 in 0..w {
            xor_pixel(img, ix2, iy);
            if iy + 1 < h && ix2 % 2 == 0 {
                xor_pixel(img, ix2, iy + 1);
            }
        }
        iy += spacing;
    }

    // Draw coordinate labels on the image edges instead of every intersection.
    // This keeps coordinates readable while reducing occlusion of page/app content.
    let label_interval = if spacing < 200 { 2 } else { 1 };
    let label_margin = 2;

    if first_x < w && first_y < h {
        let corner_label = format!(
            "{},{}",
            origin_x + first_x as i32,
            origin_y + first_y as i32
        );
        draw_label(img, label_margin, label_margin, &corner_label);
    }

    let mut lx = first_x;
    let mut xi = 0usize;
    while lx < w {
        if xi != 0 && xi % label_interval == 0 {
            let screen_x = origin_x + lx as i32;
            draw_label(
                img,
                lx.saturating_add(label_margin),
                label_margin,
                &screen_x.to_string(),
            );
        }
        lx += spacing;
        xi += 1;
    }

    let mut ly = first_y;
    let mut yi = 0usize;
    while ly < h {
        if yi != 0 && yi % label_interval == 0 {
            let screen_y = origin_y + ly as i32;
            draw_label(
                img,
                label_margin,
                ly.saturating_add(label_margin),
                &screen_y.to_string(),
            );
        }
        ly += spacing;
        yi += 1;
    }
}

fn draw_cursor_crosshair(
    img: &mut image::RgbaImage,
    origin_x: i32,
    origin_y: i32,
    cursor_x: i32,
    cursor_y: i32,
) {
    let local_x = cursor_x - origin_x;
    let local_y = cursor_y - origin_y;
    if local_x < 0 || local_y < 0 {
        return;
    }
    let x = local_x as u32;
    let y = local_y as u32;
    if x >= img.width() || y >= img.height() {
        return;
    }

    for ix in 0..img.width() {
        xor_pixel(img, ix, y);
    }
    for iy in 0..img.height() {
        xor_pixel(img, x, iy);
    }

    let radius = 10u32;
    let left = x.saturating_sub(radius);
    let right = (x + radius).min(img.width().saturating_sub(1));
    let top = y.saturating_sub(radius);
    let bottom = (y + radius).min(img.height().saturating_sub(1));
    for ix in left..=right {
        xor_pixel(img, ix, top);
        xor_pixel(img, ix, bottom);
    }
    for iy in top..=bottom {
        xor_pixel(img, left, iy);
        xor_pixel(img, right, iy);
    }

    let label_x = (x + 12).min(img.width().saturating_sub(1));
    let label_y = y.saturating_sub(28);
    draw_label(img, label_x, label_y, &format!("{},{}", cursor_x, cursor_y));
}

fn xor_pixel(img: &mut image::RgbaImage, x: u32, y: u32) {
    if x >= img.width() || y >= img.height() {
        return;
    }
    let dst = *img.get_pixel(x, y);
    img.put_pixel(
        x,
        y,
        image::Rgba([
            255u8.wrapping_sub(dst[0]),
            255u8.wrapping_sub(dst[1]),
            255u8.wrapping_sub(dst[2]),
            255,
        ]),
    );
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
/// Scale=4 means each pixel becomes a 4x4 block -> 20x28 px per char, readable after LLM compression.
fn draw_label(img: &mut image::RgbaImage, x: u32, y: u32, text: &str) {
    const SCALE: u32 = 4;
    let char_w = 5 * SCALE + SCALE; // 5 cols + 1 gap
    let char_h = 7 * SCALE + SCALE; // 7 rows + 1 pad
    let pad = SCALE;
    let text_w = text.len() as u32 * char_w + pad * 2;
    let text_h = char_h + pad * 2;
    // Dark semi-transparent background with lower opacity so labels stay readable
    // without covering as much underlying content.
    for dy in 0..text_h {
        for dx in 0..text_w {
            blend_pixel(img, x + dx, y + dy, [0, 0, 0, 144]);
        }
    }
    // Draw each character
    for (i, ch) in text.chars().enumerate() {
        let cx = x + pad + i as u32 * char_w;
        let cy = y + pad;
        draw_char_scaled(img, cx, cy, ch, [255, 255, 0, 255], SCALE);
    }
}

/// Minimal 5x7 bitmap font for digits, comma, minus sign — rendered at SCALExSCALE blocks.
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
                // Fill a scale x scale block for each lit pixel
                for dy in 0..scale {
                    for dx in 0..scale {
                        blend_pixel(img, x + col * scale + dx, y + row * scale + dy, color);
                    }
                }
            }
        }
    }
}
