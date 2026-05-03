use anyhow::Result;
use async_trait::async_trait;
/// Cross-platform desktop automation — clicks, typing, window management.
///
/// Linux: xdotool + wmctrl
/// macOS: osascript + cliclick
/// Windows: shell-out to PowerShell (uia tool is the primary but this works as fallback)
use pisci_kernel::agent::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

pub struct DesktopAutomationTool;

#[async_trait]
impl Tool for DesktopAutomationTool {
    fn name(&self) -> &str {
        "desktop_automation"
    }

    fn description(&self) -> &str {
        "Cross-platform desktop automation: mouse clicks, keyboard input, window management. \
         Coordinates are in physical screen pixels. \
         To discover element coordinates: use screen_capture with grid=true, then visually identify the target. \
         click(x,y): click at pixel (used after screen_capture grid) \
         double_click(x,y) / right_click(x,y): extended click actions \
         drag(x,y,to_x,to_y): drag from start to target \
         type_text(text): keyboard input at current focus \
         hotkey(keys): key combo (ctrl+c, alt+f4, etc.) \
         list_windows / activate_window(title): window management \
         launch_app(name): open application by name or path"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "click", "double_click", "right_click",
                        "drag", "move_mouse", "get_cursor_position",
                        "type_text", "hotkey",
                        "list_windows", "activate_window",
                        "scroll", "launch_app"
                    ],
                    "description": "click/double_click/right_click: at (x,y); drag: from (x,y) to (to_x,to_y); move_mouse: to (x,y); type_text: input text; hotkey: key combo like ctrl+c; list_windows: enumerate visible windows; activate_window: bring window to front by title; scroll: scroll at position; launch_app: open app by name"
                },
                "x": {
                    "type": "integer",
                    "description": "X coordinate for click/double_click/right_click/move_mouse/scroll, or start X for drag"
                },
                "y": {
                    "type": "integer",
                    "description": "Y coordinate for click/double_click/right_click/move_mouse/scroll, or start Y for drag"
                },
                "to_x": {
                    "type": "integer",
                    "description": "Target X coordinate for drag"
                },
                "to_y": {
                    "type": "integer",
                    "description": "Target Y coordinate for drag"
                },
                "text": {
                    "type": "string",
                    "description": "Text to type (for type_text action)"
                },
                "keys": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Key combination for hotkey, e.g. [\"ctrl\", \"c\"] for Ctrl+C, [\"alt\", \"f4\"] for Alt+F4"
                },
                "window_title": {
                    "type": "string",
                    "description": "Window title (partial match) for activate_window"
                },
                "app_name": {
                    "type": "string",
                    "description": "App name for launch_app (e.g. 'firefox', 'terminal', 'calculator')"
                },
                "scroll_direction": {
                    "type": "string",
                    "enum": ["up", "down", "left", "right"],
                    "description": "Scroll direction (default: down)"
                },
                "scroll_amount": {
                    "type": "integer",
                    "description": "Scroll amount in lines/clicks (default: 3)"
                }
            },
            "required": ["action"]
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let action = match input["action"].as_str() {
            Some(a) => a,
            None => return Ok(ToolResult::err("Missing required parameter: action")),
        };

        match action {
            "click" => platform_click(&input, 1).await,
            "double_click" => platform_click(&input, 2).await,
            "right_click" => platform_click(&input, 3).await,
            "drag" => platform_drag(&input).await,
            "move_mouse" => platform_move_mouse(&input).await,
            "get_cursor_position" => platform_get_cursor_position().await,
            "type_text" => platform_type_text(&input).await,
            "hotkey" => platform_hotkey(&input).await,
            "list_windows" => platform_list_windows().await,
            "activate_window" => platform_activate_window(&input).await,
            "scroll" => platform_scroll(&input).await,
            "launch_app" => platform_launch_app(&input).await,
            _ => Ok(ToolResult::err(format!("Unknown action: {}", action))),
        }
    }
}

// ─── Platform implementations ─────────────────────────────────────────────────

use tokio::process::Command;

// ── Common helpers ────────────────────────────────────────────────────────────

fn require_coords(input: &Value) -> anyhow::Result<(i32, i32)> {
    let x = match input["x"].as_i64() {
        Some(v) => v as i32,
        None => anyhow::bail!("Missing required parameter: x"),
    };
    let y = match input["y"].as_i64() {
        Some(v) => v as i32,
        None => anyhow::bail!("Missing required parameter: y"),
    };
    Ok((x, y))
}

async fn run_cmd(program: &str, args: &[&str]) -> Result<ToolResult> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to execute {}: {}", program, e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !output.status.success() {
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Ok(ToolResult::err(format!("{} failed: {}", program, detail)));
    }

    Ok(ToolResult::ok(format!("{} succeeded{}", program, if stdout.is_empty() { String::new() } else { format!(": {}", stdout) })))
}

// ──────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    pub async fn click(input: &Value, button: u8) -> Result<ToolResult> {
        let (x, y) = require_coords(input)?;
        let btn = match button {
            1 => "1",
            2 => "1",
            3 => "3",
            _ => "1",
        };
        if button == 2 {
            // Double-click: move then click twice
            let output = Command::new("xdotool")
                .args([
                    "mousemove", "--sync", &x.to_string(), &y.to_string(),
                    "click", "--repeat", "2", &btn,
                ])
                .output()
                .await
                .map_err(|e| anyhow::anyhow!("xdotool double-click failed: {}", e))?;
            if !output.status.success() {
                return Ok(ToolResult::err(format!("xdotool failed: {}", String::from_utf8_lossy(&output.stderr))));
            }
            Ok(ToolResult::ok(format!("Double-click at ({},{})", x, y)))
        } else {
            let output = Command::new("xdotool")
                .args([
                    "mousemove", "--sync", &x.to_string(), &y.to_string(),
                    "click", &btn,
                ])
                .output()
                .await
                .map_err(|e| anyhow::anyhow!("xdotool click failed: {}", e))?;
            if !output.status.success() {
                return Ok(ToolResult::err(format!("xdotool failed: {}", String::from_utf8_lossy(&output.stderr))));
            }
            Ok(ToolResult::ok(format!("Click button {} at ({},{})", btn, x, y)))
        }
    }

    pub async fn drag(input: &Value) -> Result<ToolResult> {
        let (x, y) = require_coords(input)?;
        let to_x = input["to_x"].as_i64().unwrap_or(0) as i32;
        let to_y = input["to_y"].as_i64().unwrap_or(0) as i32;
        run_cmd("xdotool", &[
            "mousemove", "--sync", &x.to_string(), &y.to_string(),
            "mousedown", "1",
            "mousemove", "--sync", &to_x.to_string(), &to_y.to_string(),
            "mouseup", "1",
        ]).await?;
        Ok(ToolResult::ok(format!("Dragged from ({},{}) to ({},{})", x, y, to_x, to_y)))
    }

    pub async fn move_mouse(input: &Value) -> Result<ToolResult> {
        let (x, y) = require_coords(input)?;
        run_cmd("xdotool", &["mousemove", "--sync", &x.to_string(), &y.to_string()]).await?;
        Ok(ToolResult::ok(format!("Mouse moved to ({},{})", x, y)))
    }

    pub async fn get_cursor_position() -> Result<ToolResult> {
        let output = Command::new("xdotool")
            .args(["getmouselocation", "--shell"])
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("xdotool getmouselocation failed: {}", e))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut x = 0i32;
        let mut y = 0i32;
        for line in stdout.lines() {
            if let Some(v) = line.strip_prefix("X=") { x = v.parse().unwrap_or(0); }
            if let Some(v) = line.strip_prefix("Y=") { y = v.parse().unwrap_or(0); }
        }
        Ok(ToolResult::ok(format!("Cursor at ({},{})", x, y)))
    }

    pub async fn type_text(input: &Value) -> Result<ToolResult> {
        let text = match input["text"].as_str() {
            Some(t) => t,
            None => return Ok(ToolResult::err("Missing required parameter: text")),
        };
        // Use clipboard+paste approach for reliable text input (handles CJK/IME)
        // First, copy text to clipboard, then paste
        let mut child = Command::new("xclip")
            .args(["-selection", "clipboard", "-in"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| anyhow::anyhow!("xclip failed: {}", e))?;
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(text.as_bytes()).await;
        }
        let status = child.wait().await.map_err(|e| anyhow::anyhow!("xclip wait: {}", e))?;
        if !status.success() {
            // xclip not available, fall back to xdotool type
            let output = Command::new("xdotool")
                .args(["type", "--", text])
                .output()
                .await
                .map_err(|e| anyhow::anyhow!("xdotool type failed: {}", e))?;
            if !output.status.success() {
                return Ok(ToolResult::err(format!(
                    "type_text failed (neither xclip nor xdotool type worked): {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
        } else {
            // Paste from clipboard
            Command::new("xdotool")
                .args(["key", "ctrl+v"])
                .output()
                .await
                .map_err(|e| anyhow::anyhow!("xdotool paste failed: {}", e))?;
        }
        Ok(ToolResult::ok(format!("Typed text ({} chars)", text.len())))
    }

    pub async fn hotkey(input: &Value) -> Result<ToolResult> {
        let keys: Vec<String> = match input["keys"].as_array() {
            Some(arr) => arr.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            None => return Ok(ToolResult::err("Missing required parameter: keys (array of strings)")),
        };
        if keys.is_empty() {
            return Ok(ToolResult::err("keys array must not be empty"));
        }
        let combo = keys.join("+");
        let mut args: Vec<&str> = vec!["key"];
        let key_strs: Vec<String> = keys.iter().map(|k| k.as_str().to_string()).collect();
        let key_refs: Vec<&str> = key_strs.iter().map(|s| s.as_str()).collect();
        args.extend(&key_refs);
        run_cmd("xdotool", &args).await?;
        Ok(ToolResult::ok(format!("Hotkey '{}' sent", combo)))
    }

    pub async fn list_windows() -> Result<ToolResult> {
        let output = Command::new("wmctrl")
            .args(["-l", "-G"]) // -G for geometry
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("wmctrl failed: {}", e))?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        let mut lines: Vec<String> = Vec::new();
        for line in stdout.lines() {
            // Format: <id> <desktop> <x> <y> <w> <h> <host> <title>
            let parts: Vec<&str> = line.splitn(8, ' ').collect();
            if parts.len() >= 8 {
                let title = parts[7];
                let x = parts[2];
                let y = parts[3];
                let w = parts[4];
                let h = parts[5];
                lines.push(format!("- \"{}\" at ({},{}) size {}x{}", title, x, y, w, h));
            }
        }

        if lines.is_empty() {
            Ok(ToolResult::ok("No visible windows found (wmctrl returned nothing). Try installing wmctrl."))
        } else {
            Ok(ToolResult::ok(format!("Found {} window(s):\n{}", lines.len(), lines.join("\n"))))
        }
    }

    pub async fn activate_window(input: &Value) -> Result<ToolResult> {
        let title = match input["window_title"].as_str() {
            Some(t) => t,
            None => return Ok(ToolResult::err("Missing required parameter: window_title")),
        };
        // Try exact match first, then partial
        let output = Command::new("wmctrl")
            .args(["-a", title])
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("wmctrl activate failed: {}", e))?;
        if output.status.success() {
            Ok(ToolResult::ok(format!("Activated window '{}'", title)))
        } else {
            // Try fuzzy: list windows, find partial match
            let list = Command::new("wmctrl")
                .args(["-l"])
                .output()
                .await
                .map_err(|e| anyhow::anyhow!("wmctrl list failed: {}", e))?;
            let stdout = String::from_utf8_lossy(&list.stdout);
            let title_lower = title.to_lowercase();
            let matched = stdout
                .lines()
                .find(|l| {
                    let parts: Vec<&str> = l.splitn(8, ' ').collect();
                    parts.len() >= 8 && parts[7].to_lowercase().contains(&title_lower)
                })
                .and_then(|l| l.split(' ').next())
                .map(|s| s.to_string());

            if let Some(id) = matched {
                let output = Command::new("wmctrl")
                    .args(["-i", "-a", &id])
                    .output()
                    .await
                    .map_err(|e| anyhow::anyhow!("wmctrl activate by id failed: {}", e))?;
                if output.status.success() {
                    return Ok(ToolResult::ok(format!("Activated window matching '{}'", title)));
                }
            }
            Ok(ToolResult::err(format!("Window '{}' not found or cannot be activated", title)))
        }
    }

    pub async fn scroll(input: &Value) -> Result<ToolResult> {
        let (x, y) = require_coords(input)?;
        let dir = input["scroll_direction"].as_str().unwrap_or("down");
        let amount = input["scroll_amount"].as_u64().unwrap_or(3);
        let btn = match dir {
            "up" => "4",
            "down" => "5",
            "left" => "6",
            "right" => "7",
            _ => "5",
        };
        run_cmd("xdotool", &[
            "mousemove", "--sync", &x.to_string(), &y.to_string(),
            "click", "--repeat", &amount.to_string(), btn,
        ]).await?;
        Ok(ToolResult::ok(format!("Scrolled {} {} at ({},{})", amount, dir, x, y)))
    }

    pub async fn launch_app(input: &Value) -> Result<ToolResult> {
        let app_name = match input["app_name"].as_str() {
            Some(n) => n,
            None => return Ok(ToolResult::err("Missing required parameter: app_name")),
        };

        // Try gtk-launch first (XDG desktop file)
        let gtk_result = Command::new("gtk-launch")
            .arg(app_name)
            .output()
            .await;

        if let Ok(out) = &gtk_result {
            if out.status.success() {
                return Ok(ToolResult::ok(format!("Launched '{}' via gtk-launch", app_name)));
            }
        }

        // Try xdg-open
        let xdg_result = Command::new("xdg-open")
            .arg(app_name)
            .output()
            .await;

        if let Ok(out) = &xdg_result {
            if out.status.success() {
                return Ok(ToolResult::ok(format!("Launched '{}' via xdg-open", app_name)));
            }
        }

        // Try as direct command via sh
        let sh_result = Command::new("sh")
            .args(["-c", &format!("which {} && exec {}", app_name, app_name)])
            .output()
            .await;

        if let Ok(out) = &sh_result {
            if out.status.success() {
                return Ok(ToolResult::ok(format!("Launched '{}' via shell", app_name)));
            }
        }

        Ok(ToolResult::err(format!(
            "Failed to launch '{}'. Tried gtk-launch, xdg-open, and direct shell execution.",
            app_name
        )))
    }
}

// ── macOS implementations ─────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    pub async fn click(input: &Value, button: u8) -> Result<ToolResult> {
        let (x, y) = require_coords(input)?;
        let btn = match button { 1 => "c", 2 => "dc", 3 => "rc", _ => "c" };
        let action = if button == 2 {
            format!("dc:{},{}", x, y)
        } else {
            format!("{}:{},{}", btn, x, y)
        };
        run_cmd("cliclick", &[&action]).await?;
        Ok(ToolResult::ok(format!("Click at ({},{})", x, y)))
    }

    pub async fn drag(input: &Value) -> Result<ToolResult> {
        let (x, y) = require_coords(input)?;
        let to_x = input["to_x"].as_i64().unwrap_or(0) as i32;
        let to_y = input["to_y"].as_i64().unwrap_or(0) as i32;
        run_cmd("cliclick", &[
            &format!("dd:{},{}", x, y),
            &format!("dm:{},{}", to_x, to_y),
            &format!("du:{},{}", to_x, to_y),
        ]).await?;
        Ok(ToolResult::ok(format!("Dragged from ({},{}) to ({},{})", x, y, to_x, to_y)))
    }

    pub async fn move_mouse(input: &Value) -> Result<ToolResult> {
        let (x, y) = require_coords(input)?;
        run_cmd("cliclick", &[&format!("m:{},{}", x, y)]).await?;
        Ok(ToolResult::ok(format!("Mouse moved to ({},{})", x, y)))
    }

    pub async fn get_cursor_position() -> Result<ToolResult> {
        let output = Command::new("osascript")
            .args(["-e", "tell application \"System Events\" to get position of mouse"])
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("osascript failed: {}", e))?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(ToolResult::ok(format!("Cursor at {}", stdout)))
    }

    pub async fn type_text(input: &Value) -> Result<ToolResult> {
        let text = match input["text"].as_str() {
            Some(t) => t,
            None => return Ok(ToolResult::err("Missing required parameter: text")),
        };
        run_cmd("cliclick", &[&format!("t:{}", text)]).await?;
        Ok(ToolResult::ok(format!("Typed text ({} chars)", text.len())))
    }

    pub async fn hotkey(input: &Value) -> Result<ToolResult> {
        let keys: Vec<String> = match input["keys"].as_array() {
            Some(arr) => arr.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            None => return Ok(ToolResult::err("Missing required parameter: keys (array of strings)")),
        };
        if keys.is_empty() {
            return Ok(ToolResult::err("keys array must not be empty"));
        }
        let combo = keys.join("+");
        // Convert to cliclick format: kd:key1,key2 ku:key2,key1
        let kd = format!("kd:{}", keys.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(","));
        let ku = format!("ku:{}", keys.iter().rev().map(|k| k.as_str()).collect::<Vec<_>>().join(","));
        run_cmd("cliclick", &[&kd, &ku]).await?;
        Ok(ToolResult::ok(format!("Hotkey '{}' sent", combo)))
    }

    pub async fn list_windows() -> Result<ToolResult> {
        let output = Command::new("osascript")
            .args(["-e", "tell application \"System Events\" to get {name, position, size} of every window of every process whose visible is true"])
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("osascript window list failed: {}", e))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(ToolResult::ok(format!("Windows:\n{}", stdout.trim())))
    }

    pub async fn activate_window(input: &Value) -> Result<ToolResult> {
        let title = match input["window_title"].as_str() {
            Some(t) => t,
            None => return Ok(ToolResult::err("Missing required parameter: window_title")),
        };
        let escaped = title.replace('"', "\\\"");
        let script = format!(
            "tell application \"System Events\" to set frontmost of (first process whose name contains \"{}\") to true",
            escaped
        );
        run_cmd("osascript", &["-e", &script]).await?;
        Ok(ToolResult::ok(format!("Activated window '{}'", title)))
    }

    pub async fn scroll(input: &Value) -> Result<ToolResult> {
        let _dir = input["scroll_direction"].as_str().unwrap_or("down");
        let amount = input["scroll_amount"].as_u64().unwrap_or(3) as i32;
        let (_x, _y) = require_coords(input)?;
        // cliclick doesn't support scroll directly; use osascript
        let factor = if _dir == "up" { amount } else { -amount };
        let script = format!(
            "tell application \"System Events\" to repeat {} times\nkey code 116\nend repeat",
            amount
        );
        // Simpler: just use page up/down for scroll
        let key = if _dir == "up" { "116" } else { "121" };
        let script2 = format!(
            "tell application \"System Events\" to repeat {} times\nkey code {}\nend repeat",
            amount, key
        );
        run_cmd("osascript", &["-e", &script2]).await?;
        Ok(ToolResult::ok(format!("Scrolled {} {} times", amount, _dir)))
    }

    pub async fn launch_app(input: &Value) -> Result<ToolResult> {
        let app_name = match input["app_name"].as_str() {
            Some(n) => n,
            None => return Ok(ToolResult::err("Missing required parameter: app_name")),
        };
        run_cmd("open", &["-a", app_name]).await?;
        Ok(ToolResult::ok(format!("Launched '{}'", app_name)))
    }
}

// ── Windows implementations (basic PowerShell wrappers) ────────────────────────

#[cfg(target_os = "windows")]
mod imp {
    use super::*;

    pub async fn click(input: &Value, _button: u8) -> Result<ToolResult> {
        let (x, y) = require_coords(input)?;
        let ps = format!(
            "Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.Cursor]::Position = New-Object System.Drawing.Point({},{})",
            x, y
        );
        run_cmd("powershell", &["-Command", &ps]).await?;
        Ok(ToolResult::ok(format!(
            "Mouse moved to ({},{}) — use uia tool for full click/drag on Windows",
            x, y
        )))
    }

    pub async fn drag(input: &Value) -> Result<ToolResult> {
        let (x, y) = require_coords(input)?;
        let to_x = input["to_x"].as_i64().unwrap_or(0);
        let to_y = input["to_y"].as_i64().unwrap_or(0);
        Ok(ToolResult::ok(format!(
            "Drag from ({},{}) to ({},{}) — use uia tool for drag_drop on Windows",
            x, y, to_x, to_y
        )))
    }

    pub async fn move_mouse(input: &Value) -> Result<ToolResult> {
        click(input, 0).await
    }

    pub async fn get_cursor_position() -> Result<ToolResult> {
        Ok(ToolResult::ok("Use uia tool for cursor position on Windows".to_string()))
    }

    pub async fn type_text(input: &Value) -> Result<ToolResult> {
        let text = match input["text"].as_str() {
            Some(t) => t,
            None => return Ok(ToolResult::err("Missing required parameter: text")),
        };
        run_cmd("powershell", &["-Command", &format!("[System.Windows.Forms.SendKeys]::SendWait('{}')", text)]).await?;
        Ok(ToolResult::ok(format!("Typed text ({} chars)", text.len())))
    }

    pub async fn hotkey(input: &Value) -> Result<ToolResult> {
        let keys: Vec<String> = match input["keys"].as_array() {
            Some(arr) => arr.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            None => return Ok(ToolResult::err("Missing keys array")),
        };
        let combo = keys.join("+");
        // Convert to SendKeys format: ^ for Ctrl, % for Alt, + for Shift
        let sk: String = keys.iter().map(|k| {
            match k.to_lowercase().as_str() {
                "ctrl" | "control" => "^",
                "alt" => "%",
                "shift" => "+",
                other => other,
            }
        }).collect();
        run_cmd("powershell", &["-Command", &format!("[System.Windows.Forms.SendKeys]::SendWait('{}')", sk)]).await?;
        Ok(ToolResult::ok(format!("Hotkey '{}' sent", combo)))
    }

    pub async fn list_windows() -> Result<ToolResult> {
        run_cmd("powershell", &["-Command", "Get-Process | Where-Object {$_.MainWindowTitle} | Select-Object Id, MainWindowTitle | Format-Table -AutoSize"]).await
    }

    pub async fn activate_window(input: &Value) -> Result<ToolResult> {
        let title = match input["window_title"].as_str() {
            Some(t) => t,
            None => return Ok(ToolResult::err("Missing window_title")),
        };
        let ps = format!(
            r#"Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32 {{
    [DllImport("user32.dll")]
    public static extern IntPtr FindWindow(string lpClassName, string lpWindowName);
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);
}}
"@
$hwnd = [Win32]::FindWindow($null, '*{}*')
if ($hwnd) {{ [Win32]::SetForegroundWindow($hwnd); Write-Output "activated" }} else {{ Write-Error "not found" }}"#,
            title
        );
        run_cmd("powershell", &["-Command", &ps]).await
    }

    pub async fn scroll(_input: &Value) -> Result<ToolResult> {
        Ok(ToolResult::ok("Scroll not implemented on Windows via desktop_automation — use uia tool".to_string()))
    }

    pub async fn launch_app(input: &Value) -> Result<ToolResult> {
        let app_name = match input["app_name"].as_str() {
            Some(n) => n,
            None => return Ok(ToolResult::err("Missing app_name")),
        };
        run_cmd("cmd", &["/c", "start", "", app_name]).await?;
        Ok(ToolResult::ok(format!("Launched '{}'", app_name)))
    }
}

// ── Dispatch to platform module ────────────────────────────────────────────────

async fn platform_click(input: &Value, button: u8) -> Result<ToolResult> {
    imp::click(input, button).await
}
async fn platform_drag(input: &Value) -> Result<ToolResult> {
    imp::drag(input).await
}
async fn platform_move_mouse(input: &Value) -> Result<ToolResult> {
    imp::move_mouse(input).await
}
async fn platform_get_cursor_position() -> Result<ToolResult> {
    imp::get_cursor_position().await
}
async fn platform_type_text(input: &Value) -> Result<ToolResult> {
    imp::type_text(input).await
}
async fn platform_hotkey(input: &Value) -> Result<ToolResult> {
    imp::hotkey(input).await
}
async fn platform_list_windows() -> Result<ToolResult> {
    imp::list_windows().await
}
async fn platform_activate_window(input: &Value) -> Result<ToolResult> {
    imp::activate_window(input).await
}
async fn platform_scroll(input: &Value) -> Result<ToolResult> {
    imp::scroll(input).await
}
async fn platform_launch_app(input: &Value) -> Result<ToolResult> {
    imp::launch_app(input).await
}
