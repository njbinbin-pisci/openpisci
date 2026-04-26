use async_trait::async_trait;
use pisci_kernel::agent::messages::AgentEvent;
use pisci_kernel::agent::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

pub struct ChatUiTool {
    pub app: AppHandle,
}

fn render_interactive_response_result(values: &Value) -> String {
    let json = serde_json::to_string_pretty(values).unwrap_or_else(|_| format!("{:?}", values));
    format!(
        "USER_INTERACTIVE_RESPONSE_JSON:\n{}\n\n\
This is the user's latest explicit structured input from the interactive card. \
You MUST treat these values as authoritative. Override any prior defaults, assumptions, examples, or tentative plans that conflict with this response. \
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
        "Display an interactive UI card in the chat for the user to make structured choices. \
         Use when the user needs to select from options, pick Koi team members, choose a project, \
         or confirm a complex action. Do NOT use for simple yes/no questions — just ask in text. \
         The tool blocks until the user submits the card, then returns USER_INTERACTIVE_RESPONSE_JSON. \
         You must treat the returned values as authoritative user input and use them exactly; do not continue with prior defaults or assumptions that conflict with the submitted response."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["ui_definition"],
            "properties": {
                "ui_definition": {
                    "type": "object",
                    "description": "The interactive UI card definition.",
                    "required": ["blocks"],
                    "properties": {
                        "title": {
                            "type": "string",
                            "description": "Card title displayed at the top."
                        },
                        "description": {
                            "type": "string",
                            "description": "Optional description text below the title."
                        },
                        "blocks": {
                            "type": "array",
                            "description": "Array of UI blocks to render.",
                            "items": {
                                "type": "object",
                                "required": ["type"],
                                "properties": {
                                    "type": {
                                        "type": "string",
                                        "enum": ["text", "radio", "checkbox", "text_input", "number_input", "select", "koi_picker", "project_picker", "confirm", "actions", "divider"],
                                        "description": "Block type."
                                    },
                                    "id": {
                                        "type": "string",
                                        "description": "Unique field ID for interactive blocks. Not needed for text/divider."
                                    },
                                    "label": {
                                        "type": "string",
                                        "description": "Label text for the field. For an action/confirm button fallback, this is display text only; semantic meaning comes from the button value."
                                    },
                                    "value": {
                                        "description": "Value submitted when this block is used as a single-button action fallback."
                                    },
                                    "content": {
                                        "type": "string",
                                        "description": "Text content for 'text' blocks (supports markdown)."
                                    },
                                    "options": {
                                        "type": "array",
                                        "description": "Options for radio/checkbox/select blocks.",
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "value": { "type": "string" },
                                                "label": { "type": "string" },
                                                "description": { "type": "string" }
                                            }
                                        }
                                    },
                                    "default": {
                                        "description": "Default value (string for radio/select, array for checkbox)."
                                    },
                                    "placeholder": {
                                        "type": "string",
                                        "description": "Placeholder for text_input/number_input."
                                    },
                                    "show_when": {
                                        "type": "object",
                                        "description": "Conditional visibility: show this block only when another field has a specific value.",
                                        "properties": {
                                            "field": { "type": "string" },
                                            "equals": { "type": "string" }
                                        }
                                    },
                                    "suggestions": {
                                        "type": "array",
                                        "description": "Suggested koi IDs for koi_picker.",
                                        "items": { "type": "string" }
                                    },
                                    "allow_new": {
                                        "type": "boolean",
                                        "description": "For project_picker: allow creating a new project."
                                    },
                                    "min": {
                                        "type": "integer",
                                        "description": "Minimum value for number_input, or minimum selections for koi_picker/checkbox."
                                    },
                                    "max": {
                                        "type": "integer",
                                        "description": "Maximum value for number_input."
                                    },
                                    "step": {
                                        "type": "integer",
                                        "description": "Step size for number_input."
                                    },
                                    "buttons": {
                                        "type": "array",
                                        "description": "Button definitions for 'actions' or 'confirm' blocks. The UI renders exactly these buttons. Each button's label is display text and value is the submitted semantic value.",
                                        "items": {
                                            "type": "object",
                                            "required": ["label"],
                                            "properties": {
                                                "id": { "type": "string" },
                                                "label": { "type": "string" },
                                                "value": {
                                                    "description": "Semantic value submitted when the user clicks this button. If omitted, the frontend falls back to id, then label."
                                                },
                                                "style": {
                                                    "type": "string",
                                                    "enum": ["primary", "danger", "default"]
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
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

        let request_id = uuid::Uuid::new_v4().to_string();
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();

        // Register the response channel
        {
            let state = self.app.state::<crate::store::AppState>();
            let mut map = state.interactive_responses.lock().await;
            map.insert(request_id.clone(), resp_tx);
        }

        // Emit the interactive UI event to the frontend
        let event = AgentEvent::InteractiveUi {
            request_id: request_id.clone(),
            ui_definition: ui_def.clone(),
        };
        let event_key = format!("agent_event_{}", ctx.session_id);
        let payload = serde_json::to_value(&event).unwrap_or_default();
        let _ = self.app.emit(&event_key, payload);

        // Wait for user response with 5-minute timeout
        match tokio::time::timeout(std::time::Duration::from_secs(300), resp_rx).await {
            Ok(Ok(values)) => Ok(ToolResult::ok(render_interactive_response_result(&values))),
            Ok(Err(_)) => Ok(ToolResult::err(
                "Interactive UI response channel was dropped (user may have navigated away).",
            )),
            Err(_) => {
                // Clean up on timeout
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
            "project_name": "timy"
        }));

        assert!(result.contains("USER_INTERACTIVE_RESPONSE_JSON"));
        assert!(result.contains("\"game_type\": \"puzzle\""));
        assert!(result.contains("\"project_name\": \"timy\""));
        assert!(result.contains("authoritative"));
        assert!(result.contains("Override any prior defaults"));
    }
}
