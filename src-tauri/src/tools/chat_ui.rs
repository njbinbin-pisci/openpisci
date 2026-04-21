use async_trait::async_trait;
use pisci_kernel::agent::messages::AgentEvent;
use pisci_kernel::agent::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

pub struct ChatUiTool {
    pub app: AppHandle,
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
         The tool blocks until the user submits the card, then returns their selections as JSON."
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
                                        "description": "Label text for the field."
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
                                        "description": "Button definitions for 'actions' block.",
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "id": { "type": "string" },
                                                "label": { "type": "string" },
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
            Ok(Ok(values)) => {
                let action = values
                    .get("__action__")
                    .and_then(|v| v.as_str())
                    .unwrap_or("submit");

                if action == "cancel" {
                    return Ok(ToolResult::ok("User cancelled the interactive card."));
                }

                let summary = serde_json::to_string_pretty(&values)
                    .unwrap_or_else(|_| format!("{:?}", values));
                Ok(ToolResult::ok(format!(
                    "User submitted the interactive card. Selections:\n{}",
                    summary
                )))
            }
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
