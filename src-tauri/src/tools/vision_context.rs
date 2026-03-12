use crate::agent::tool::{Tool, ToolContext, ToolResult};
use crate::agent::vision;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct VisionContextTool;

fn summarize(items: &[vision::VisionArtifactSummary]) -> String {
    if items.is_empty() {
        return "No vision artifacts stored for this session.".to_string();
    }
    items
        .iter()
        .map(|item| {
            format!(
                "- {} [{}] {} ({}){}",
                item.id,
                item.source_tool,
                item.label,
                item.media_type,
                if item.selected { " [selected]" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait]
impl Tool for VisionContextTool {
    fn name(&self) -> &str {
        "vision_context"
    }

    fn description(&self) -> &str {
        "Manage reusable visual artifacts for iterative multimodal reasoning. \
         Use this after screenshot/PDF/image tools create images, or import an image file path directly. \
         The selected artifacts are injected into the NEXT LLM round as vision input, so use this when you need to decide what to inspect next."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "add_path", "select", "clear_selection", "clear_all"],
                    "description": "The vision-context action to perform."
                },
                "path": {
                    "type": "string",
                    "description": "Image file path for action=add_path. Supports png/jpg/jpeg/gif/webp/bmp."
                },
                "label": {
                    "type": "string",
                    "description": "Optional short label for the stored artifact."
                },
                "artifact_ids": {
                    "type": "array",
                    "description": "Artifact ids for action=select.",
                    "items": { "type": "string" }
                },
                "merge": {
                    "type": "boolean",
                    "description": "For action=select: if true, append to the existing selection instead of replacing it."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let action = input["action"].as_str().unwrap_or("list");
        match action {
            "list" => {
                let items = vision::list_artifacts(&ctx.session_id).await;
                Ok(ToolResult::ok(format!(
                    "Vision artifacts in this session ({}):\n{}",
                    items.len(),
                    summarize(&items)
                )))
            }
            "add_path" => {
                let raw = match input["path"].as_str() {
                    Some(v) if !v.trim().is_empty() => v.trim(),
                    _ => return Ok(ToolResult::err("Missing required parameter: path")),
                };
                let path = if std::path::Path::new(raw).is_absolute() {
                    std::path::PathBuf::from(raw)
                } else {
                    ctx.workspace_root.join(raw)
                };
                let saved = match vision::store_image_path(
                    &ctx.session_id,
                    self.name(),
                    &path,
                    input["label"].as_str().map(str::to_string),
                )
                .await
                {
                    Ok(item) => item,
                    Err(e) => return Ok(ToolResult::err(e.to_string())),
                };
                Ok(ToolResult::ok(format!(
                    "Stored image as vision artifact:\n- {} [{}] {} ({})",
                    saved.id, saved.source_tool, saved.label, saved.media_type
                )))
            }
            "select" => {
                let ids: Vec<String> = input["artifact_ids"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if ids.is_empty() {
                    return Ok(ToolResult::err("Missing required parameter: artifact_ids"));
                }
                let selected = match vision::select_artifacts(
                    &ctx.session_id,
                    &ids,
                    input["merge"].as_bool().unwrap_or(false),
                )
                .await
                {
                    Ok(items) => items,
                    Err(e) => return Ok(ToolResult::err(e.to_string())),
                };
                Ok(ToolResult::ok(format!(
                    "Selected {} vision artifact(s) for the next LLM round:\n{}",
                    selected.len(),
                    summarize(&selected)
                )))
            }
            "clear_selection" => {
                vision::clear_selection(&ctx.session_id).await;
                Ok(ToolResult::ok(
                    "Cleared the selected vision artifacts. The next round will not receive extra visual context unless you select new artifacts.",
                ))
            }
            "clear_all" => {
                vision::clear_session(&ctx.session_id).await;
                Ok(ToolResult::ok(
                    "Cleared all stored vision artifacts for this session.",
                ))
            }
            _ => Ok(ToolResult::err(format!("Unknown action: {}", action))),
        }
    }
}
