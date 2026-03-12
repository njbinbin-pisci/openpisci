use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

const MAX_TEXT_BYTES: u64 = 256 * 1024; // 256 KB
const MAX_IMAGE_BYTES: u64 = 4 * 1024 * 1024; // 4 MB

pub struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read the contents of a known file. Returns text with line numbers, or base64 for images. \
         IMPORTANT: This tool reads FILE CONTENT only — do NOT use it to list directory contents. \
         To list files in a directory, use: shell with interpreter=cmd, command='dir C:\\SomePath /b'. \
         If you get 'permission denied', use shell with 'Get-Content \"path\"' or 'type \"path\"' instead. \
         Always use absolute paths (e.g. C:\\Users\\name\\file.txt). \
         Use offset/limit for large files to avoid reading the whole file at once."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the file (e.g. C:\\Users\\name\\file.txt). Relative paths are resolved from workspace root."
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

        let content = std::fs::read_to_string(&path).map_err(|e| {
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
            "File: {} ({} lines total, showing lines {}-{})\n\n{}",
            path.display(),
            total,
            start + 1,
            end,
            numbered
        )))
    }
}
