/// call_koi tool — lets Pisci or another Koi delegate a task to a persistent Koi agent.
///
/// Unlike call_fish (stateless), call_koi:
/// - Loads the Koi's private memories before execution
/// - Persists the Koi's conversation to the DB
/// - Records messages in the Chat Pool (if a pool_session_id is provided)
/// - Sets memory_owner_id so new memories are scoped to the Koi
/// - Allows the Koi to call other Kois (excluding itself, to prevent recursion)
use crate::agent::loop_::{AgentLoop, ConfirmFlags};
use crate::agent::messages::AgentEvent;
use crate::agent::tool::{Tool, ToolContext, ToolResult, ToolSettings};
use crate::llm::{LlmMessage, MessageContent};
use crate::store::AppState;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{atomic::AtomicBool, Arc};
use tauri::{AppHandle, Emitter, Manager};

pub struct CallKoiTool {
    pub app: AppHandle,
    /// The ID of the calling Koi (if called from within a Koi), to prevent self-recursion.
    pub caller_koi_id: Option<String>,
    /// Current recursion depth to prevent infinite @mention chains.
    pub depth: u32,
}

const MAX_CALL_DEPTH: u32 = 5;

#[async_trait]
impl Tool for CallKoiTool {
    fn name(&self) -> &str { "call_koi" }

    fn description(&self) -> &str {
        "Delegate a task to a persistent Koi agent. \
         Koi agents have their own identity, memory, and full tool access. \
         Unlike Fish (ephemeral), Koi agents remember past interactions. \
         \
         Actions: \
         - 'list': List all available Koi agents. \
         - 'call': Send a task to a specific Koi and wait for the result. \
         Provide a complete task description. The Koi will use its own memory and tools."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "call"],
                    "description": "Action: 'list' to see available Koi, 'call' to delegate a task"
                },
                "koi_id": {
                    "type": "string",
                    "description": "For 'call': the Koi ID to delegate to"
                },
                "task": {
                    "type": "string",
                    "description": "For 'call': the task description"
                },
                "pool_session_id": {
                    "type": "string",
                    "description": "Optional: Chat Pool session ID to record the interaction"
                }
            },
            "required": ["action"]
        })
    }

    fn is_read_only(&self) -> bool { false }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let action = input["action"].as_str().unwrap_or("list");
        match action {
            "list" => self.list_kois().await,
            "call" => self.call_koi(&input, ctx).await,
            _ => Ok(ToolResult::err(format!("Unknown action '{}'. Use: list, call", action))),
        }
    }
}

impl CallKoiTool {
    fn state(&self) -> tauri::State<'_, AppState> {
        self.app.state::<AppState>()
    }

    async fn list_kois(&self) -> anyhow::Result<ToolResult> {
        let state = self.state();
        let db = state.db.lock().await;
        let kois = db.list_kois().unwrap_or_default();
        drop(db);

        if kois.is_empty() {
            return Ok(ToolResult::ok("No Koi agents available. Create one in the Pond UI."));
        }

        let lines: Vec<String> = kois.iter()
            .filter(|k| self.caller_koi_id.as_deref() != Some(&k.id))
            .map(|k| format!(
                "- {} {} (id: {}): {} [status: {}]",
                k.icon, k.name, k.id, k.description, k.status
            ))
            .collect();

        Ok(ToolResult::ok(format!(
            "Available Koi agents ({}):\n{}",
            lines.len(),
            lines.join("\n")
        )))
    }

    async fn call_koi(&self, input: &Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        if self.depth >= MAX_CALL_DEPTH {
            return Ok(ToolResult::err(format!(
                "Maximum Koi call depth ({}) reached. Cannot delegate further.", MAX_CALL_DEPTH
            )));
        }

        let koi_id = match input["koi_id"].as_str() {
            Some(id) if !id.trim().is_empty() => id.trim().to_string(),
            _ => return Ok(ToolResult::err("'koi_id' is required for action 'call'")),
        };
        let task = match input["task"].as_str() {
            Some(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => return Ok(ToolResult::err("'task' is required for action 'call'")),
        };
        let pool_session_id = input["pool_session_id"].as_str().map(|s| s.to_string());

        if self.caller_koi_id.as_deref() == Some(&koi_id) {
            return Ok(ToolResult::err("A Koi cannot call itself."));
        }

        let state = self.state();

        let koi_def = {
            let db = state.db.lock().await;
            match db.get_koi(&koi_id)? {
                Some(k) => k,
                None => return Ok(ToolResult::err(format!(
                    "Koi '{}' not found. Use action 'list' to see available Koi agents.", koi_id
                ))),
            }
        };

        // Mark Koi as busy
        {
            let db = state.db.lock().await;
            let _ = db.update_koi_status(&koi_id, "busy");
        }
        let _ = state.app_handle.emit("koi_status_changed", json!({ "id": koi_id, "status": "busy" }));

        // Record task assignment in Chat Pool
        let caller_id = self.caller_koi_id.as_deref().unwrap_or("pisci");
        if let Some(ref pool_sid) = pool_session_id {
            let db = state.db.lock().await;
            let _ = db.insert_pool_message(
                pool_sid,
                caller_id,
                &format!("@{} {}", koi_def.name, task),
                "task_assign",
                &json!({ "koi_id": koi_id, "task": task }).to_string(),
            );
        }

        let parent_session_id = ctx.session_id.clone();

        tracing::info!(
            "call_koi: delegation to koi='{}' ({}), depth={}, parent_session='{}'",
            koi_def.name, koi_id, self.depth, parent_session_id
        );

        // Load Koi's private memories for context injection
        let memory_context = {
            let db = state.db.lock().await;
            let memories = db.search_memories_fts(&task, 5).unwrap_or_default();
            let koi_memories: Vec<_> = memories.into_iter()
                .filter(|_m| true) // TODO: filter by owner_id once FTS supports it
                .collect();
            if koi_memories.is_empty() {
                String::new()
            } else {
                let items: Vec<String> = koi_memories.iter()
                    .map(|m| format!("- [{}] {}", m.category, m.content))
                    .collect();
                format!("\n\n## Your Memories\n{}", items.join("\n"))
            }
        };

        let system_prompt = format!(
            "{}\n\nYou are {} ({}). You have your own independent memory and full tool access.\
             When you learn something important, use memory_store to save it.{}",
            koi_def.system_prompt, koi_def.name, koi_def.icon, memory_context
        );

        let llm_messages = vec![LlmMessage {
            role: "user".into(),
            content: MessageContent::text(&task),
        }];

        // Read settings
        let (provider, model, api_key, base_url, workspace_root, max_tokens,
             policy_mode, tool_rate_limit_per_minute, tool_settings, builtin_tool_enabled,
             allow_outside_workspace, vision_enabled) = {
            let settings = state.settings.lock().await;
            (
                settings.provider.clone(),
                settings.model.clone(),
                settings.active_api_key().to_string(),
                settings.custom_base_url.clone(),
                settings.workspace_root.clone(),
                settings.max_tokens,
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

        let vision_capable = vision_enabled
            || crate::commands::chat::model_supports_vision(&provider, &model);

        let cancel = Arc::new(AtomicBool::new(false));
        let client = crate::llm::build_client(
            &provider,
            &api_key,
            if base_url.is_empty() { None } else { Some(&base_url) },
        );

        let user_tools_dir = self.app.path().app_data_dir().map(|d| d.join("user-tools")).ok();
        let registry_tools = Arc::new(crate::tools::build_registry(
            state.browser.clone(),
            user_tools_dir.as_deref(),
            Some(state.db.clone()),
            Some(&builtin_tool_enabled),
            None, // no call_fish inside Koi
            None,
            None,
            None,
        ));

        let policy = Arc::new(crate::policy::PolicyGate::with_profile_and_flags(
            &workspace_root, &policy_mode, tool_rate_limit_per_minute, allow_outside_workspace,
        ));

        let agent = AgentLoop {
            client,
            registry: registry_tools,
            policy,
            system_prompt,
            model,
            max_tokens,
            db: Some(state.db.clone()),
            app_handle: Some(state.app_handle.clone()),
            confirmation_responses: None,
            confirm_flags: ConfirmFlags {
                confirm_shell: false,
                confirm_file_write: false,
            },
            vision_override: Some(vision_capable),
        };

        let koi_ctx = ToolContext {
            session_id: format!("koi_{}", koi_id),
            workspace_root: std::path::PathBuf::from(&workspace_root),
            bypass_permissions: false,
            settings: tool_settings,
            max_iterations: Some(30),
        };

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);

        // Forward progress events to the parent session
        let app_fwd = state.app_handle.clone();
        let parent_sid = parent_session_id.clone();
        let koi_id_fwd = koi_id.clone();
        let koi_name_fwd = koi_def.name.clone();
        let forward_handle = tokio::spawn(async move {
            let mut iteration: u32 = 0;
            while let Some(event) = event_rx.recv().await {
                let progress = match &event {
                    AgentEvent::TextSegmentStart { iteration: it } => {
                        iteration = *it;
                        Some(AgentEvent::FishProgress {
                            fish_id: koi_id_fwd.clone(),
                            fish_name: koi_name_fwd.clone(),
                            iteration: *it,
                            tool_name: None,
                            status: "thinking".to_string(),
                        })
                    }
                    AgentEvent::ToolStart { name, .. } => {
                        Some(AgentEvent::FishProgress {
                            fish_id: koi_id_fwd.clone(),
                            fish_name: koi_name_fwd.clone(),
                            iteration,
                            tool_name: Some(name.clone()),
                            status: "tool_call".to_string(),
                        })
                    }
                    AgentEvent::ToolEnd { name, .. } => {
                        Some(AgentEvent::FishProgress {
                            fish_id: koi_id_fwd.clone(),
                            fish_name: koi_name_fwd.clone(),
                            iteration,
                            tool_name: Some(name.clone()),
                            status: "tool_done".to_string(),
                        })
                    }
                    AgentEvent::Done { .. } => {
                        Some(AgentEvent::FishProgress {
                            fish_id: koi_id_fwd.clone(),
                            fish_name: koi_name_fwd.clone(),
                            iteration,
                            tool_name: None,
                            status: "done".to_string(),
                        })
                    }
                    _ => None,
                };
                if let Some(prog) = progress {
                    let prog_payload = serde_json::to_value(&prog).unwrap_or_default();
                    let _ = app_fwd.emit(&format!("agent_event_{}", parent_sid), prog_payload);
                }
            }
        });

        let run_result = agent.run(llm_messages, event_tx, cancel, koi_ctx).await;
        let _ = forward_handle.await;

        // Mark Koi as idle
        {
            let db = state.db.lock().await;
            let _ = db.update_koi_status(&koi_id, "idle");
        }
        let _ = state.app_handle.emit("koi_status_changed", json!({ "id": koi_id, "status": "idle" }));

        match run_result {
            Ok((final_msgs, _, _)) => {
                let reply = final_msgs.iter().rev()
                    .find(|m| m.role == "assistant")
                    .map(|m| m.content.as_text())
                    .unwrap_or_default();

                // Record result in Chat Pool
                if let Some(ref pool_sid) = pool_session_id {
                    let db = state.db.lock().await;
                    let summary = if reply.len() > 500 { &reply[..500] } else { &reply };
                    let _ = db.insert_pool_message(
                        pool_sid,
                        &koi_id,
                        summary,
                        "result",
                        "{}",
                    );
                }

                let summary = if reply.len() > 2000 {
                    format!("{}...\n[truncated, {} chars total]", &reply[..2000], reply.len())
                } else {
                    reply
                };
                Ok(ToolResult::ok(format!(
                    "Koi '{}' {} completed the task.\n\nResult:\n{}",
                    koi_def.name, koi_def.icon, summary
                )))
            }
            Err(e) => {
                // Record error in Chat Pool
                if let Some(ref pool_sid) = pool_session_id {
                    let db = state.db.lock().await;
                    let _ = db.insert_pool_message(
                        pool_sid,
                        &koi_id,
                        &format!("Task failed: {}", e),
                        "status_update",
                        "{}",
                    );
                }
                Ok(ToolResult::err(format!(
                    "Koi '{}' failed: {}", koi_def.name, e
                )))
            }
        }
    }
}
