use anyhow::Result;
use pisci_kernel::agent::tool::ToolResult;
use serde_json::Value;
use xcap::{Monitor, Window};

pub async fn list_monitors() -> Result<ToolResult> {
    let monitors = Monitor::all().map_err(|e| anyhow::anyhow!("{}", e))?;
    let windows = Window::all().unwrap_or_default();

    let mut lines = Vec::new();
    for (i, monitor) in monitors.iter().enumerate() {
        let name = monitor.name().unwrap_or_default();
        let primary_tag = if monitor.is_primary().unwrap_or(false) {
            " [PRIMARY]"
        } else {
            ""
        };
        lines.push(format!(
            "Monitor {} (index={}): {}x{} at ({},{}) {}{}",
            i,
            i,
            monitor.width().unwrap_or(0),
            monitor.height().unwrap_or(0),
            monitor.x().unwrap_or(0),
            monitor.y().unwrap_or(0),
            name,
            primary_tag
        ));
    }

    // List visible windows
    let mut win_lines = Vec::new();
    for win in windows {
        if win.is_minimized().unwrap_or(true) {
            continue;
        }
        let title = win.title().unwrap_or_default();
        let x = win.x().unwrap_or(0);
        let y = win.y().unwrap_or(0);
        let w = win.width().unwrap_or(0);
        let h = win.height().unwrap_or(0);
        if !title.is_empty() && w > 0 && h > 0 {
            win_lines.push(format!(
                "    - \"{}\" at ({},{})-({},{})",
                title,
                x,
                y,
                x + w as i32,
                y + h as i32
            ));
        }
    }
    if !win_lines.is_empty() {
        lines.push("  Visible windows:".to_string());
        lines.extend(win_lines);
    }

    Ok(ToolResult::ok(format!(
        "Found {} monitor(s). Use monitor_index=N with action=capture to screenshot a specific display.\n\n{}",
        monitors.len(),
        lines.join("\n")
    )))
}

pub async fn capture_full(input: &Value) -> Result<ToolResult> {
    let monitor_index = input["monitor_index"].as_u64().unwrap_or(0) as usize;
    let monitors = Monitor::all().map_err(|e| anyhow::anyhow!("{}", e))?;

    let monitor = monitors
        .get(monitor_index)
        .or_else(|| monitors.first())
        .ok_or_else(|| anyhow::anyhow!("No displays found"))?;

    let image = monitor
        .capture_image()
        .map_err(|e| anyhow::anyhow!("Screenshot failed: {}", e))?;
    let (width, height) = image.dimensions();
    let rgba = image.into_raw();

    let x = monitor.x().unwrap_or(0);
    let y = monitor.y().unwrap_or(0);

    super::encode_and_return_with_offset(&rgba, width, height, input, x, y)
}

pub async fn capture_window(input: &Value) -> Result<ToolResult> {
    let title = match input["window_title"].as_str() {
        Some(t) => t,
        None => return Ok(ToolResult::err("capture_window requires window_title")),
    };

    let windows = Window::all().map_err(|e| anyhow::anyhow!("{}", e))?;

    // Try exact match first
    let win = windows
        .iter()
        .find(|w| w.title().unwrap_or_default().eq_ignore_ascii_case(title))
        .or_else(|| {
            // Partial match
            windows.iter().find(|w| {
                w.title()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&title.to_lowercase())
            })
        });

    let win = match win {
        Some(w) => w,
        None => return Ok(ToolResult::err(format!("Window '{}' not found", title))),
    };

    let image = win
        .capture_image()
        .map_err(|e| anyhow::anyhow!("Screenshot failed: {}", e))?;
    let (width, height) = image.dimensions();
    let rgba = image.into_raw();

    let x = win.x().unwrap_or(0);
    let y = win.y().unwrap_or(0);

    super::encode_and_return_with_offset(&rgba, width, height, input, x, y)
}

pub async fn capture_region(input: &Value) -> Result<ToolResult> {
    let x = input["x"].as_i64().unwrap_or(0) as i32;
    let y = input["y"].as_i64().unwrap_or(0) as i32;
    let w = match input["width"].as_i64() {
        Some(v) if v > 0 => v as u32,
        _ => return Ok(ToolResult::err("capture_region requires width > 0")),
    };
    let h = match input["height"].as_i64() {
        Some(v) if v > 0 => v as u32,
        _ => return Ok(ToolResult::err("capture_region requires height > 0")),
    };

    let monitor = Monitor::from_point(x, y).or_else(|_| {
        Monitor::all()
            .map(|m| m.into_iter().next().ok_or_else(|| anyhow::anyhow!("No displays")))?
    })?;

    let rel_x = (x - monitor.x().unwrap_or(0)).max(0) as u32;
    let rel_y = (y - monitor.y().unwrap_or(0)).max(0) as u32;

    let image = monitor
        .capture_region(rel_x, rel_y, w, h)
        .map_err(|e| anyhow::anyhow!("Screenshot failed: {}", e))?;
    let (width, height) = image.dimensions();
    let rgba = image.into_raw();

    super::encode_and_return_with_offset(&rgba, width, height, input, x, y)
}
