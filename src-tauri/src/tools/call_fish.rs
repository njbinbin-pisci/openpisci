/// call_fish tool — lets the main agent delegate a sub-task to a Fish sub-agent.
///
/// The Fish is activated (if not already), the task is sent to its session,
/// and the tool blocks until the Fish agent completes and returns its reply.
use crate::agent::tool::{Tool, ToolContext, ToolResult};
use crate::store::AppState;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

pub struct CallFishTool {
    pub app: AppHandle,
}

#[async_trait]
impl Tool for CallFishTool {
    fn name(&self) -> &str { "call_fish" }

    fn description(&self) -> &str {
        "Delegate a sub-task to a specialized Fish sub-agent. \
         Each Fish is a purpose-built AI assistant with its own tools and expertise. \
         Use this when a task is better handled by a specialist (e.g. file management, \
         web research, code review). The Fish runs the task and returns its result. \
         \
         Actions: \
         - 'list': List all available Fish agents with their descriptions. \
         - 'call': Send a task to a specific Fish and wait for the result."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "call"],
                    "description": "Action: 'list' to see available Fish, 'call' to delegate a task"
                },
                "fish_id": {
                    "type": "string",
                    "description": "For 'call': the Fish ID to delegate to (get from 'list')"
                },
                "task": {
                    "type": "string",
                    "description": "For 'call': the task description to send to the Fish agent"
                }
            },
            "required": ["action"]
        })
    }

    fn is_read_only(&self) -> bool { false }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let action = input["action"].as_str().unwrap_or("list");
        match action {
            "list" => self.list_fish().await,
            "call" => self.call_fish(&input, ctx).await,
            _ => Ok(ToolResult::err(format!("Unknown action '{}'. Use: list, call", action))),
        }
    }
}

impl CallFishTool {
    fn state(&self) -> tauri::State<'_, AppState> {
        self.app.state::<AppState>()
    }

    async fn list_fish(&self) -> anyhow::Result<ToolResult> {
        let registry = crate::fish::FishRegistry::load(
            self.app.path().app_data_dir().ok().as_deref(),
        );
        let fish_list: Vec<String> = registry
            .list()
            .iter()
            .map(|f| format!(
                "- {} (id: {}): {}{}",
                f.name,
                f.id,
                f.description,
                if f.agent.model.is_empty() { String::new() } else { format!(" [model: {}]", f.agent.model) }
            ))
            .collect();

        if fish_list.is_empty() {
            return Ok(ToolResult::ok("No Fish agents available."));
        }
        Ok(ToolResult::ok(format!(
            "Available Fish agents ({}):\n{}",
            fish_list.len(),
            fish_list.join("\n")
        )))
    }

    async fn call_fish(&self, input: &Value, _ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let fish_id = match input["fish_id"].as_str() {
            Some(id) if !id.trim().is_empty() => id.trim().to_string(),
            _ => return Ok(ToolResult::err("'fish_id' is required for action 'call'")),
        };
        let task = match input["task"].as_str() {
            Some(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => return Ok(ToolResult::err("'task' is required for action 'call'")),
        };

        let state = self.state();

        // Load Fish registry to validate fish_id
        let registry = crate::fish::FishRegistry::load(
            self.app.path().app_data_dir().ok().as_deref(),
        );
        let fish_def = match registry.get(&fish_id) {
            Some(f) => f.clone(),
            None => return Ok(ToolResult::err(format!(
                "Fish '{}' not found. Use action 'list' to see available Fish agents.", fish_id
            ))),
        };

        // Ensure the Fish session exists
        let session_id = format!("fish_{}", fish_id);
        {
            let db = state.db.lock().await;
            let session_title = fish_def.name.clone();
            let _ = db.ensure_im_session(&session_id, &session_title, &format!("fish_{}", fish_id));
        }

        tracing::info!("call_fish: delegating task to fish='{}' session='{}'", fish_id, session_id);

        // Run the Fish agent headlessly
        let result = crate::commands::chat::run_agent_headless(
            &state,
            &session_id,
            &task,
            None,
            "internal",
        ).await;

        match result {
            Ok((reply, _, _)) => {
                let summary = if reply.len() > 2000 {
                    format!("{}…\n[truncated, {} chars total]", &reply[..2000], reply.len())
                } else {
                    reply
                };
                Ok(ToolResult::ok(format!(
                    "Fish '{}' completed the task.\n\nResult:\n{}",
                    fish_def.name, summary
                )))
            }
            Err(e) => Ok(ToolResult::err(format!(
                "Fish '{}' failed: {}", fish_def.name, e
            ))),
        }
    }
}
