use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

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

    fn needs_confirmation(&self, _input: &Value) -> bool {
        true
    }

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
// File Edit Tool (patch-based, supports single edit or batched edits array)
// ---------------------------------------------------------------------------

pub struct FileEditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "file_edit"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing exact strings with new strings. \
         Supports two modes:\n\
         1. Single edit: provide `old_string` and `new_string` — replaces one occurrence.\n\
         2. Batch edits: provide `edits` array of `{old_string, new_string}` objects — \
            all replacements are validated first (each old_string must appear exactly once) \
            then applied atomically in a single write. Prefer batch mode when making \
            multiple changes to the same file to reduce round-trips."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "Single-edit mode: the exact string to replace (must appear exactly once)"
                },
                "new_string": {
                    "type": "string",
                    "description": "Single-edit mode: the replacement string"
                },
                "edits": {
                    "type": "array",
                    "description": "Batch-edit mode: list of replacements to apply atomically. Each old_string must appear exactly once.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": { "type": "string", "description": "Exact text to replace (must appear exactly once)" },
                            "new_string": { "type": "string", "description": "Replacement text" }
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            },
            "required": ["path"]
        })
    }

    fn needs_confirmation(&self, _input: &Value) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = match input["path"].as_str() {
            Some(p) => p,
            None => return Ok(ToolResult::err("Missing required parameter: path")),
        };

        let path = if std::path::Path::new(path_str).is_absolute() {
            std::path::PathBuf::from(path_str)
        } else {
            ctx.workspace_root.join(path_str)
        };

        if !path.exists() {
            return Ok(ToolResult::err(format!(
                "File not found: {}",
                path.display()
            )));
        }

        // Build the list of (old, new) pairs from either mode
        let pairs: Vec<(String, String)> = if let Some(edits_arr) = input["edits"].as_array() {
            // Batch mode
            if edits_arr.is_empty() {
                return Ok(ToolResult::err("edits array is empty"));
            }
            let mut pairs = Vec::with_capacity(edits_arr.len());
            for (i, edit) in edits_arr.iter().enumerate() {
                let old = match edit["old_string"].as_str() {
                    Some(s) if !s.is_empty() => s.to_string(),
                    Some(_) => {
                        return Ok(ToolResult::err(format!(
                            "edits[{}].old_string cannot be empty",
                            i
                        )))
                    }
                    None => return Ok(ToolResult::err(format!("edits[{}] missing old_string", i))),
                };
                let new = match edit["new_string"].as_str() {
                    Some(s) => s.to_string(),
                    None => return Ok(ToolResult::err(format!("edits[{}] missing new_string", i))),
                };
                pairs.push((old, new));
            }
            pairs
        } else {
            // Single-edit mode (backward compatible)
            let old_str =
                match input["old_string"].as_str() {
                    Some(s) if !s.is_empty() => s.to_string(),
                    Some(_) => return Ok(ToolResult::err(
                        "old_string cannot be empty — provide the exact text you want to replace",
                    )),
                    None => {
                        return Ok(ToolResult::err(
                            "Missing required parameter: old_string (or use edits array)",
                        ))
                    }
                };
            let new_str = match input["new_string"].as_str() {
                Some(s) => s.to_string(),
                None => return Ok(ToolResult::err("Missing required parameter: new_string")),
            };
            vec![(old_str, new_str)]
        };

        let content = std::fs::read_to_string(&path)?;
        let lines_before = content.lines().count();

        // Validation pass: every old_string must appear exactly once
        for (i, (old, _)) in pairs.iter().enumerate() {
            let count = content.matches(old.as_str()).count();
            if count == 0 {
                let label = if pairs.len() == 1 {
                    "old_string".to_string()
                } else {
                    format!("edits[{}].old_string", i)
                };
                return Ok(ToolResult::err(format!(
                    "{} not found in file: {}",
                    label,
                    path.display()
                )));
            }
            if count > 1 {
                let label = if pairs.len() == 1 {
                    "old_string".to_string()
                } else {
                    format!("edits[{}].old_string", i)
                };
                return Ok(ToolResult::err(format!(
                    "{} appears {} times in file (must appear exactly once): {}",
                    label,
                    count,
                    path.display()
                )));
            }
        }

        // Check for duplicate old_strings within the batch (would cause offset confusion)
        for i in 0..pairs.len() {
            for j in (i + 1)..pairs.len() {
                if pairs[i].0 == pairs[j].0 {
                    return Ok(ToolResult::err(format!(
                        "edits[{}] and edits[{}] have the same old_string — each must be unique",
                        i, j
                    )));
                }
            }
        }

        // Apply pass: find each old_string's byte offset, sort descending, replace back-to-front
        // to keep earlier offsets valid after each replacement.
        let mut offsets: Vec<(usize, usize, usize)> = Vec::with_capacity(pairs.len()); // (byte_start, pair_idx, old_len)
        for (idx, (old, _)) in pairs.iter().enumerate() {
            if let Some(pos) = content.find(old.as_str()) {
                offsets.push((pos, idx, old.len()));
            }
        }
        offsets.sort_by(|a, b| b.0.cmp(&a.0)); // descending by position

        let mut result = content.clone();
        for (pos, pair_idx, old_len) in offsets {
            result.replace_range(pos..pos + old_len, &pairs[pair_idx].1);
        }

        std::fs::write(&path, &result)?;

        let lines_after = result.lines().count();
        let line_delta = lines_after as i64 - lines_before as i64;
        let delta_str = if line_delta >= 0 {
            format!("+{}", line_delta)
        } else {
            format!("{}", line_delta)
        };

        if pairs.len() == 1 {
            Ok(ToolResult::ok(format!(
                "Edited file: {} ({} chars → {} chars, {} lines {})",
                path.display(),
                pairs[0].0.len(),
                pairs[0].1.len(),
                lines_after,
                delta_str
            )))
        } else {
            Ok(ToolResult::ok(format!(
                "Edited file: {} ({} replacements applied, {} lines {})",
                path.display(),
                pairs.len(),
                lines_after,
                delta_str
            )))
        }
    }
}
