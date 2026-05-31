use async_trait::async_trait;
use pisci_kernel::agent::messages::AgentEvent;
use pisci_kernel::agent::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use super::chat_ui_schema;

pub struct ChatUiTool {
    pub app: AppHandle,
}

pub fn render_interactive_response_result(values: &Value) -> String {
    let json = serde_json::to_string_pretty(values).unwrap_or_else(|_| format!("{:?}", values));
    let action_type = values
        .get("__action_type__")
        .and_then(|v| v.as_str())
        .unwrap_or("submit");
    let hint = if action_type == "action" {
        "This is a non-terminal action. You may call chat_ui_patch to update the card and chat_ui_listen before expecting final submit."
    } else {
        "This is the user's final structured input from the interactive card."
    };
    format!(
        "USER_INTERACTIVE_RESPONSE_JSON:\n{}\n\n\
{hint} \
You MUST treat field ids, __data_model__, and __action__ as authoritative. \
Override any prior defaults, assumptions, examples, or tentative plans that conflict with this response. \
If a field is present here, use this submitted value exactly unless the user later changes it.",
        json
    )
}

#[async_trait]
impl Tool for ChatUiTool {
    fn name(&self) -> &str {
        "chat_ui"
    }

    fn description(&self) -> &str {
        "Display an interactive UI card (Chat UI Protocol v2; v1 compatible) for structured user input. \
         Supports layout (row/column/card), image/code_preview/progress/link_list, wizard steps, file_picker, \
         text/number/date/time/slider/switch, select/radio/checkbox/tags, koi/project pickers, show_when, validation. \
         Buttons: emit=submit (terminal) or emit=action (non-terminal; then use chat_ui_patch + chat_ui_listen). \
         Include data{} for v2 data model. Catalog: docs/pisci.chat.catalog.json — spec: docs/chat-ui-protocol.md. \
         NOT for simple yes/no. Blocks until submit (or action+listen flow)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["ui_definition"],
            "properties": {
                "ui_definition": chat_ui_schema::ui_definition_schema()
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let ui_def = input.get("ui_definition").cloned().unwrap_or(Value::Null);

        if ui_def.is_null() || ui_def.get("blocks").is_none() {
            return Ok(ToolResult::err(
                "ui_definition must contain a 'blocks' array.",
            ));
        }

        let request_id = ctx
            .tool_use_id
            .clone()
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();

        {
            let state = self.app.state::<crate::store::AppState>();
            let mut map = state.interactive_responses.lock().await;
            map.insert(request_id.clone(), resp_tx);
        }

        let event = AgentEvent::InteractiveUi {
            request_id: request_id.clone(),
            ui_definition: ui_def.clone(),
        };
        let event_key = format!("agent_event_{}", ctx.session_id);
        let payload = serde_json::to_value(&event).unwrap_or_default();
        let _ = self.app.emit(&event_key, payload);

        match tokio::time::timeout(std::time::Duration::from_secs(300), resp_rx).await {
            Ok(Ok(values)) => Ok(ToolResult::ok(render_interactive_response_result(&values))),
            Ok(Err(_)) => Ok(ToolResult::err(
                "Interactive UI response channel was dropped (user may have navigated away).",
            )),
            Err(_) => {
                let state = self.app.state::<crate::store::AppState>();
                let mut map = state.interactive_responses.lock().await;
                map.remove(&request_id);
                Ok(ToolResult::err(
                    "Interactive UI timed out after 5 minutes with no user response.",
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn interactive_response_result_is_authoritative_and_machine_readable() {
        let result = render_interactive_response_result(&json!({
            "game_type": "puzzle",
            "project_name": "timy",
            "__data_model__": { "game_type": "puzzle" },
            "__action_type__": "submit"
        }));

        assert!(result.contains("USER_INTERACTIVE_RESPONSE_JSON"));
        assert!(result.contains("\"game_type\": \"puzzle\""));
        assert!(result.contains("authoritative"));
    }

    #[test]
    fn action_response_hints_patch_listen() {
        let result = render_interactive_response_result(&json!({
            "__action_type__": "action",
            "__action__": "preview"
        }));
        assert!(result.contains("chat_ui_patch"));
        assert!(result.contains("chat_ui_listen"));
    }
}
