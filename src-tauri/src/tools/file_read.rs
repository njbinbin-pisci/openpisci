use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::borrow::Cow;

const MAX_TEXT_BYTES: u64 = 256 * 1024; // 256 KB
const MAX_IMAGE_BYTES: u64 = 4 * 1024 * 1024; // 4 MB

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const UTF16_LE_BOM: &[u8] = &[0xFF, 0xFE];
const UTF16_BE_BOM: &[u8] = &[0xFE, 0xFF];

/// Decode raw file bytes to a String.
/// Priority:
///   1. Strip UTF-8 BOM if present, decode as UTF-8.
///   2. Try UTF-8 (no BOM).
///   3. Try GBK/GB18030 (common on Chinese Windows systems).
///   4. Lossy UTF-8 as last resort.
fn decode_bytes(bytes: &[u8]) -> (String, &'static str) {
    // UTF-16 LE/BE — convert via encoding_rs
    if bytes.starts_with(UTF16_LE_BOM) {
        let (cow, _, had_errors) = encoding_rs::UTF_16LE.decode(&bytes[UTF16_LE_BOM.len()..]);
        if !had_errors {
            return (cow.into_owned(), "utf-16-le");
        }
    }
    if bytes.starts_with(UTF16_BE_BOM) {
        let (cow, _, had_errors) = encoding_rs::UTF_16BE.decode(&bytes[UTF16_BE_BOM.len()..]);
        if !had_errors {
            return (cow.into_owned(), "utf-16-be");
        }
    }

    // UTF-8 with BOM
    let payload = if bytes.starts_with(UTF8_BOM) {
        &bytes[UTF8_BOM.len()..]
    } else {
        bytes
    };

    // Try strict UTF-8 first
    if let Ok(s) = std::str::from_utf8(payload) {
        let label = if bytes.starts_with(UTF8_BOM) {
            "utf-8-bom"
        } else {
            "utf-8"
        };
        return (s.to_owned(), label);
    }

    // Fall back to GBK (covers GB2312, GB18030 subset)
    let (cow, _, had_errors) = encoding_rs::GBK.decode(bytes);
    if !had_errors {
        return (cow.into_owned(), "gbk");
    }

    // Last resort: lossy UTF-8
    (String::from_utf8_lossy(bytes).into_owned(), "utf-8-lossy")
}

pub struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read the contents of a known file. Returns text with line numbers, or base64 for images. \
         IMPORTANT: This tool reads FILE CONTENT only — do NOT use it to list directory contents. \
         To list files in a directory, use file_list or shell with 'dir C:\\SomePath /b'. \
         If you get 'permission denied', use shell with 'Get-Content \"path\"' or 'type \"path\"' instead. \
         Paths: relative paths (e.g. src/auth/auth.service.ts) are resolved from workspace root — prefer relative paths when reading files inside the workspace. \
         Use offset/limit for large files to avoid reading the whole file at once.\n\
         \n\
         Encoding: the tool auto-detects and transparently handles UTF-8 BOM, UTF-16 LE/BE, and GBK/GB18030. \
         When the file is not plain UTF-8, the result header will include '[encoding: gbk]' or similar. \
         You MUST pay attention to this label: if a file is GBK-encoded (common in legacy Chinese projects, \
         system logs, .ini/.cfg files from older Windows software), the content you receive has been \
         decoded to Unicode for you to read — but if you need to write it back, you must be aware that \
         the original encoding is GBK, not UTF-8. Use shell with PowerShell to write GBK files: \
         `[System.IO.File]::WriteAllText(path, content, [System.Text.Encoding]::GetEncoding('gbk'))`. \
         For UTF-8-BOM files, file_write and file_edit will automatically preserve the BOM on write-back."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file. Relative paths (e.g. src/auth/auth.service.ts) are resolved from workspace root — prefer relative paths when working inside the workspace. Use absolute paths for files outside the workspace."
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-indexed). Use with limit to read large files in chunks."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read. Omit to read the whole file (up to 256KB)."
                }
            },
            "required": ["path"]
        })
    }

    fn description_minimal(&self) -> Cow<'_, str> {
        Cow::Borrowed(
            "Read a file's contents. Returns numbered text, or base64 for images. \
             Relative paths resolve from workspace root. Use offset/limit for large files. \
             Does NOT list directory contents — use file_list for that. \
             Auto-detects UTF-8/UTF-16/GBK and reports encoding in the result header.",
        )
    }

    fn input_schema_minimal(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path":   { "type": "string" },
                "offset": { "type": "integer", "minimum": 1 },
                "limit":  { "type": "integer", "minimum": 1 }
            },
            "required": ["path"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = match input["path"].as_str() {
            Some(p) => p,
            None => return Ok(ToolResult::err("Missing required parameter: path")),
        };

        // Resolve path relative to workspace
        let path = if std::path::Path::new(path_str).is_absolute() {
            std::path::PathBuf::from(path_str)
        } else {
            ctx.workspace_root.join(path_str)
        };

        if !path.exists() {
            // Try to suggest similar files
            return Ok(ToolResult::err(format!(
                "File not found: {}",
                path.display()
            )));
        }

        let metadata = std::fs::metadata(&path)?;

        // Determine file type by extension
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let is_image = matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
        );

        if is_image {
            if metadata.len() > MAX_IMAGE_BYTES {
                return Ok(ToolResult::err(format!(
                    "Image too large ({} bytes, max {} bytes)",
                    metadata.len(),
                    MAX_IMAGE_BYTES
                )));
            }
            let bytes = std::fs::read(&path)?;
            let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
            let media_type = match ext.as_str() {
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                _ => "image/png",
            };
            return Ok(ToolResult::ok(format!(
                "Image file: {} ({} bytes)\nbase64:{};{}",
                path.display(),
                bytes.len(),
                media_type,
                b64
            )));
        }

        let offset = input["offset"].as_u64().unwrap_or(1).max(1) as usize;
        let limit = input["limit"].as_u64().map(|l| l as usize);

        // For large files, only reject if no offset/limit is specified
        if metadata.len() > MAX_TEXT_BYTES && limit.is_none() && offset <= 1 {
            return Ok(ToolResult::err(format!(
                "File too large ({} bytes, max {} bytes). Use offset/limit parameters to read in chunks. \
                 Example: offset=1, limit=200 reads the first 200 lines.",
                metadata.len(), MAX_TEXT_BYTES
            )));
        }

        let raw = std::fs::read(&path).map_err(|e| {
            let hint = if e.kind() == std::io::ErrorKind::PermissionDenied {
                format!(
                    "Failed to read file: {} (os error 5 - 拒绝访问)\n\
                     提示：该文件受系统权限保护，无法直接读取。\
                     请改用 shell 工具（如 `Get-Content` 或 `type`）以当前用户权限读取，\
                     或确认文件路径是否正确。",
                    path.display()
                )
            } else {
                format!("Failed to read file: {}", e)
            };
            anyhow::anyhow!("{}", hint)
        })?;

        let (content, encoding) = decode_bytes(&raw);
        let encoding_note = if encoding != "utf-8" {
            format!(" [encoding: {}]", encoding)
        } else {
            String::new()
        };

        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let start = (offset - 1).min(total);
        let end = match limit {
            Some(l) => (start + l).min(total),
            None => total,
        };

        let numbered: String = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:6}|{}", start + i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(ToolResult::ok(format!(
            "File: {}{} ({} lines total, showing lines {}-{})\n\n{}",
            path.display(),
            encoding_note,
            total,
            start + 1,
            end,
            numbered
        )))
    }
}
