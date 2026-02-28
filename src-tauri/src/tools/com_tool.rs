/// COM/OLE/Shell tool for Windows — clipboard, shell operations, special folders.
/// Windows only.
use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ComTool;

#[async_trait]
impl Tool for ComTool {
    fn name(&self) -> &str { "com" }

    fn description(&self) -> &str {
        "Windows COM/Shell operations: read/write clipboard (text, image, file list), \
         open files with default programs, explore folders in Explorer, \
         get special folder paths (Desktop, Documents, Downloads, etc.)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "clipboard_read", "clipboard_write", "clipboard_clear",
                        "shell_open", "shell_explore", "shell_run",
                        "get_special_folder"
                    ],
                    "description": "Action to perform"
                },
                "text": {
                    "type": "string",
                    "description": "Text to write to clipboard (for clipboard_write)"
                },
                "path": {
                    "type": "string",
                    "description": "File or folder path (for shell_open, shell_explore)"
                },
                "command": {
                    "type": "string",
                    "description": "Command to run via ShellExecute (for shell_run)"
                },
                "verb": {
                    "type": "string",
                    "description": "Shell verb: 'open', 'edit', 'print', 'runas' (default: 'open')"
                },
                "folder": {
                    "type": "string",
                    "enum": [
                        "desktop", "documents", "downloads", "pictures", "music",
                        "videos", "appdata", "localappdata", "temp", "home",
                        "startup", "programs", "system", "windows"
                    ],
                    "description": "Special folder name (for get_special_folder)"
                }
            },
            "required": ["action"]
        })
    }

    fn needs_confirmation(&self, input: &Value) -> bool {
        matches!(input["action"].as_str(), Some("clipboard_write") | Some("shell_run"))
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let action = match input["action"].as_str() {
            Some(a) => a,
            None => return Ok(ToolResult::err("Missing required parameter: action")),
        };

        match action {
            "clipboard_read"    => self.clipboard_read(),
            "clipboard_write"   => self.clipboard_write(&input),
            "clipboard_clear"   => self.clipboard_clear(),
            "shell_open"        => self.shell_open(&input),
            "shell_explore"     => self.shell_explore(&input),
            "shell_run"         => self.shell_run(&input),
            "get_special_folder" => self.get_special_folder(&input),
            _ => Ok(ToolResult::err(format!("Unknown action: {}", action))),
        }
    }
}

impl ComTool {
    // ─── Clipboard ────────────────────────────────────────────────────────────

    fn clipboard_read(&self) -> Result<ToolResult> {
        use windows::Win32::System::DataExchange::{
            OpenClipboard, CloseClipboard, GetClipboardData, IsClipboardFormatAvailable,
        };
        use windows::Win32::System::Ole::{CF_UNICODETEXT, CF_HDROP};
        use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};

        unsafe {
            OpenClipboard(None).map_err(|e| anyhow::anyhow!("OpenClipboard: {}", e))?;

            let result = (|| -> Result<ToolResult> {
                // Try text first
                if IsClipboardFormatAvailable(CF_UNICODETEXT.0.into()).is_ok() {
                    let handle = GetClipboardData(CF_UNICODETEXT.0.into())
                        .map_err(|e| anyhow::anyhow!("GetClipboardData: {}", e))?;
                    let hglobal = windows::Win32::Foundation::HGLOBAL(handle.0 as *mut _);
                    let ptr = GlobalLock(hglobal) as *const u16;
                    if !ptr.is_null() {
                        let mut len = 0;
                        while *ptr.add(len) != 0 { len += 1; }
                        let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
                        let _ = GlobalUnlock(hglobal);
                        return Ok(ToolResult::ok(format!("Clipboard text ({} chars):\n{}", text.len(), text)));
                    }
                }

                // Try file drop list
                if IsClipboardFormatAvailable(CF_HDROP.0.into()).is_ok() {
                    use windows::Win32::UI::Shell::DragQueryFileW;
                    use windows::Win32::UI::Shell::HDROP;
                    let handle = GetClipboardData(CF_HDROP.0.into())
                        .map_err(|e| anyhow::anyhow!("GetClipboardData HDROP: {}", e))?;
                    let hdrop = HDROP(handle.0 as *mut _);
                    let count = DragQueryFileW(hdrop, 0xFFFFFFFF, None);
                    let mut files = Vec::new();
                    for i in 0..count {
                        let len = DragQueryFileW(hdrop, i, None) as usize + 1;
                        let mut buf = vec![0u16; len];
                        DragQueryFileW(hdrop, i, Some(&mut buf));
                        files.push(String::from_utf16_lossy(&buf[..len-1]).to_string());
                    }
                    return Ok(ToolResult::ok(format!(
                        "Clipboard files ({}):\n{}", files.len(), files.join("\n")
                    )));
                }

                Ok(ToolResult::ok("Clipboard is empty or contains unsupported format"))
            })();

            CloseClipboard().ok();
            result
        }
    }

    fn clipboard_write(&self, input: &Value) -> Result<ToolResult> {
        use windows::Win32::System::DataExchange::{
            OpenClipboard, CloseClipboard, EmptyClipboard, SetClipboardData,
        };
        use windows::Win32::System::Ole::CF_UNICODETEXT;
        use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

        let text = match input["text"].as_str() {
            Some(t) => t,
            None => return Ok(ToolResult::err("clipboard_write requires text")),
        };

        unsafe {
            OpenClipboard(None).map_err(|e| anyhow::anyhow!("OpenClipboard: {}", e))?;
            EmptyClipboard().ok();

            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let byte_len = wide.len() * 2;
            let hmem = GlobalAlloc(GMEM_MOVEABLE, byte_len)
                .map_err(|e| anyhow::anyhow!("GlobalAlloc: {}", e))?;
            let ptr = GlobalLock(hmem) as *mut u16;
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(hmem);

            SetClipboardData(CF_UNICODETEXT.0.into(), windows::Win32::Foundation::HANDLE(hmem.0 as *mut _))
                .map_err(|e| anyhow::anyhow!("SetClipboardData: {}", e))?;
            CloseClipboard().ok();
        }

        Ok(ToolResult::ok(format!("Wrote {} chars to clipboard", text.len())))
    }

    fn clipboard_clear(&self) -> Result<ToolResult> {
        use windows::Win32::System::DataExchange::{OpenClipboard, CloseClipboard, EmptyClipboard};
        unsafe {
            OpenClipboard(None).map_err(|e| anyhow::anyhow!("OpenClipboard: {}", e))?;
            let _ = EmptyClipboard();
            let _ = CloseClipboard();
        }
        Ok(ToolResult::ok("Clipboard cleared"))
    }

    // ─── Shell operations ─────────────────────────────────────────────────────

    fn shell_open(&self, input: &Value) -> Result<ToolResult> {
        let path = match input["path"].as_str() {
            Some(p) => p,
            None => return Ok(ToolResult::err("shell_open requires path")),
        };
        let verb = input["verb"].as_str().unwrap_or("open");
        self.shell_execute(verb, path, None)
    }

    fn shell_explore(&self, input: &Value) -> Result<ToolResult> {
        let path = match input["path"].as_str() {
            Some(p) => p,
            None => return Ok(ToolResult::err("shell_explore requires path")),
        };
        self.shell_execute("explore", path, None)
    }

    fn shell_run(&self, input: &Value) -> Result<ToolResult> {
        let command = match input["command"].as_str() {
            Some(c) => c,
            None => return Ok(ToolResult::err("shell_run requires command")),
        };
        let verb = input["verb"].as_str().unwrap_or("open");
        self.shell_execute(verb, command, None)
    }

    fn shell_execute(&self, verb: &str, file: &str, params: Option<&str>) -> Result<ToolResult> {
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOW;
        use windows::core::PCWSTR;

        let verb_wide: Vec<u16> = verb.encode_utf16().chain(std::iter::once(0)).collect();
        let file_wide: Vec<u16> = file.encode_utf16().chain(std::iter::once(0)).collect();
        let params_wide: Vec<u16> = params.unwrap_or("").encode_utf16().chain(std::iter::once(0)).collect();

        let result = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(verb_wide.as_ptr()),
                PCWSTR(file_wide.as_ptr()),
                if params.is_some() { PCWSTR(params_wide.as_ptr()) } else { PCWSTR::null() },
                PCWSTR::null(),
                SW_SHOW,
            )
        };

        // ShellExecuteW returns > 32 on success
        if result.0 as usize > 32 {
            Ok(ToolResult::ok(format!("Shell {} '{}' launched", verb, file)))
        } else {
            Ok(ToolResult::err(format!("ShellExecute failed with code: {:?}", result.0)))
        }
    }

    // ─── Special folders ─────────────────────────────────────────────────────

    fn get_special_folder(&self, input: &Value) -> Result<ToolResult> {
        let folder = match input["folder"].as_str() {
            Some(f) => f,
            None => return Ok(ToolResult::err("get_special_folder requires folder")),
        };

        let path = match folder {
            "desktop"    => dirs::desktop_dir(),
            "documents"  => dirs::document_dir(),
            "downloads"  => dirs::download_dir(),
            "pictures"   => dirs::picture_dir(),
            "music"      => dirs::audio_dir(),
            "videos"     => dirs::video_dir(),
            "appdata"    => dirs::config_dir(),
            "localappdata" => dirs::data_local_dir(),
            "temp"       => Some(std::env::temp_dir()),
            "home"       => dirs::home_dir(),
            "startup"    => {
                // Windows Startup folder
                dirs::data_dir().map(|d| d.join("Microsoft\\Windows\\Start Menu\\Programs\\Startup"))
            }
            "programs"   => {
                dirs::data_dir().map(|d| d.join("Microsoft\\Windows\\Start Menu\\Programs"))
            }
            "system"     => Some(std::path::PathBuf::from(r"C:\Windows\System32")),
            "windows"    => Some(std::path::PathBuf::from(r"C:\Windows")),
            _ => return Ok(ToolResult::err(format!("Unknown folder: {}", folder))),
        };

        match path {
            Some(p) => Ok(ToolResult::ok(format!("{}: {}", folder, p.display()))),
            None => Ok(ToolResult::err(format!("Could not determine path for: {}", folder))),
        }
    }
}
