//! call_koi tool — lets Pisci or another Koi delegate a task to a
//! persistent Koi agent.
//!
//! This module bundles the in-process desktop path:
//! - [`CallKoiTool`] and its [`Tool`] impl (this file, below)
//! - [`runtime`] — slot / soft-fence / Koi-session plumbing
//! - [`event_bus`] — `AgentEvent` publisher that reaches the Tauri UI
//!
//! Callers go through [`CallKoiTool`]; [`runtime`] and [`event_bus`]
//! are crate-internal glue used by the tool and its reconciliation path.

pub mod event_bus;
pub mod runtime;

use crate::commands::config::scene::{
    build_registry_for_scene, load_skill_loader, SceneKind, ScenePolicy,
};
use crate::store::db::TaskSpine;
use crate::store::AppState;
use async_trait::async_trait;
use pisci_core::project_state::build_coordination_event_digest;
/// call_koi tool — lets Pisci or another Koi delegate a task to a persistent Koi agent.
///
/// Unlike call_fish (stateless), call_koi:
/// - Loads the Koi's private memories before execution
/// - Persists the Koi's conversation to the DB
/// - Records messages in the Chat Pool (if a pool_session_id is provided)
/// - Sets memory_owner_id so new memories are scoped to the Koi
/// - Allows the Koi to call other Kois (excluding itself, to prevent recursion)
use pisci_kernel::agent::harness::HarnessConfig;
use pisci_kernel::agent::messages::AgentEvent;
use pisci_kernel::agent::tool::{Tool, ToolContext, ToolResult, ToolSettings};
use pisci_kernel::llm::{LlmMessage, MessageContent};
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
    /// When true, `call()` will run the agent inline (awaiting `agent.run()`
    /// and the follow-up reconcile) instead of spawning a background task and
    /// returning immediately. Used by the soft-fence retry path so the caller
    /// can synchronously wait for the retry's outcome before deciding whether
    /// the hard fence needs to fire.
    pub await_completion: bool,
}

const MAX_CALL_DEPTH: u32 = 5;

fn truncate_chars(content: &str, max_chars: usize) -> String {
    if max_chars == 0 || content.chars().count() <= max_chars {
        return content.to_string();
    }
    format!("{}...", content.chars().take(max_chars).collect::<String>())
}

fn koi_continuity_scope_id(koi_id: &str, pool_session_id: Option<&str>) -> String {
    format!("{}::{}", koi_id, pool_session_id.unwrap_or("default"))
}

fn koi_continuity_context(task_state: &crate::store::db::TaskState) -> String {
    crate::commands::chat::render_task_state_section(
        "Your Recent Working Context",
        "Most Recent Outcome",
        task_state,
    )
}

fn build_koi_continuity_spine(task: &str, outcome: &str, success: bool) -> TaskSpine {
    let mut spine = TaskSpine {
        goal: truncate_chars(task.trim(), 500),
        current_step: if success {
            "Continue from the latest validated outcome below.".to_string()
        } else {
            "The previous run did not complete cleanly; inspect blockers before proceeding."
                .to_string()
        },
        ..TaskSpine::default()
    };
    let outcome_line = truncate_chars(outcome.trim(), 600);
    if success {
        if !outcome_line.is_empty() {
            spine
                .done
                .push(format!("Last run outcome: {}", outcome_line));
        }
    } else if !outcome_line.is_empty() {
        spine.blockers.push(outcome_line);
    }
    spine
}

fn persist_koi_continuity_state(
    db: &crate::store::db::Database,
    koi_id: &str,
    pool_session_id: Option<&str>,
    task: &str,
    outcome: &str,
    success: bool,
) {
    let scope_id = koi_continuity_scope_id(koi_id, pool_session_id);
    if let Ok(state) = db.get_or_create_task_state("koi_session", &scope_id) {
        let spine = build_koi_continuity_spine(task, outcome, success);
        let summary = if success {
            truncate_chars(outcome.trim(), 240)
        } else {
            format!("Last run failed: {}", truncate_chars(outcome.trim(), 220))
        };
        let status = if success { "active" } else { "blocked" };
        let state_json = serde_json::to_string(&spine).unwrap_or_else(|_| "{}".into());
        let _ = db.update_task_state(
            &state.id,
            Some(&spine.goal),
            Some(&state_json),
            Some(&summary),
            Some(status),
        );
    }
}

// The Koi system prompt is assembled by `pisci_core::koi_prompt` as a fixed
// 6-layer structure (Identity → Run Shape → Coordination → Context & Tools →
// Capabilities → Stop Gate). The contract lives in pisci-core because it is
// pure coordination logic and is unit-tested there. Desktop only supplies the
// identity preamble and dynamic context slices and calls
// `build_koi_task_system_prompt` below.
use pisci_core::koi_prompt::build_koi_task_system_prompt;

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
         IMPORTANT: call_koi is NON-BLOCKING. The Koi runs in the background independently. \
         You do NOT wait for the Koi to finish — return to the user immediately after assigning tasks. \
         The Koi will post results to the pool chat when done. \
         \
         Actions: \
         - 'list': List all available Koi agents. \
         - 'call': Assign a task to a Koi. The Koi starts immediately in the background. \
         Provide a complete, self-contained task description. The Koi will use its own memory and tools."
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
        let scene_policy = ScenePolicy::for_kind(SceneKind::KoiTask);

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
                        Some(format!(
                            "\n\n## Project Organization\n{}",
                            truncate_chars(
                                &session.org_spec,
                                scene_policy.org_spec_preview_chars()
                            )
                        ))
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

        let continuity_context = {
            let scope_id = koi_continuity_scope_id(&koi_id, pool_session_id.as_deref());
            let db = state.db.lock().await;
            db.load_task_state("koi_session", &scope_id)
                .ok()
                .flatten()
                .map(|state| koi_continuity_context(&state))
                .unwrap_or_default()
        };

        // Load Koi's scoped memories for context injection
        let memory_context = {
            let db = state.db.lock().await;
            let mut sections = Vec::new();
            let koi_memories = db
                .search_memories_scoped(&task, &koi_id, pool_session_id.as_deref(), 5)
                .unwrap_or_default();
            if !koi_memories.is_empty() {
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
                sections.push(format!("\n\n## Your Memories\n{}", items.join("\n")));
            }
            if matches!(
                scene_policy.memory_slice_mode(),
                crate::commands::config::scene::MemorySliceMode::ScopedPlusRecent
            ) {
                let recent_items = db
                    .list_memories_for_owner(&koi_id)
                    .unwrap_or_default()
                    .into_iter()
                    .take(3)
                    .map(|m| format!("- [{}] {}", m.category, truncate_chars(&m.content, 180)))
                    .collect::<Vec<_>>();
                if !recent_items.is_empty() {
                    sections.push(format!(
                        "\n\n## Recently Saved Memory\n{}",
                        recent_items.join("\n")
                    ));
                }
            }
            sections.join("")
        };

        // Inject structured coordination digest plus a tiny raw tail for local grounding.
        let pool_chat_ctx = if let Some(ref psid) = pool_session_id {
            let db = state.db.lock().await;
            let messages = db
                .get_pool_messages(psid, scene_policy.recent_pool_message_limit() as i64 * 2, 0)
                .unwrap_or_default();
            if messages.is_empty() {
                String::new()
            } else {
                let kois = db.list_kois().unwrap_or_default();
                let koi_names: std::collections::HashMap<String, String> = kois
                    .iter()
                    .map(|k| (k.id.clone(), format!("{} {}", k.icon, k.name)))
                    .collect();
                let digest = build_coordination_event_digest(
                    &messages,
                    scene_policy.event_digest_mode(),
                    &[koi_def.name.as_str()],
                    scene_policy.recent_pool_message_limit(),
                    scene_policy.recent_pool_message_chars(),
                );
                let raw_lines: Vec<String> = messages
                    .iter()
                    .rev()
                    .take(3)
                    .rev()
                    .map(|m| {
                        let sender = koi_names
                            .get(&m.sender_id)
                            .cloned()
                            .unwrap_or_else(|| m.sender_id.clone());
                        let time = m.created_at.format("%m-%d %H:%M").to_string();
                        let content =
                            truncate_chars(&m.content, scene_policy.recent_pool_message_chars());
                        format!("[{}] {} ({}): {}", time, sender, m.msg_type, content)
                    })
                    .collect();
                let mut section = String::new();
                if !digest.lines.is_empty() {
                    section.push_str("\n\n## Coordination Event Digest\n");
                    section.push_str(&digest.lines.join("\n"));
                }
                if !raw_lines.is_empty() {
                    section.push_str("\n\n## Latest Raw Pool Messages\n");
                    section.push_str(&raw_lines.join("\n"));
                }
                section
            }
        } else {
            String::new()
        };

        // Board state: inject current todo summary so the Koi knows
        // who is working on what, without needing to call get_todos first.
        let board_state_ctx = if let Some(ref psid) = pool_session_id {
            let db = state.db.lock().await;
            let all_todos = db.list_koi_todos(None).unwrap_or_default();
            let pool_todos: Vec<_> = all_todos
                .iter()
                .filter(|t| t.pool_session_id.as_deref() == Some(psid.as_str()))
                .collect();
            if pool_todos.is_empty() {
                String::new()
            } else {
                let lines: Vec<String> = pool_todos
                    .iter()
                    .map(|t| {
                        let marker = match t.status.as_str() {
                            "in_progress" => "🔄",
                            "todo" => "📋",
                            "done" => "✅",
                            "blocked" => "🚫",
                            _ => "❓",
                        };
                        format!(
                            "{} [{}] {} — \"{}\"",
                            marker,
                            t.status,
                            t.owner_id,
                            t.title.chars().take(80).collect::<String>()
                        )
                    })
                    .collect();
                format!("\n\n## Current Board State\n{}", lines.join("\n"))
            }
        } else {
            String::new()
        };

        // kb/ directory listing: hint at shared project knowledge.
        let kb_ctx = {
            let ws = {
                let settings = state.settings.lock().await;
                settings.workspace_root.clone()
            };
            let kb_path = std::path::Path::new(&ws).join("kb");
            if kb_path.exists() {
                let entries: Vec<String> = std::fs::read_dir(&kb_path)
                    .unwrap_or_else(|_| std::fs::read_dir(std::path::Path::new(&ws)).unwrap())
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .take(30)
                    .collect();
                if entries.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\n\n## Project Knowledge Base (kb/)\nAvailable directories: {}\nRead relevant files before starting work.",
                        entries.join(", ")
                    )
                }
            } else {
                String::new()
            }
        };

        let assignment_ctx = {
            let trimmed = task.trim();
            if trimmed.is_empty() {
                String::new()
            } else {
                let clipped = if trimmed.chars().count() > 2400 {
                    format!("{}...", trimmed.chars().take(2400).collect::<String>())
                } else {
                    trimmed.to_string()
                };
                format!(
                    "\n\n## Current Assignment\n{}\n\
                     - This assignment remains your active contract for the entire run.\n\
                     - Keep it aligned with the latest relevant pool_chat evidence.\n\
                     - Do not let exploratory tool use, repeated planning, or repeated notifications replace the actual deliverable, handoff target, or completion condition stated here.",
                    clipped
                )
            }
        };

        // Merge board_state and kb/ into the pool_chat_ctx so they
        // appear in the project-environment layer without changing the
        // build_koi_task_system_prompt signature.
        let combined_env_ctx = format!("{}{}{}", board_state_ctx, kb_ctx, pool_chat_ctx);

        let system_prompt = build_koi_task_system_prompt(
            &koi_def.system_prompt,
            &koi_def.name,
            &koi_def.icon,
            &continuity_context,
            &memory_context,
            &org_spec_ctx,
            &combined_env_ctx,
            &assignment_ctx,
        );
        tracing::info!(
            "koi_context_slices koi={} pool={} history_mode={:?} memory_mode={:?} event_digest_mode={:?} continuity_chars={} memory_chars={} pool_chars={} assignment_chars={} system_prompt_chars={}",
            koi_id,
            pool_session_id.as_deref().unwrap_or("default"),
            scene_policy.history_slice_mode(),
            scene_policy.memory_slice_mode(),
            scene_policy.event_digest_mode(),
            continuity_context.chars().count(),
            memory_context.chars().count(),
            pool_chat_ctx.chars().count(),
            assignment_ctx.chars().count(),
            system_prompt.chars().count(),
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
            context_window,
            policy_mode,
            tool_rate_limit_per_minute,
            tool_settings,
            builtin_tool_enabled,
            allow_outside_workspace,
            vision_enabled,
            vision_use_main_llm,
            vision_provider,
            vision_model,
            vision_api_key,
            auto_compact_input_tokens_threshold,
            loop_max_iterations,
        ) = {
            let settings = state.settings.lock().await;
            // Resolve per-Koi LLM provider: if the koi has a provider_id and it exists in settings, use it
            let (provider, model, api_key, base_url, max_tokens) =
                if let Some(ref pid) = koi_def.llm_provider_id {
                    if let Some(p) = settings.find_llm_provider(pid) {
                        let key = p.effective_api_key().to_string();
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
            // Koi always works within a project scope.
            // allow_outside_workspace is a Pisci (general assistant) setting.
            // When a pool_session_id is present (project context), Koi is confined to the
            // project workspace regardless of the global setting.
            // Only when called with no project context (rare, ad-hoc) does it inherit the setting.
            let koi_allow_outside = if pool_session_id.is_some() {
                false
            } else {
                settings.allow_outside_workspace
            };
            (
                provider,
                model,
                api_key,
                base_url,
                settings.workspace_root.clone(),
                max_tokens,
                settings.context_window,
                settings.policy_mode.clone(),
                settings.tool_rate_limit_per_minute,
                Arc::new(ToolSettings::from_settings(&settings)),
                settings.builtin_tool_enabled.clone(),
                koi_allow_outside,
                settings.vision_enabled,
                settings.vision_use_main_llm,
                settings.vision_provider.clone(),
                settings.vision_model.clone(),
                settings.vision_api_key.clone(),
                settings.auto_compact_input_tokens_threshold,
                if koi_def.max_iterations > 0 {
                    Some(koi_def.max_iterations)
                } else if settings.max_iterations > 0 {
                    Some(settings.max_iterations)
                } else {
                    None
                },
            )
        };

        if api_key.is_empty() {
            return Ok(ToolResult::err("API key not configured"));
        }

        let vision_capable = if vision_use_main_llm {
            vision_enabled || crate::commands::chat::model_supports_vision(&provider, &model)
        } else {
            !vision_provider.is_empty() && !vision_model.is_empty() && !vision_api_key.is_empty()
        };

        let cancel = Arc::new(AtomicBool::new(false));

        // Mark Koi as busy only after we know execution can actually start.
        if !self.managed_externally {
            let acquired = crate::tools::call_koi::runtime::try_acquire_managed_run_slot(
                &state.app_handle,
                &state.db,
                &koi_id,
                pool_session_id.as_deref(),
            )
            .await;
            if !acquired {
                return Ok(ToolResult::ok(format!(
                    "Koi '{}' is already active for this pool. Let the existing run finish before delegating more work.",
                    koi_def.name
                )));
            }
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

        let client = pisci_kernel::llm::build_client(
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
        let skill_loader = load_skill_loader(&self.app);
        let mut registry_tools = build_registry_for_scene(
            SceneKind::KoiTask,
            state.browser.clone(),
            user_tools_dir.as_deref(),
            Some(state.db.clone()),
            Some(&builtin_tool_enabled),
            Some(self.app.clone()),
            Some(state.settings.clone()),
            app_data_dir,
            skill_loader,
        )
        .await;
        // Replace the default call_koi (depth=0) with one scoped to this Koi.
        // The neutral kernel `pool_chat` tool already picks up the Koi's
        // identity from `ToolContext::memory_owner_id`, so we no longer
        // need to re-register a sender-scoped copy here.
        registry_tools.unregister("call_koi");
        if self.depth + 1 < MAX_CALL_DEPTH {
            registry_tools.register(Box::new(CallKoiTool {
                app: self.app.clone(),
                caller_koi_id: Some(koi_id.clone()),
                depth: self.depth + 1,
                managed_externally: false,
                notification_rx: std::sync::Mutex::new(None),
                await_completion: false,
            }));
        }

        let registry_tools = Arc::new(registry_tools);

        let policy = Arc::new(pisci_kernel::policy::PolicyGate::with_profile_and_flags(
            &workspace_root,
            &policy_mode,
            tool_rate_limit_per_minute,
            allow_outside_workspace,
        ));

        // Per-run plumbing kept at the call site: the cross-session
        // notification channel is *taken* out of `self` once and handed
        // to the bridge so the AgentLoop receives ownership.
        let notification_rx = self.notification_rx.lock().unwrap().take();
        let koi_compaction_settings = {
            let s = state.settings.lock().await;
            pisci_kernel::agent::harness::config::CompactionSettings::from_settings(&s)
        };
        let agent = HarnessConfig::for_koi(
            model,
            vec![],
            registry_tools,
            policy,
            system_prompt,
            max_tokens,
            context_window,
            Some(vision_capable),
            scene_policy.effective_auto_compact_threshold(auto_compact_input_tokens_threshold),
            koi_compaction_settings,
            Some(state.db.clone()),
            Some(state.plan_state.clone()),
        )
        .into_agent_loop(client, notification_rx, None);

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
            // Koi-specific max_iterations overrides the user-configurable system default.
            max_iterations: loop_max_iterations,
            memory_owner_id: koi_id.clone(),
            pool_session_id: pool_session_id.clone(),
            cancel: cancel.clone(),
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
                            text_delta: None,
                        })
                    }
                    AgentEvent::ToolStart { name, .. } => Some(AgentEvent::FishProgress {
                        fish_id: koi_id_fwd.clone(),
                        fish_name: koi_name_fwd.clone(),
                        iteration,
                        tool_name: Some(name.clone()),
                        status: "tool_call".to_string(),
                        text_delta: None,
                    }),
                    AgentEvent::ToolEnd { name, .. } => Some(AgentEvent::FishProgress {
                        fish_id: koi_id_fwd.clone(),
                        fish_name: koi_name_fwd.clone(),
                        iteration,
                        tool_name: Some(name.clone()),
                        status: "tool_done".to_string(),
                        text_delta: None,
                    }),
                    AgentEvent::Done { .. } => Some(AgentEvent::FishProgress {
                        fish_id: koi_id_fwd.clone(),
                        fish_name: koi_name_fwd.clone(),
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

        // Spawn Koi in the background so Pisci is not blocked.
        // The user can stop Pisci's conversation without interrupting the Koi.
        // Exception: when `await_completion` is true, the caller explicitly
        // wants to synchronously observe the agent's outcome (e.g., the
        // soft-fence retry path in KoiRuntime).
        let managed_externally = self.managed_externally;
        let await_completion = self.await_completion;
        let koi_timeout_secs = {
            let settings = state.settings.lock().await;
            settings.koi_timeout_secs.max(60) as u64
        };
        let app_bg = state.app_handle.clone();
        let db_bg = state.db.clone();
        let cancel_flags_bg = state.cancel_flags.clone();
        let koi_name_bg = koi_def.name.clone();
        let _koi_icon_bg = koi_def.icon.clone();
        let koi_id_bg = koi_id.clone();
        let pool_session_id_bg = pool_session_id.clone();
        let cancel_key_bg = cancel_key.clone();
        let task_bg = task.clone();

        let run_future = async move {
            let run_result = match tokio::time::timeout(
                std::time::Duration::from_secs(koi_timeout_secs),
                agent.run(llm_messages, event_tx, cancel, koi_ctx),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!(
                    "Koi '{}' timed out after {} seconds on task: {}",
                    koi_name_bg,
                    koi_timeout_secs,
                    task_bg
                )),
            };
            let _ = forward_handle.await;

            // Clean up cancel flag
            {
                let mut flags = cancel_flags_bg.lock().await;
                flags.remove(&cancel_key_bg);
            }

            // Mark Koi as idle
            if !managed_externally {
                crate::tools::call_koi::runtime::release_managed_run_slot(
                    &app_bg,
                    &db_bg,
                    &koi_id_bg,
                    pool_session_id_bg.as_deref(),
                )
                .await;
            }

            match run_result {
                Ok((final_msgs, _, _)) => {
                    let llm_reply = final_msgs
                        .iter()
                        .rev()
                        .find(|m| m.role == "assistant")
                        .map(|m| m.content.as_text())
                        .unwrap_or_default();

                    let reply = if llm_reply.trim().is_empty() {
                        if let Some(ref pool_sid) = pool_session_id_bg {
                            let db = db_bg.lock().await;
                            db.get_latest_result_message(pool_sid, &koi_id_bg)
                                .unwrap_or_default()
                                .unwrap_or_default()
                        } else {
                            llm_reply
                        }
                    } else {
                        llm_reply
                    };

                    {
                        let db = db_bg.lock().await;
                        persist_koi_continuity_state(
                            &db,
                            &koi_id_bg,
                            pool_session_id_bg.as_deref(),
                            &task_bg,
                            &reply,
                            true,
                        );
                    }

                    if let Some(ref pool_sid) = pool_session_id_bg {
                        if managed_externally {
                            crate::tools::call_koi::runtime::reconcile_managed_pool_completion(
                                &app_bg,
                                &db_bg,
                                pool_sid,
                                &koi_id_bg,
                                &koi_name_bg,
                                &reply,
                                true,
                            )
                            .await;
                        } else if !reply.trim().is_empty() {
                            let db = db_bg.lock().await;
                            let already_recorded = db
                                .get_latest_result_message(pool_sid, &koi_id_bg)
                                .ok()
                                .flatten()
                                .map(|m| m == reply)
                                .unwrap_or(false);
                            if !already_recorded {
                                let _ = db.insert_pool_message(
                                    pool_sid, &koi_id_bg, &reply, "result", "{}",
                                );
                            }
                        }
                    }

                    tracing::info!("call_koi background: Koi '{}' completed task", koi_name_bg);
                }
                Err(e) => {
                    tracing::warn!("call_koi background: Koi '{}' failed: {}", koi_name_bg, e);
                    {
                        let db = db_bg.lock().await;
                        persist_koi_continuity_state(
                            &db,
                            &koi_id_bg,
                            pool_session_id_bg.as_deref(),
                            &task_bg,
                            &e.to_string(),
                            false,
                        );
                    }
                    if let Some(ref pool_sid) = pool_session_id_bg {
                        if managed_externally {
                            crate::tools::call_koi::runtime::reconcile_managed_pool_completion(
                                &app_bg,
                                &db_bg,
                                pool_sid,
                                &koi_id_bg,
                                &koi_name_bg,
                                &e.to_string(),
                                false,
                            )
                            .await;
                        } else {
                            let db = db_bg.lock().await;
                            let _ = db.insert_pool_message(
                                pool_sid,
                                &koi_id_bg,
                                &format!("Task failed: {}", e),
                                "status_update",
                                "{}",
                            );
                        }
                    }
                }
            }
        };

        if await_completion {
            // Inline: caller (typically the soft-fence retry) wants to await
            // the agent's full completion AND its reconcile before returning.
            run_future.await;
            Ok(ToolResult::ok(format!(
                "Koi '{}' {} has completed the task synchronously.\n\nTask: {}",
                koi_def.name, koi_def.icon, task
            )))
        } else {
            tokio::spawn(run_future);
            // Return immediately — Koi is running in the background.
            // Results will appear in the pool chat when the Koi completes.
            let pool_hint = if pool_session_id.is_some() {
                " Results will appear in the pool chat when done."
            } else {
                ""
            };
            Ok(ToolResult::ok(format!(
                "Koi '{}' {} has been assigned the task and is now working in the background.{}\n\nTask: {}",
                koi_def.name, koi_def.icon, pool_hint, task
            )))
        }
    }
}
