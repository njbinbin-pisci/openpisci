use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str { "file_write" }

    fn description(&self) -> &str {
        "Write content to a file. Creates the file and all parent directories if they don't exist. \
         Completely overwrites existing content — use file_edit if you only want to change part of a file. \
         Always use absolute paths. \
         Note: writing to system directories (C:\\Windows\\, C:\\Program Files\\) will fail with permission denied — \
         write to user directories (C:\\Users\\name\\, Desktop, Documents) or the workspace instead."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the file to write (e.g. C:\\Users\\name\\output.txt). Parent directories are created automatically."
                },
                "content": {
                    "type": "string",
                    "description": "Full content to write. This REPLACES the entire file. Use file_edit to modify only part of an existing file."
                }
            },
            "required": ["path", "content"]
        })
    }

    fn needs_confirmation(&self, _input: &Value) -> bool { true }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = match input["path"].as_str() {
            Some(p) => p,
            None => return Ok(ToolResult::err("Missing required parameter: path")),
        };
        let content = match input["content"].as_str() {
            Some(c) => c,
            None => return Ok(ToolResult::err("Missing required parameter: content")),
        };

        let path = if std::path::Path::new(path_str).is_absolute() {
            std::path::PathBuf::from(path_str)
        } else {
            ctx.workspace_root.join(path_str)
        };

        // Create parent directories
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let existed = path.exists();
        std::fs::write(&path, content)?;

        let action = if existed { "Updated" } else { "Created" };
        Ok(ToolResult::ok(format!(
            "{} file: {} ({} bytes)",
            action,
            path.display(),
            content.len()
        )))
    }
}

// ---------------------------------------------------------------------------
// File Edit Tool (patch-based)
// ---------------------------------------------------------------------------

pub struct FileEditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str { "file_edit" }

    fn description(&self) -> &str {
        "Edit a file by replacing a specific string with a new string. \
         The old_string must appear exactly once in the file."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact string to replace (must appear exactly once)"
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement string"
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    fn needs_confirmation(&self, _input: &Value) -> bool { true }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = match input["path"].as_str() {
            Some(p) => p,
            None => return Ok(ToolResult::err("Missing required parameter: path")),
        };
        let old_str = match input["old_string"].as_str() {
            Some(s) if !s.is_empty() => s,
            Some(_) => return Ok(ToolResult::err("old_string cannot be empty — provide the exact text you want to replace")),
            None => return Ok(ToolResult::err("Missing required parameter: old_string")),
        };
        let new_str = match input["new_string"].as_str() {
            Some(s) => s,
            None => return Ok(ToolResult::err("Missing required parameter: new_string")),
        };

        let path = if std::path::Path::new(path_str).is_absolute() {
            std::path::PathBuf::from(path_str)
        } else {
            ctx.workspace_root.join(path_str)
        };

        if !path.exists() {
            return Ok(ToolResult::err(format!("File not found: {}", path.display())));
        }

        let content = std::fs::read_to_string(&path)?;
        let count = content.matches(old_str).count();

        if count == 0 {
            return Ok(ToolResult::err(format!(
                "old_string not found in file: {}",
                path.display()
            )));
        }
        if count > 1 {
            return Ok(ToolResult::err(format!(
                "old_string appears {} times in file (must appear exactly once): {}",
                count, path.display()
            )));
        }

        let new_content = content.replacen(old_str, new_str, 1);
        std::fs::write(&path, &new_content)?;

        Ok(ToolResult::ok(format!(
            "Edited file: {} (replaced {} chars with {} chars)",
            path.display(), old_str.len(), new_str.len()
        )))
    }
}
