use async_trait::async_trait;
use pisci_kernel::agent::messages::AgentEvent;
use pisci_kernel::agent::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use super::chat_ui;

pub struct ChatUiListenTool {
    pub app: AppHandle,
}

#[async_trait]
impl Tool for ChatUiListenTool {
    fn name(&self) -> &str {
        "chat_ui_listen"
    }

    fn description(&self) -> &str {
        "Resume blocking wait for submit on an existing chat_ui card (same request_id / tool_use_id). \
         Use after chat_ui_patch when the user must confirm or submit final values following a non-terminal action."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["request_id"],
            "properties": {
                "request_id": {
                    "type": "string",
                    "description": "Original chat_ui request_id (tool_use_id)."
                }
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

        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        {
            let state = self.app.state::<crate::store::AppState>();
            let mut map = state.interactive_responses.lock().await;
            map.insert(request_id.clone(), resp_tx);
        }

        let event = AgentEvent::InteractiveUiListen {
            request_id: request_id.clone(),
        };
        let event_key = format!("agent_event_{}", ctx.session_id);
        let payload = serde_json::to_value(&event).unwrap_or_default();
        let _ = self.app.emit(&event_key, payload);

        match tokio::time::timeout(std::time::Duration::from_secs(300), resp_rx).await {
            Ok(Ok(values)) => Ok(ToolResult::ok(chat_ui::render_interactive_response_result(
                &values,
            ))),
            Ok(Err(_)) => Ok(ToolResult::err(
                "Interactive UI response channel was dropped (user may have navigated away).",
            )),
            Err(_) => {
                let state = self.app.state::<crate::store::AppState>();
                let mut map = state.interactive_responses.lock().await;
                map.remove(&request_id);
                Ok(ToolResult::err(
                    "Interactive UI listen timed out after 5 minutes with no user response.",
                ))
            }
        }
    }
}
