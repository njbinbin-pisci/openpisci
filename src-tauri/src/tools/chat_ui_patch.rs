use async_trait::async_trait;
use pisci_kernel::agent::messages::AgentEvent;
use pisci_kernel::agent::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use super::chat_ui_schema;

pub struct ChatUiPatchTool {
    pub app: AppHandle,
}

#[async_trait]
impl Tool for ChatUiPatchTool {
    fn name(&self) -> &str {
        "chat_ui_patch"
    }

    fn description(&self) -> &str {
        "Patch a live chat_ui card (Protocol v2) without blocking. Update title, data model, blocks, wizard step, or progress fields. \
         Use after a non-terminal action button (emit=action) so the user can continue on the same card. \
         Pair with chat_ui_listen when you need another blocking submit. Catalog: docs/pisci.chat.catalog.json."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["request_id", "patch"],
            "properties": {
                "request_id": {
                    "type": "string",
                    "description": "The chat_ui tool_use_id / request_id of the card to update."
                },
                "patch": chat_ui_schema::patch_schema()
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let request_id = input
            .get("request_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if request_id.is_empty() {
            return Ok(ToolResult::err("request_id is required."));
        }
        let patch = input.get("patch").cloned().unwrap_or(Value::Null);
        if patch.is_null() {
            return Ok(ToolResult::err("patch object is required."));
        }

        let event = AgentEvent::InteractiveUiPatch {
            request_id: request_id.clone(),
            patch,
        };
        let event_key = format!("agent_event_{}", ctx.session_id);
        let payload = serde_json::to_value(&event).unwrap_or_default();
        let _ = self.app.emit(&event_key, payload);

        Ok(ToolResult::ok(format!(
            "CHAT_UI_PATCH_APPLIED request_id={}\nThe live card was updated in the user's chat view.",
            request_id
        )))
    }
}
