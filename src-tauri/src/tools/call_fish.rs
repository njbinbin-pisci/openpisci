/// call_fish tool — lets the main agent delegate a sub-task to a Fish sub-agent.
///
/// Stateless: each call creates a fresh AgentLoop with only a system prompt +
/// the task as a single user message. No session is created, no DB writes.
/// While the Fish runs, FishProgress events are forwarded to the parent session
/// so the user can see real-time progress in the main Chat view.
use crate::agent::loop_::{AgentLoop, ConfirmFlags};
use crate::agent::messages::AgentEvent;
use crate::agent::tool::{Tool, ToolContext, ToolResult, ToolSettings};
use crate::llm::{LlmMessage, MessageContent};
use crate::store::AppState;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{atomic::AtomicBool, Arc};
use tauri::{AppHandle, Emitter, Manager};

pub struct CallFishTool {
    pub app: AppHandle,
}

#[async_trait]
impl Tool for CallFishTool {
    fn name(&self) -> &str {
        "call_fish"
    }

    fn description(&self) -> &str {
        "Delegate a sub-task to a specialized Fish sub-agent. \
         Fish agents are stateless, ephemeral workers — each call starts fresh. \
         **Key benefit**: all intermediate tool calls and reasoning happen inside the Fish \
         and are NOT added to your context, only the final result is returned. \
         Use this to keep your context clean when a task involves many steps \
         (batch file processing, data collection, code scanning, etc.). \
         \
         Actions: \
         - 'list': List all available Fish agents with their descriptions. \
         - 'call': Send a task to a specific Fish and wait for the result. \
         Provide a complete, self-contained task description since the Fish has no access to conversation history."
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

    fn is_read_only(&self) -> bool {
        false
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let action = input["action"].as_str().unwrap_or("list");
        match action {
            "list" => self.list_fish().await,
            "call" => self.call_fish(&input, ctx).await,
            _ => Ok(ToolResult::err(format!(
                "Unknown action '{}'. Use: list, call",
                action
            ))),
        }
    }
}

impl CallFishTool {
    fn state(&self) -> tauri::State<'_, AppState> {
        self.app.state::<AppState>()
    }

    async fn list_fish(&self) -> anyhow::Result<ToolResult> {
        let registry =
            crate::fish::FishRegistry::load(self.app.path().app_data_dir().ok().as_deref());
        let fish_list: Vec<String> = registry
            .list()
            .iter()
            .map(|f| {
                format!(
                    "- {} (id: {}): {}{}",
                    f.name,
                    f.id,
                    f.description,
                    if f.agent.model.is_empty() {
                        String::new()
                    } else {
                        format!(" [model: {}]", f.agent.model)
                    }
                )
            })
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

    async fn call_fish(&self, input: &Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let fish_id = match input["fish_id"].as_str() {
            Some(id) if !id.trim().is_empty() => id.trim().to_string(),
            _ => return Ok(ToolResult::err("'fish_id' is required for action 'call'")),
        };
        let task = match input["task"].as_str() {
            Some(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => return Ok(ToolResult::err("'task' is required for action 'call'")),
        };

        let state = self.state();

        let app_data_dir = self.app.path().app_data_dir().ok();
        let registry = crate::fish::FishRegistry::load(app_data_dir.as_deref());
        let fish_def = match registry.get(&fish_id) {
            Some(f) => f.clone(),
            None => {
                return Ok(ToolResult::err(format!(
                    "Fish '{}' not found. Use action 'list' to see available Fish agents.",
                    fish_id
                )))
            }
        };

        let parent_session_id = ctx.session_id.clone();

        tracing::info!(
            "call_fish: stateless delegation to fish='{}' parent_session='{}'",
            fish_id,
            parent_session_id
        );

        // Inherit the parent session's cancel flag so that clicking "Stop" in the
        // main chat also cancels the Fish sub-agent immediately.
        let cancel = {
            let flags = state.cancel_flags.lock().await;
            flags
                .get(&parent_session_id)
                .cloned()
                .unwrap_or_else(|| Arc::new(AtomicBool::new(false)))
        };

        let fish_system_prompt = fish_def.agent.system_prompt.clone();

        // Read settings snapshot
        let (
            provider,
            model,
            api_key,
            base_url,
            workspace_root,
            max_tokens,
            _context_window,
            policy_mode,
            tool_rate_limit_per_minute,
            tool_settings,
            builtin_tool_enabled,
            allow_outside_workspace,
            vision_enabled,
        ) = {
            let settings = state.settings.lock().await;
            (
                settings.provider.clone(),
                settings.model.clone(),
                settings.active_api_key().to_string(),
                settings.custom_base_url.clone(),
                settings.workspace_root.clone(),
                settings.max_tokens,
                settings.context_window,
                settings.policy_mode.clone(),
                settings.tool_rate_limit_per_minute,
                Arc::new(ToolSettings::from_settings(&settings)),
                settings.builtin_tool_enabled.clone(),
                settings.allow_outside_workspace,
                settings.vision_enabled,
            )
        };
        if api_key.is_empty() {
            return Ok(ToolResult::err("API key not configured"));
        }

        let vision_capable =
            vision_enabled || crate::commands::chat::model_supports_vision(&provider, &model);

        // Build a fresh message list: only the task as a single user message
        let llm_messages = vec![LlmMessage {
            role: "user".into(),
            content: MessageContent::text(&task),
        }];

        let client = crate::llm::build_client(
            &provider,
            &api_key,
            if base_url.is_empty() {
                None
            } else {
                Some(&base_url)
            },
        );

        let user_tools_dir = self
            .app
            .path()
            .app_data_dir()
            .map(|d| d.join("user-tools"))
            .ok();
        let registry_tools = Arc::new(crate::tools::build_registry(
            state.browser.clone(),
            user_tools_dir.as_deref(),
            Some(state.db.clone()),
            Some(&builtin_tool_enabled),
            None, // no call_fish inside Fish (prevent recursion)
            None,
            None,
            None,
        ));

        let policy = Arc::new(crate::policy::PolicyGate::with_profile_and_flags(
            &workspace_root,
            &policy_mode,
            tool_rate_limit_per_minute,
            allow_outside_workspace,
        ));

        let agent = AgentLoop {
            client,
            registry: registry_tools,
            policy,
            system_prompt: fish_system_prompt,
            model,
            max_tokens,
            context_window: 0,
            fallback_models: vec![],
            db: None, // stateless — no DB persistence
            app_handle: Some(state.app_handle.clone()),
            confirmation_responses: None,
            confirm_flags: ConfirmFlags {
                confirm_shell: false,
                confirm_file_write: false,
            },
            vision_override: Some(vision_capable),
            notification_rx: None,
            auto_compact_input_tokens_threshold: 100_000,
        };

        let fish_ctx = ToolContext {
            session_id: format!("fish_ephemeral_{}", fish_id),
            workspace_root: std::path::PathBuf::from(&workspace_root),
            bypass_permissions: false,
            settings: tool_settings,
            max_iterations: Some(fish_def.agent.max_iterations),
            memory_owner_id: ctx.memory_owner_id.clone(),
            pool_session_id: ctx.pool_session_id.clone(),
            cancel: ctx.cancel.clone(),
        };

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);

        // Forward FishProgress events to the parent session
        let app_fwd = state.app_handle.clone();
        let parent_sid = parent_session_id.clone();
        let fish_id_fwd = fish_id.clone();
        let fish_name_fwd = fish_def.name.clone();
        let forward_handle = tokio::spawn(async move {
            let mut iteration: u32 = 0;
            while let Some(event) = event_rx.recv().await {
                let progress = match &event {
                    AgentEvent::TextSegmentStart { iteration: it } => {
                        iteration = *it;
                        Some(AgentEvent::FishProgress {
                            fish_id: fish_id_fwd.clone(),
                            fish_name: fish_name_fwd.clone(),
                            iteration: *it,
                            tool_name: None,
                            status: "thinking".to_string(),
                            text_delta: None,
                        })
                    }
                    AgentEvent::TextDelta { delta } => Some(AgentEvent::FishProgress {
                        fish_id: fish_id_fwd.clone(),
                        fish_name: fish_name_fwd.clone(),
                        iteration,
                        tool_name: None,
                        status: "thinking_text".to_string(),
                        text_delta: Some(delta.clone()),
                    }),
                    AgentEvent::ToolStart { name, .. } => Some(AgentEvent::FishProgress {
                        fish_id: fish_id_fwd.clone(),
                        fish_name: fish_name_fwd.clone(),
                        iteration,
                        tool_name: Some(name.clone()),
                        status: "tool_call".to_string(),
                        text_delta: None,
                    }),
                    AgentEvent::ToolEnd { name, .. } => Some(AgentEvent::FishProgress {
                        fish_id: fish_id_fwd.clone(),
                        fish_name: fish_name_fwd.clone(),
                        iteration,
                        tool_name: Some(name.clone()),
                        status: "tool_done".to_string(),
                        text_delta: None,
                    }),
                    AgentEvent::Done { .. } => Some(AgentEvent::FishProgress {
                        fish_id: fish_id_fwd.clone(),
                        fish_name: fish_name_fwd.clone(),
                        iteration,
                        tool_name: None,
                        status: "done".to_string(),
                        text_delta: None,
                    }),
                    _ => None,
                };
                if let Some(prog) = progress {
                    let prog_payload = serde_json::to_value(&prog).unwrap_or_default();
                    let _ = app_fwd.emit(&format!("agent_event_{}", parent_sid), prog_payload);
                }
            }
        });

        let run_result = agent.run(llm_messages, event_tx, cancel, fish_ctx).await;
        let _ = forward_handle.await;

        match run_result {
            Ok((final_msgs, _, _)) => {
                let reply = final_msgs
                    .iter()
                    .rev()
                    .find(|m| m.role == "assistant")
                    .map(|m| m.content.as_text())
                    .unwrap_or_default();

                let summary = if reply.chars().count() > 2000 {
                    format!(
                        "{}…\n[truncated, {} chars total]",
                        reply.chars().take(2000).collect::<String>(),
                        reply.chars().count()
                    )
                } else {
                    reply
                };
                Ok(ToolResult::ok(format!(
                    "Fish '{}' completed the task.\n\nResult:\n{}",
                    fish_def.name, summary
                )))
            }
            Err(e) => Ok(ToolResult::err(format!(
                "Fish '{}' failed: {}",
                fish_def.name, e
            ))),
        }
    }
}
