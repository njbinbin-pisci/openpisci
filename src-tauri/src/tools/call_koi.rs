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
    /// When true, status management (busy/idle) and pool message recording are handled
    /// by an external orchestrator (e.g. KoiRuntime). The tool only runs the agent logic.
    pub managed_externally: bool,
    /// Optional notification receiver for injecting @mention alerts mid-execution.
    /// Only set when called via KoiRuntime (managed_externally = true).
    /// Wrapped in Mutex so it can be taken from &self during call().
    pub notification_rx: std::sync::Mutex<Option<tokio::sync::mpsc::Receiver<String>>>,
}

const MAX_CALL_DEPTH: u32 = 5;

#[async_trait]
impl Tool for CallKoiTool {
    fn name(&self) -> &str {
        "call_koi"
    }

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

    fn is_read_only(&self) -> bool {
        false
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let action = input["action"].as_str().unwrap_or("list");
        match action {
            "list" => self.list_kois().await,
            "call" => self.call_koi(&input, ctx).await,
            _ => Ok(ToolResult::err(format!(
                "Unknown action '{}'. Use: list, call",
                action
            ))),
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
            return Ok(ToolResult::ok(
                "No Koi agents available. Create one in the Pond UI.",
            ));
        }

        let lines: Vec<String> = kois
            .iter()
            .filter(|k| self.caller_koi_id.as_deref() != Some(&k.id))
            .map(|k| {
                format!(
                    "- {} {} (id: {}) | role: {} | description: {} [status: {}]",
                    k.icon,
                    k.name,
                    k.id,
                    if k.role.trim().is_empty() {
                        "unspecified"
                    } else {
                        &k.role
                    },
                    k.description,
                    k.status
                )
            })
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
                "Maximum Koi call depth ({}) reached. Cannot delegate further.",
                MAX_CALL_DEPTH
            )));
        }

        let requested_koi_id = match input["koi_id"].as_str() {
            Some(id) if !id.trim().is_empty() => id.trim().to_string(),
            _ => return Ok(ToolResult::err("'koi_id' is required for action 'call'")),
        };
        let task = match input["task"].as_str() {
            Some(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => return Ok(ToolResult::err("'task' is required for action 'call'")),
        };
        let requested_pool_session_id = input["pool_session_id"]
            .as_str()
            .map(str::trim)
            .filter(|id| !id.is_empty() && *id != "current")
            .map(str::to_string)
            .or_else(|| ctx.pool_session_id.clone());

        let state = self.state();

        let (koi_def, pool_session_id, org_spec_ctx) = {
            let db = state.db.lock().await;
            let koi_def = match db.resolve_koi_identifier(&requested_koi_id)? {
                Some(k) => k,
                None => {
                    return Ok(ToolResult::err(format!(
                        "Koi '{}' not found. Use action 'list' to see available Koi agents.",
                        requested_koi_id
                    )))
                }
            };
            let pool_session = match requested_pool_session_id.as_deref() {
                Some(id) => match db.resolve_pool_session_identifier(id)? {
                    Some(session) => Some(session),
                    None => return Ok(ToolResult::err(format!("Pool '{}' not found.", id))),
                },
                None => None,
            };
            let org_spec_ctx = pool_session
                .as_ref()
                .and_then(|session| {
                    if session.org_spec.is_empty() {
                        None
                    } else {
                        Some(format!("\n\n## Project Organization\n{}", session.org_spec))
                    }
                })
                .unwrap_or_default();
            (
                koi_def,
                pool_session.map(|session| session.id),
                org_spec_ctx,
            )
        };
        let koi_id = koi_def.id.clone();

        if self.caller_koi_id.as_deref() == Some(koi_id.as_str()) {
            return Ok(ToolResult::err("A Koi cannot call itself."));
        }

        let parent_session_id = ctx.session_id.clone();

        tracing::info!(
            "call_koi: delegation to koi='{}' (requested='{}', canonical='{}'), depth={}, parent_session='{}', pool='{}'",
            koi_def.name,
            requested_koi_id,
            koi_id,
            self.depth,
            parent_session_id,
            pool_session_id.as_deref().unwrap_or("default")
        );

        // Load Koi's scoped memories for context injection
        let memory_context = {
            let db = state.db.lock().await;
            let koi_memories = db
                .search_memories_scoped(&task, &koi_id, pool_session_id.as_deref(), 5)
                .unwrap_or_default();
            if koi_memories.is_empty() {
                String::new()
            } else {
                let items: Vec<String> = koi_memories
                    .iter()
                    .map(|m| {
                        let scope_tag = if m.scope_type != "private" {
                            format!(" [{}]", m.scope_type)
                        } else {
                            String::new()
                        };
                        format!("- [{}]{} {}", m.category, scope_tag, m.content)
                    })
                    .collect();
                format!("\n\n## Your Memories\n{}", items.join("\n"))
            }
        };

        // Inject recent pool chat messages as context
        let pool_chat_ctx = if let Some(ref psid) = pool_session_id {
            let db = state.db.lock().await;
            let messages = db.get_pool_messages(psid, 20, 0).unwrap_or_default();
            if messages.is_empty() {
                String::new()
            } else {
                let kois = db.list_kois().unwrap_or_default();
                let koi_names: std::collections::HashMap<String, String> = kois
                    .iter()
                    .map(|k| (k.id.clone(), format!("{} {}", k.icon, k.name)))
                    .collect();
                let lines: Vec<String> = messages
                    .iter()
                    .map(|m| {
                        let sender = koi_names
                            .get(&m.sender_id)
                            .cloned()
                            .unwrap_or_else(|| m.sender_id.clone());
                        let time = m.created_at.format("%m-%d %H:%M").to_string();
                        let content = if m.content.chars().count() > 300 {
                            format!("{}...", m.content.chars().take(300).collect::<String>())
                        } else {
                            m.content.clone()
                        };
                        format!("[{}] {} ({}): {}", time, sender, m.msg_type, content)
                    })
                    .collect();
                format!("\n\n## Recent Pool Chat\n{}", lines.join("\n"))
            }
        } else {
            String::new()
        };

        let system_prompt = format!(
            "{}\n\nYou are {} ({}). You have your own independent memory and full tool access.\
             When you learn something important, use memory_store to save it.{}{}{}\
             \n\n## Collaboration Rules\n\
             - You are an autonomous agent participating in a project pool.\n\
             - Use pool_chat(action=\"read\") to see what your team members have said and done.\n\
             - Use pool_chat(action=\"send\") to share your progress, results, and discussions with the team.\n\
             - When you complete a task, post your output to pool_chat. If another Koi should act next, @mention them — e.g. \"@Reviewer please check my implementation\".\n\
             - If someone @mentions you with a concrete request or task handoff, use pool_chat to respond — accept, discuss, or decline.\n\
             - If someone @mentions you only to share a status update, say the project is done, or acknowledge your work — you do NOT need to reply. Avoid sending acknowledgement-only messages like \"noted\", \"great job\", or \"I agree, project is done\". These add noise and can trigger unnecessary reply chains.\n\
             - You may @mention other Koi in pool_chat when you need their input or want to hand off work that requires their action. Do NOT @mention someone just to inform them the project is done — simply signal @pisci instead.\n\
             - Only Pisci or the user can directly assign tasks to you. Other Koi can request via @mention.\n\
             - No fixed role decides when a project is finished. If more work is needed, clearly hand off with @mentions and keep the project moving. If you believe the project may be ready to wrap up, signal @pisci — do not unilaterally declare the project complete, and do not @mention peer Koi to get their agreement.\n\
             - If you are working in a Git worktree, your file changes are local to your branch (shown in [Environment] above). Do not run git commands manually — commits and cleanup are handled automatically.\n\
             - To see what other Koi are working on or have completed, use pool_org(action=\"get_todos\", pool_id=\"...\") or pool_chat(action=\"read\").\n\
             - Focus on your assigned scope. Do not modify files outside the directories relevant to your task.\n\
             \n\n## Task Lifecycle\n\
             - When you finish a task, mark it complete: pool_org(action=\"complete_todo\", todo_id=\"...\"). Always do this — leaving tasks unmarked pollutes the kanban board.\n\
             - If you realize a task is no longer needed, cancel your own: pool_org(action=\"cancel_todo\", todo_id=\"...\", reason=\"...\").\n\
             - You can ONLY complete or cancel your own tasks. To request cancellation of another agent's task, @pisci in pool_chat and explain why.\n\
             - If your task is blocked, update its status: pool_org(action=\"update_todo_status\", todo_id=\"...\", status=\"blocked\") and notify @pisci.\n\
             - Use pool_org(action=\"get_todos\", pool_id=\"...\") to see the current task board before starting work.\n\
             - In significant pool_chat updates, prefer including a structured status signal so Pisci can reason about project state: `[ProjectStatus] follow_up_needed`, `[ProjectStatus] waiting`, or `[ProjectStatus] ready_for_pisci_review`.\n\
             - Use `[ProjectStatus] follow_up_needed` when another agent must continue the work. @mention the next agent or @pisci.\n\
             - Use `[ProjectStatus] ready_for_pisci_review` only when you believe your branch of work has no remaining known follow-up and Pisci should decide whether the project can conclude.\n\
             \n\n## Knowledge Base (kb/)\n\
             - The workspace contains a shared `kb/` directory for accumulated project knowledge. Always check it before starting work: use file_list to browse `<workspace>/kb/`, then file_read to read relevant files.\n\
             - When you discover something worth preserving — architecture decisions, API specs, tricky bugs, lessons learned, useful data — write it to `kb/`. Use subdirectories to organize: `kb/decisions/`, `kb/architecture/`, `kb/api/`, `kb/bugs/`, `kb/research/`.\n\
             - Format: use `.md` for human-readable notes and specs; use `.jsonl` for structured records (one JSON object per line, append-only). Name files descriptively, e.g. `kb/decisions/2024-auth-strategy.md` or `kb/bugs/known-issues.jsonl`.\n\
             - For `.jsonl` entries, always include `timestamp`, `author` (your name), and a `summary` field so entries are self-describing.\n\
             - The `kb/` directory is shared across all agents and persists across sessions — treat it as institutional memory.",
            koi_def.system_prompt, koi_def.name, koi_def.icon,
            memory_context, org_spec_ctx, pool_chat_ctx
        );

        let llm_messages = vec![LlmMessage {
            role: "user".into(),
            content: MessageContent::text(&task),
        }];

        // Read settings, applying per-Koi LLM provider override when configured
        let (
            provider,
            model,
            api_key,
            base_url,
            workspace_root,
            max_tokens,
            policy_mode,
            tool_rate_limit_per_minute,
            tool_settings,
            builtin_tool_enabled,
            allow_outside_workspace,
            vision_enabled,
        ) = {
            let settings = state.settings.lock().await;
            // Resolve per-Koi LLM provider: if the koi has a provider_id and it exists in settings, use it
            let (provider, model, api_key, base_url, max_tokens) =
                if let Some(ref pid) = koi_def.llm_provider_id {
                    if let Some(p) = settings.find_llm_provider(pid) {
                        let key = p.api_key.clone();
                        let mt = if p.max_tokens > 0 {
                            p.max_tokens
                        } else {
                            settings.max_tokens
                        };
                        (
                            p.provider.clone(),
                            p.model.clone(),
                            key,
                            p.base_url.clone(),
                            mt,
                        )
                    } else {
                        // Provider id set but not found — fall back to global
                        (
                            settings.provider.clone(),
                            settings.model.clone(),
                            settings.active_api_key().to_string(),
                            settings.custom_base_url.clone(),
                            settings.max_tokens,
                        )
                    }
                } else {
                    (
                        settings.provider.clone(),
                        settings.model.clone(),
                        settings.active_api_key().to_string(),
                        settings.custom_base_url.clone(),
                        settings.max_tokens,
                    )
                };
            (
                provider,
                model,
                api_key,
                base_url,
                settings.workspace_root.clone(),
                max_tokens,
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

        let cancel = Arc::new(AtomicBool::new(false));

        // Mark Koi as busy only after we know execution can actually start.
        if !self.managed_externally {
            {
                let db = state.db.lock().await;
                let _ = db.update_koi_status(&koi_id, "busy");
            }
            let _ = state.app_handle.emit(
                "koi_status_changed",
                json!({ "id": koi_id, "status": "busy" }),
            );
        }

        // Record task assignment in Chat Pool
        if !self.managed_externally {
            let caller_id = self.caller_koi_id.as_deref().unwrap_or("pisci");
            if let Some(ref pool_sid) = pool_session_id {
                let db = state.db.lock().await;
                let _ = db.insert_pool_message(
                    pool_sid,
                    caller_id,
                    &format!("@{} {}", koi_def.name, task),
                    "task_assign",
                    &json!({ "koi_id": &koi_id, "task": task }).to_string(),
                );
            }
        }

        // Register cancel flag so cancel_koi_task can find it.
        // Include pool_session_id so the same Koi can be cancelled per-project independently.
        let cancel_key = format!(
            "koi_{}_{}",
            koi_id,
            pool_session_id.as_deref().unwrap_or("default")
        );
        {
            let mut flags = state.cancel_flags.lock().await;
            flags.insert(cancel_key.clone(), cancel.clone());
        }

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
        let app_data_dir = self.app.path().app_data_dir().ok();
        // Load skill_loader so Koi can use skill_search (same as Pisci)
        let skill_loader = app_data_dir.as_ref().map(|d| {
            let loader = crate::skills::loader::SkillLoader::new(d.join("skills"));
            let mut l = loader;
            let _ = l.load_all();
            std::sync::Arc::new(tokio::sync::Mutex::new(l))
        });
        let mut registry_tools = crate::tools::build_registry(
            state.browser.clone(),
            user_tools_dir.as_deref(),
            Some(state.db.clone()),
            Some(&builtin_tool_enabled),
            Some(self.app.clone()),
            Some(state.settings.clone()),
            app_data_dir,
            skill_loader,
        );
        // Replace the default call_koi (depth=0) with one scoped to this Koi
        registry_tools.unregister("call_koi");
        registry_tools.unregister("pool_chat");
        if self.depth + 1 < MAX_CALL_DEPTH {
            registry_tools.register(Box::new(CallKoiTool {
                app: self.app.clone(),
                caller_koi_id: Some(koi_id.clone()),
                depth: self.depth + 1,
                managed_externally: false,
                notification_rx: std::sync::Mutex::new(None),
            }));
        }

        // Register pool_chat tool scoped to this Koi's identity
        registry_tools.register(Box::new(crate::tools::pool_chat::PoolChatTool {
            app: self.app.clone(),
            db: state.db.clone(),
            sender_id: koi_id.clone(),
            sender_name: koi_def.name.clone(),
        }));

        let registry_tools = Arc::new(registry_tools);

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
            notification_rx: self
                .notification_rx
                .lock()
                .unwrap()
                .take()
                .map(|rx| tokio::sync::Mutex::new(rx)),
        };

        let koi_ctx = ToolContext {
            // Include pool_session_id so each project gets an isolated session context
            session_id: format!(
                "koi_{}_{}",
                koi_id,
                pool_session_id.as_deref().unwrap_or("default")
            ),
            workspace_root: std::path::PathBuf::from(&workspace_root),
            bypass_permissions: false,
            settings: tool_settings,
            max_iterations: Some(30),
            memory_owner_id: koi_id.clone(),
            pool_session_id: pool_session_id.clone(),
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
                    AgentEvent::ToolStart { name, .. } => Some(AgentEvent::FishProgress {
                        fish_id: koi_id_fwd.clone(),
                        fish_name: koi_name_fwd.clone(),
                        iteration,
                        tool_name: Some(name.clone()),
                        status: "tool_call".to_string(),
                    }),
                    AgentEvent::ToolEnd { name, .. } => Some(AgentEvent::FishProgress {
                        fish_id: koi_id_fwd.clone(),
                        fish_name: koi_name_fwd.clone(),
                        iteration,
                        tool_name: Some(name.clone()),
                        status: "tool_done".to_string(),
                    }),
                    AgentEvent::Done { .. } => Some(AgentEvent::FishProgress {
                        fish_id: koi_id_fwd.clone(),
                        fish_name: koi_name_fwd.clone(),
                        iteration,
                        tool_name: None,
                        status: "done".to_string(),
                    }),
                    _ => None,
                };
                if let Some(prog) = progress {
                    let prog_payload = serde_json::to_value(&prog).unwrap_or_default();
                    let _ = app_fwd.emit(&format!("agent_event_{}", parent_sid), prog_payload);
                }
            }
        });

        let run_result = match tokio::time::timeout(
            std::time::Duration::from_secs(600),
            agent.run(llm_messages, event_tx, cancel, koi_ctx),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!(
                "Koi '{}' timed out after 10 minutes on task: {}",
                koi_def.name,
                task
            )),
        };
        let _ = forward_handle.await;

        // Clean up cancel flag
        {
            let mut flags = state.cancel_flags.lock().await;
            flags.remove(&cancel_key);
        }

        // Mark Koi as idle
        if !self.managed_externally {
            {
                let db = state.db.lock().await;
                let _ = db.update_koi_status(&koi_id, "idle");
            }
            let _ = state.app_handle.emit(
                "koi_status_changed",
                json!({ "id": koi_id, "status": "idle" }),
            );
        }

        match run_result {
            Ok((final_msgs, _, _)) => {
                let reply = final_msgs
                    .iter()
                    .rev()
                    .find(|m| m.role == "assistant")
                    .map(|m| m.content.as_text())
                    .unwrap_or_default();

                // Record result in Chat Pool
                if !self.managed_externally {
                    if let Some(ref pool_sid) = pool_session_id {
                        let db = state.db.lock().await;
                        let summary = if reply.chars().count() > 500 {
                            reply.chars().take(500).collect::<String>()
                        } else {
                            reply.clone()
                        };
                        let _ = db.insert_pool_message(pool_sid, &koi_id, &summary, "result", "{}");
                    }
                }

                let summary = if reply.chars().count() > 2000 {
                    format!(
                        "{}...\n[truncated, {} chars total]",
                        reply.chars().take(2000).collect::<String>(),
                        reply.chars().count()
                    )
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
                if !self.managed_externally {
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
                }
                Ok(ToolResult::err(format!(
                    "Koi '{}' failed: {}",
                    koi_def.name, e
                )))
            }
        }
    }
}
