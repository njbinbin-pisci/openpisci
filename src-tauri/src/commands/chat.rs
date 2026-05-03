// Chat-domain submodules (registered as Tauri commands by `app::bootstrap`).
pub mod collab_trial;
pub mod debug;
pub mod fish;
pub mod gateway;
pub mod scheduler;

use crate::commands::config::scene::{
    build_registry_for_scene, load_skill_loader, HistorySliceMode, MemorySliceMode,
    PoolSnapshotMode, SceneKind, ScenePolicy,
};
use crate::store::{
    db::ChatMessage, db::Session, db::SessionContextState, db::TaskSpine, db::TaskState, AppState,
};
use pisci_core::project_state::build_coordination_event_digest;
use pisci_kernel::agent::messages::AgentEvent;
use pisci_kernel::agent::plan::summarize_todos;
use pisci_kernel::agent::tool::ToolContext;
use pisci_kernel::llm::{
    build_client_with_timeout, ContentBlock, LlmMessage, MessageContent, ToolDef,
};
use pisci_kernel::policy::PolicyGate;
use pisci_kernel::project_context::render_project_instruction_context;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{atomic::AtomicBool, Arc};
use tauri::{AppHandle, Emitter, Manager, State};

/// Attachment sent from the frontend with a chat message.
/// Either `path` (local file path) or `data` (base64-encoded bytes) must be provided.
#[derive(Debug, Clone, Deserialize)]
pub struct FrontendAttachment {
    /// MIME type, e.g. "image/png", "application/pdf"
    pub media_type: String,
    /// Local file path (preferred for non-image files or non-vision models)
    pub path: Option<String>,
    /// Base64-encoded file data (used for images with vision models)
    pub data: Option<String>,
    /// Original filename
    pub filename: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionList {
    pub sessions: Vec<Session>,
    pub total: usize,
}

struct SessionMessageContext {
    llm_messages: Vec<LlmMessage>,
    session_state: Option<SessionContextState>,
    latest_user_text: String,
    tool_minimals: HashMap<String, String>,
}

struct ChatPromptArtifacts {
    system_prompt: String,
    registry: Arc<pisci_kernel::agent::tool::ToolRegistry>,
    tool_defs: Vec<ToolDef>,
}

fn append_task_spine_list(ctx: &mut String, label: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    ctx.push_str(&format!("**{}:**\n", label));
    for item in items.iter().take(4) {
        ctx.push_str(&format!("- {}\n", item));
    }
}

pub(crate) fn render_task_state_section(
    title: &str,
    progress_label: &str,
    task_state: &TaskState,
) -> String {
    let spine = task_state.to_task_spine();
    let has_spine_content = !spine.goal.trim().is_empty()
        || !spine.current_step.trim().is_empty()
        || !spine.done.is_empty()
        || !spine.pending.is_empty()
        || !spine.blockers.is_empty()
        || !spine.facts.is_empty()
        || !spine.decisions.is_empty()
        || !spine.next_questions.is_empty();
    if !has_spine_content && task_state.summary.trim().is_empty() {
        return String::new();
    }

    let mut ctx = format!("\n\n## {}\n", title);
    if !spine.goal.trim().is_empty() {
        ctx.push_str(&format!("**Goal:** {}\n", spine.goal));
    }
    if !spine.current_step.trim().is_empty() {
        ctx.push_str(&format!("**{}:** {}\n", progress_label, spine.current_step));
    } else if !task_state.summary.trim().is_empty() {
        ctx.push_str(&format!("**{}:** {}\n", progress_label, task_state.summary));
    }
    append_task_spine_list(&mut ctx, "Done", &spine.done);
    append_task_spine_list(&mut ctx, "Pending", &spine.pending);
    append_task_spine_list(&mut ctx, "Blockers", &spine.blockers);
    append_task_spine_list(&mut ctx, "Facts", &spine.facts);
    append_task_spine_list(&mut ctx, "Decisions", &spine.decisions);
    append_task_spine_list(&mut ctx, "Next Questions", &spine.next_questions);
    if ctx.ends_with('\n') {
        ctx.pop();
    }
    ctx
}

pub(crate) async fn persist_task_spine_from_plan_state(
    app: &AppHandle,
    db_arc: &Arc<tokio::sync::Mutex<crate::store::Database>>,
    plan_session_id: &str,
    scope_type: &str,
    scope_id: &str,
    fallback_goal: &str,
) {
    let state = app.state::<AppState>();
    let todos = {
        let plan_state = state.plan_state.lock().await;
        plan_state.get(plan_session_id).cloned().unwrap_or_default()
    };
    if todos.is_empty() {
        return;
    }

    let current_step = todos
        .iter()
        .find(|t| t.status == "in_progress")
        .or_else(|| todos.iter().find(|t| t.status == "pending"))
        .map(|t| t.content.clone())
        .unwrap_or_default();
    let spine = TaskSpine {
        goal: fallback_goal.to_string(),
        current_step,
        done: todos
            .iter()
            .filter(|t| t.status == "completed")
            .map(|t| t.content.clone())
            .collect(),
        pending: todos
            .iter()
            .filter(|t| t.status == "pending" || t.status == "in_progress")
            .map(|t| t.content.clone())
            .collect(),
        blockers: Vec::new(),
        facts: Vec::new(),
        decisions: Vec::new(),
        next_questions: Vec::new(),
    };
    let summary = summarize_todos(&todos);
    let status = if todos
        .iter()
        .all(|t| t.status == "completed" || t.status == "cancelled")
    {
        "completed"
    } else {
        "active"
    };

    let db = db_arc.lock().await;
    if let Ok(existing) = db.get_or_create_task_state(scope_type, scope_id) {
        let goal = if existing.goal.trim().is_empty() {
            fallback_goal
        } else {
            existing.goal.as_str()
        };
        let mut persisted_spine = spine;
        persisted_spine.goal = goal.to_string();
        let state_json = serde_json::to_string(&persisted_spine).unwrap_or_else(|_| "{}".into());
        let _ = db.update_task_state(
            &existing.id,
            Some(goal),
            Some(&state_json),
            Some(&summary),
            Some(status),
        );
    }
}

async fn persist_session_task_contract(
    db_arc: &Arc<tokio::sync::Mutex<crate::store::Database>>,
    session_id: &str,
    latest_user_text: &str,
    replace_goal: bool,
) {
    let trimmed = latest_user_text.trim();
    if trimmed.is_empty() {
        return;
    }
    let db = db_arc.lock().await;
    if let Ok(existing) = db.get_or_create_task_state("session", session_id) {
        let mut spine = if replace_goal {
            TaskSpine::default()
        } else {
            existing.to_task_spine()
        };
        if replace_goal || spine.goal.trim().is_empty() {
            spine.goal = trimmed.to_string();
        }
        spine.current_step = trimmed.to_string();
        let summary = spine.current_step.clone();
        let goal = spine.goal.clone();
        let state_json = serde_json::to_string(&spine).unwrap_or_else(|_| "{}".into());
        let _ = db.update_task_state(
            &existing.id,
            Some(&goal),
            Some(&state_json),
            Some(&summary),
            Some("active"),
        );
    }
}

async fn build_session_message_context_from_db(
    db_arc: &Arc<tokio::sync::Mutex<crate::store::Database>>,
    session_id: &str,
    budget: usize,
    history_mode: HistorySliceMode,
    context_toggles: &crate::headless_cli::HeadlessContextToggles,
) -> Result<SessionMessageContext, String> {
    let db = db_arc.lock().await;
    let history = db
        .get_messages_latest(session_id, 2000)
        .map_err(|e| e.to_string())?;
    let tool_minimals = extract_tool_minimals_from_history(&history);
    let session_state = db
        .get_session_context_state(session_id)
        .map_err(|e| e.to_string())?;
    let rolling_summary = session_state
        .as_ref()
        .map(|s| s.rolling_summary.as_str())
        .unwrap_or("");
    // p6 state frame: reload the explicit "where are we now" snapshot and
    // inject it right after the rolling summary so a resumed session picks
    // up the latest bearings without re-deriving from compressed history.
    let state_frame = if context_toggles.disable_state_frame {
        None
    } else {
        db.get_session_state_frame_json(session_id)
            .ok()
            .flatten()
            .and_then(|raw| pisci_kernel::agent::state_frame::StateFrame::from_json_opt(&raw))
    };
    let latest_user_text = history
        .iter()
        .rev()
        .find(|m| m.role == "user" && !m.content.trim().is_empty())
        .map(|m| m.content.clone())
        .unwrap_or_default();
    let rolling_summary_opt =
        if context_toggles.disable_rolling_summary || rolling_summary.trim().is_empty() {
            None
        } else {
            Some(rolling_summary)
        };
    let mut llm_messages = match history_mode {
        HistorySliceMode::FullRecent => {
            build_context_messages(&history, budget, rolling_summary_opt)
        }
        HistorySliceMode::SummaryOnly => {
            build_context_messages_summary_only(&history, budget, rolling_summary_opt)
        }
        HistorySliceMode::None => {
            if rolling_summary_opt.is_none() {
                Vec::new()
            } else {
                vec![rolling_summary_message(
                    rolling_summary_opt.unwrap_or_default(),
                )]
            }
        }
    };
    if let Some(frame) = state_frame {
        // Insert right after the rolling summary (if present) — i.e. at
        // position 1 when a summary exists, else at the very top.
        let summary_offset = if rolling_summary_opt.is_some() { 1 } else { 0 };
        let insert_at = summary_offset.min(llm_messages.len());
        llm_messages.insert(
            insert_at,
            pisci_kernel::agent::state_frame::state_frame_message(&frame),
        );
    }
    Ok(SessionMessageContext {
        llm_messages,
        session_state,
        latest_user_text,
        tool_minimals,
    })
}

async fn build_session_message_context(
    state: &State<'_, AppState>,
    session_id: &str,
    budget: usize,
) -> Result<SessionMessageContext, String> {
    build_session_message_context_from_db(
        &state.db,
        session_id,
        budget,
        HistorySliceMode::FullRecent,
        &crate::headless_cli::HeadlessContextToggles::default(),
    )
    .await
}

fn build_context_messages_summary_only(
    history: &[ChatMessage],
    budget: usize,
    rolling_summary: Option<&str>,
) -> Vec<LlmMessage> {
    let mut messages = Vec::new();
    if let Some(summary) = rolling_summary.filter(|summary| !summary.trim().is_empty()) {
        messages.push(rolling_summary_message(summary));
    }

    let turns = split_history_into_turns(history);
    let mut tail: Vec<LlmMessage> = Vec::new();
    for turn in turns
        .into_iter()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let (user_summary, assistant_summary) = summarize_turn(&turn);
        tail.push(LlmMessage {
            role: "user".into(),
            content: MessageContent::text(user_summary),
        });
        tail.push(LlmMessage {
            role: "assistant".into(),
            content: MessageContent::text(assistant_summary),
        });
    }
    messages.extend(tail);

    let mut kept = Vec::new();
    let mut token_est = 0usize;
    for message in messages.into_iter().rev() {
        let message_tokens = pisci_kernel::llm::estimate_request_input_tokens(
            std::slice::from_ref(&message),
            None,
            &[],
        );
        if token_est + message_tokens > budget {
            continue;
        }
        token_est += message_tokens;
        kept.push(message);
    }
    kept.reverse();
    kept
}

fn truncate_chars(content: &str, max_chars: usize) -> String {
    if max_chars == 0 || content.chars().count() <= max_chars {
        return content.to_string();
    }
    format!("{}...", content.chars().take(max_chars).collect::<String>())
}

fn render_pool_context_snapshot(
    scene_policy: ScenePolicy,
    pool: Option<&crate::pool::PoolSession>,
    pool_todos: &[crate::pool::KoiTodo],
    recent_messages: &[crate::pool::PoolMessage],
) -> String {
    if !scene_policy.include_pool_context
        || matches!(scene_policy.pool_snapshot_mode(), PoolSnapshotMode::Off)
    {
        return String::new();
    }
    let mut ctx = String::new();
    if let Some(pool) = pool {
        ctx.push_str("\n\n## Pool Context\n");
        ctx.push_str(&format!(
            "Pool: {} ({})\nStatus: {}",
            pool.name, pool.id, pool.status
        ));
        if let Some(project_dir) = pool.project_dir.as_deref() {
            ctx.push_str(&format!("\nProject dir: {}", project_dir));
        }
        if !pool.org_spec.trim().is_empty() {
            ctx.push_str(&format!(
                "\nOrg spec:\n{}",
                truncate_chars(&pool.org_spec, scene_policy.org_spec_preview_chars())
            ));
        }
    }

    let todo_summary = if pool_todos.is_empty() {
        "No pool todos.".to_string()
    } else {
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        for todo in pool_todos {
            *counts.entry(todo.status.clone()).or_insert(0) += 1;
        }
        let parts = counts
            .into_iter()
            .map(|(status, count)| format!("{}={}", status, count))
            .collect::<Vec<_>>()
            .join(", ");
        let mut summary = format!("Pool todos: {}", parts);
        if matches!(scene_policy.pool_snapshot_mode(), PoolSnapshotMode::Full) {
            let highlighted = pool_todos
                .iter()
                .filter(|todo| matches!(todo.status.as_str(), "todo" | "in_progress" | "blocked"))
                .take(4)
                .map(|todo| format!("- [{}] {} ({})", todo.status, todo.title, todo.owner_id))
                .collect::<Vec<_>>();
            if !highlighted.is_empty() {
                summary.push_str("\nOpen todo highlights:\n");
                summary.push_str(&highlighted.join("\n"));
            }
        }
        summary
    };
    ctx.push_str(&format!("\n{}", todo_summary));

    let digest = build_coordination_event_digest(
        recent_messages,
        scene_policy.event_digest_mode(),
        &[],
        scene_policy.recent_pool_message_limit(),
        scene_policy.recent_pool_message_chars(),
    );
    if !digest.lines.is_empty() {
        ctx.push_str("\nCoordination event digest:\n");
        ctx.push_str(&digest.lines.join("\n"));
    }

    let message_limit = match scene_policy.pool_snapshot_mode() {
        PoolSnapshotMode::Off => 0,
        PoolSnapshotMode::Compact => scene_policy.recent_pool_message_limit().min(3),
        PoolSnapshotMode::Full => scene_policy.recent_pool_message_limit(),
    };
    if message_limit == 0 {
        return ctx;
    }
    let digest: Vec<String> = recent_messages
        .iter()
        .rev()
        .take(message_limit)
        .rev()
        .map(|msg| {
            let content = truncate_chars(
                &msg.content.replace('\n', " "),
                scene_policy.recent_pool_message_chars(),
            );
            format!(
                "- #{} {} [{}{}]: {}",
                msg.id,
                msg.sender_id,
                msg.msg_type,
                msg.event_type
                    .as_deref()
                    .map(|event| format!("/{}", event))
                    .unwrap_or_default(),
                content
            )
        })
        .collect();
    if digest.is_empty() {
        if ctx.contains("Coordination event digest:") {
            return ctx;
        }
        ctx.push_str("\nRecent pool messages: none.");
    } else {
        ctx.push_str("\nRecent pool messages:\n");
        ctx.push_str(&digest.join("\n"));
    }
    ctx
}

#[allow(clippy::too_many_arguments)]
async fn build_chat_prompt_artifacts(
    app: &AppHandle,
    state: &State<'_, AppState>,
    session_id: &str,
    query_text: &str,
    workspace_root: &str,
    context_window: u32,
    max_tokens: u32,
    allow_outside_workspace: bool,
    builtin_tool_enabled: &std::collections::HashMap<String, bool>,
    project_instruction_budget_chars: u32,
    enable_project_instructions: bool,
) -> Result<ChatPromptArtifacts, String> {
    let scene_policy = ScenePolicy::for_kind(SceneKind::MainChat);
    let user_tools_dir = app.path().app_data_dir().map(|d| d.join("user-tools")).ok();
    let app_data_dir = app.path().app_data_dir().ok();

    let skill_loader_arc = load_skill_loader(app);
    let registry = build_registry_for_scene(
        SceneKind::MainChat,
        state.browser.clone(),
        user_tools_dir.as_deref(),
        Some(state.db.clone()),
        Some(builtin_tool_enabled),
        Some(app.clone()),
        Some(state.settings.clone()),
        app_data_dir,
        skill_loader_arc,
    )
    .await;
    let registry = Arc::new(registry);
    // Chat budget estimation uses the same Minimal injection mode the
    // harness will actually send, so the token accounting stays honest.
    let tool_defs = registry.to_tool_defs(pisci_kernel::agent::tool::ToolDefMode::Minimal);

    let memory_context = {
        let db = state.db.lock().await;
        let keywords: Vec<&str> = query_text.split_whitespace().take(10).collect();
        let query = keywords.join(" ");
        match db.search_memories_scoped(&query, "pisci", None, 5) {
            Ok(mems) if !mems.is_empty() => {
                let mut ctx = String::from("\n\n## Personal Context (from memory)\n");
                for m in &mems {
                    ctx.push_str(&format!("- {}\n", m.content));
                }
                ctx
            }
            _ => String::new(),
        }
    };

    let active_task_state = {
        let db = state.db.lock().await;
        match db.load_task_state("session", session_id) {
            Ok(Some(ts))
                if ts.status == "active" && (!ts.goal.is_empty() || !ts.summary.is_empty()) =>
            {
                Some(ts)
            }
            _ => None,
        }
    };
    let task_state_context = active_task_state
        .as_ref()
        .map(|ts| render_task_state_section("Active Task State", "Progress", ts))
        .unwrap_or_default();

    let injection_budget = scene_policy.compute_injection_budget(context_window, max_tokens);
    let full_memory_context = budget_truncate(
        &format!("{}{}", memory_context, task_state_context),
        injection_budget,
    );
    let project_instruction_context =
        if scene_policy.project_instructions_enabled(enable_project_instructions) {
            match render_project_instruction_context(
                std::path::Path::new(workspace_root),
                project_instruction_budget_chars as usize,
            ) {
                Ok(content) => content,
                Err(error) => {
                    tracing::warn!("Failed to load project instructions: {}", error);
                    String::new()
                }
            }
        } else {
            String::new()
        };

    let mut system_prompt = build_main_chat_system_prompt(
        &full_memory_context,
        workspace_root,
        allow_outside_workspace,
    );
    if !project_instruction_context.is_empty() {
        system_prompt.push_str(&project_instruction_context);
    }
    tracing::info!(
        "main_chat_context_slices session={} memory_mode={:?} pool_snapshot_mode={:?} memory_chars={} task_state_chars={} injected_chars={} project_instruction_chars={} system_prompt_chars={}",
        session_id,
        scene_policy.memory_slice_mode(),
        scene_policy.pool_snapshot_mode(),
        memory_context.chars().count(),
        task_state_context.chars().count(),
        full_memory_context.chars().count(),
        project_instruction_context.chars().count(),
        system_prompt.chars().count(),
    );
    Ok(ChatPromptArtifacts {
        system_prompt,
        registry,
        tool_defs,
    })
}

#[tauri::command]
pub async fn create_session(
    state: State<'_, AppState>,
    title: Option<String>,
) -> Result<Session, String> {
    let db = state.db.lock().await;
    db.create_session(title.as_deref())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_sessions(
    state: State<'_, AppState>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<SessionList, String> {
    let db = state.db.lock().await;
    let sessions = db
        .list_sessions(limit.unwrap_or(20), offset.unwrap_or(0))
        .map_err(|e| e.to_string())?;
    let total = sessions.len();
    Ok(SessionList { sessions, total })
}

#[tauri::command]
pub async fn delete_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let db = state.db.lock().await;
    db.delete_session(&session_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn rename_session(
    state: State<'_, AppState>,
    session_id: String,
    title: String,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.rename_session(&session_id, &title)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_messages(
    state: State<'_, AppState>,
    session_id: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<ChatMessage>, String> {
    let db = state.db.lock().await;
    let lim = limit.unwrap_or(100);
    let off = offset.unwrap_or(0);
    if off == 0 {
        // Default: return the latest `limit` messages in chronological order.
        // This ensures the frontend always sees the newest messages regardless of how many
        // tool_calls/tool_results have accumulated in the session history.
        db.get_messages_latest(&session_id, lim)
            .map_err(|e| e.to_string())
    } else {
        // Pagination: caller wants older messages (load-more-history).
        // `off` is the number of messages already loaded (from the newest end).
        // We skip the newest `off` rows and return the next `limit` older rows,
        // still in chronological (ascending) order.
        db.get_messages_older(&session_id, lim, off)
            .map_err(|e| e.to_string())
    }
}

/// Send a user message and run the agent loop.
/// Streams AgentEvents to the frontend via Tauri events.
#[tauri::command]
pub async fn chat_send(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    content: String,
    attachment: Option<FrontendAttachment>,
    // If false, preserve the existing plan (continue previous tasks).
    // If true or None (default), clear the plan before starting a new turn.
    clear_plan: Option<bool>,
) -> Result<(), String> {
    tracing::info!(
        "chat_send called: session={} content_len={} has_attachment={}",
        session_id,
        content.len(),
        attachment.is_some()
    );

    // Load settings
    let (
        provider,
        model,
        api_key,
        base_url,
        workspace_root,
        max_tokens,
        context_window,
        confirm_shell,
        confirm_file_write,
        policy_mode,
        tool_rate_limit_per_minute,
        tool_settings,
        max_iterations,
        builtin_tool_enabled,
        allow_outside_workspace,
        vision_enabled,
        vision_use_main_llm,
        vision_provider,
        vision_model,
        vision_api_key,
        llm_read_timeout_secs,
        auto_compact_input_tokens_threshold,
        project_instruction_budget_chars,
        enable_project_instructions,
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
            settings.confirm_shell_commands,
            settings.confirm_file_writes,
            settings.policy_mode.clone(),
            settings.tool_rate_limit_per_minute,
            std::sync::Arc::new(pisci_kernel::agent::tool::ToolSettings::from_settings(
                &settings,
            )),
            settings.max_iterations,
            settings.builtin_tool_enabled.clone(),
            settings.allow_outside_workspace,
            settings.vision_enabled,
            settings.vision_use_main_llm,
            settings.vision_provider.clone(),
            settings.vision_model.clone(),
            settings.vision_api_key.clone(),
            settings.llm_read_timeout_secs,
            settings.auto_compact_input_tokens_threshold,
            settings.project_instruction_budget_chars,
            settings.enable_project_instructions,
        )
    };

    tracing::info!(
        "chat_send: provider={} model={} api_key_empty={}",
        provider,
        model,
        api_key.is_empty()
    );

    if api_key.is_empty() {
        tracing::warn!("chat_send: API key not configured");
        return Err(
            "API key not configured. Please open Settings to configure your API key.".into(),
        );
    }

    // Prompt injection detection on user input
    {
        let gate = PolicyGate::with_profile_and_flags(
            &workspace_root,
            &policy_mode,
            tool_rate_limit_per_minute,
            allow_outside_workspace,
        );
        let decision = gate.check_user_input(&content);
        match decision {
            pisci_kernel::policy::PolicyDecision::Deny(reason) => {
                tracing::warn!(
                    "chat_send: user input rejected by injection detection: {}",
                    reason
                );
                return Err(format!("Input rejected: {}", reason));
            }
            pisci_kernel::policy::PolicyDecision::Warn(reason) => {
                tracing::warn!(
                    "chat_send: potential injection detected (proceeding): {}",
                    reason
                );
                let db = state.db.lock().await;
                let _ = db.append_audit(
                    &session_id,
                    "injection_detection",
                    "warn",
                    Some(&reason),
                    None,
                    false,
                );
            }
            pisci_kernel::policy::PolicyDecision::Allow => {}
        }
    }

    // Resolve attachment: convert FrontendAttachment → MediaAttachment
    // For non-vision models or non-image files, we append the path to the message text.
    // For vision models + image data, we pass through as MediaAttachment for inline injection.
    let vision_capable = if vision_use_main_llm {
        vision_enabled || model_supports_vision(&provider, &model)
    } else {
        !vision_provider.is_empty() && !vision_model.is_empty() && !vision_api_key.is_empty()
    };
    let (effective_content, media_attachment): (String, Option<crate::gateway::MediaAttachment>) =
        if let Some(att) = attachment {
            if att.media_type.starts_with("image/") {
                if vision_capable {
                    // Vision model: pass raw bytes for inline base64 injection
                    let data = att.data.as_deref().and_then(|b64| {
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64).ok()
                    });
                    let media = crate::gateway::MediaAttachment {
                        media_type: att.media_type.clone(),
                        url: None,
                        data,
                        filename: att.filename.clone(),
                    };
                    (content.clone(), Some(media))
                } else {
                    // Non-vision model: use file path directly or save base64 to temp
                    let path_str = if let Some(p) = &att.path {
                        p.clone()
                    } else if let Some(b64) = &att.data {
                        let ext = match att.media_type.as_str() {
                            "image/png" => "png",
                            "image/gif" => "gif",
                            "image/webp" => "webp",
                            _ => "jpg",
                        };
                        let default_fname = format!("attachment.{}", ext);
                        let fname = att.filename.as_deref().unwrap_or(&default_fname);
                        let tmp = std::env::temp_dir().join(fname);
                        if let Ok(bytes) =
                            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
                        {
                            let _ = std::fs::write(&tmp, &bytes);
                        }
                        tmp.to_string_lossy().to_string()
                    } else {
                        String::new()
                    };
                    let msg = if path_str.is_empty() {
                        content.clone()
                    } else if content.trim().is_empty() {
                        format!("[图片已保存到: {}]", path_str)
                    } else {
                        format!("{}\n[附带图片已保存到: {}]", content, path_str)
                    };
                    (msg, None)
                }
            } else {
                // Non-image file: always pass as path reference in message text
                let path_str = att.path.clone().unwrap_or_default();
                let msg = if path_str.is_empty() {
                    content.clone()
                } else if content.trim().is_empty() {
                    format!("[附件: {}]", path_str)
                } else {
                    format!("{}\n[附件: {}]", content, path_str)
                };
                (msg, None)
            }
        } else {
            (content.clone(), None)
        };

    // Save user message to DB (use effective_content which may include file path annotation)
    // clear_plan defaults to true; pass false to preserve an existing plan (continue previous tasks).
    let replace_task_contract = clear_plan.unwrap_or(true);
    if replace_task_contract {
        let mut plans = state.plan_state.lock().await;
        plans.remove(&session_id);
    }
    pisci_kernel::agent::vision::clear_selection(&session_id).await;

    {
        let db = state.db.lock().await;
        db.append_message(&session_id, "user", &effective_content)
            .map_err(|e| e.to_string())?;
        db.update_session_status(&session_id, "running")
            .map_err(|e| e.to_string())?;
    }
    persist_session_task_contract(
        &state.db,
        &session_id,
        &effective_content,
        replace_task_contract,
    )
    .await;

    // Load message history and build context with layered compression.
    let budget = compute_context_budget(context_window, max_tokens);
    let mut llm_messages = build_session_message_context(&state, &session_id, budget)
        .await?
        .llm_messages;

    // For vision-capable models: inject the attachment image into the last user message
    if let Some(ref media) = media_attachment {
        if let Some(ref data) = media.data {
            if media.media_type.starts_with("image/") {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(data);
                let image_block = ContentBlock::Image {
                    source: pisci_kernel::llm::ImageSource {
                        source_type: "base64".to_string(),
                        media_type: media.media_type.clone(),
                        data: b64,
                    },
                };
                if let Some(last) = llm_messages.last_mut() {
                    if last.role == "user" {
                        let text = last.content.as_text();
                        let mut blocks = vec![ContentBlock::Text { text }];
                        blocks.push(image_block);
                        last.content = MessageContent::Blocks(blocks);
                    }
                }
            }
        }
    }

    // Build cancellation token
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut flags = state.cancel_flags.lock().await;
        flags.insert(session_id.clone(), cancel.clone());
    }

    // Build agent components
    let client = build_client_with_timeout(
        &provider,
        &api_key,
        if base_url.is_empty() {
            None
        } else {
            Some(&base_url)
        },
        llm_read_timeout_secs,
    );

    let prompt_artifacts = build_chat_prompt_artifacts(
        &app,
        &state,
        &session_id,
        &effective_content,
        &workspace_root,
        context_window,
        max_tokens,
        allow_outside_workspace,
        &builtin_tool_enabled,
        project_instruction_budget_chars,
        enable_project_instructions,
    )
    .await?;
    let registry = prompt_artifacts.registry.clone();

    let policy = Arc::new(PolicyGate::with_profile_and_flags(
        &workspace_root,
        &policy_mode,
        tool_rate_limit_per_minute,
        allow_outside_workspace,
    ));

    // Main pisci chat — uses the persistent, UI-attached harness
    // shape. Per-run plumbing (`notification_rx`, confirmations) is
    // passed to the bridge rather than stored in the config.
    let (fallback_models, compaction_settings, enable_streaming) = {
        let settings = state.settings.lock().await;
        (
            settings.fallback_models.clone(),
            pisci_kernel::agent::harness::config::CompactionSettings::from_settings(&settings),
            settings.enable_streaming,
        )
    };
    let agent = pisci_kernel::agent::harness::HarnessConfig::for_main_chat(
        model.clone(),
        fallback_models,
        registry,
        policy,
        prompt_artifacts.system_prompt,
        max_tokens,
        context_window,
        pisci_kernel::agent::harness::config::ConfirmFlags {
            confirm_shell,
            confirm_file_write,
        },
        Some(vision_capable),
        auto_compact_input_tokens_threshold,
        compaction_settings,
        state.db.clone(),
        state.plan_state.clone(),
    )
    .with_streaming(enable_streaming)
    .into_agent_loop(client, None, Some(state.confirmation_responses.clone()));

    let ctx = ToolContext {
        session_id: session_id.clone(),
        workspace_root: std::path::PathBuf::from(&workspace_root),
        bypass_permissions: false,
        settings: tool_settings,
        max_iterations: Some(max_iterations),
        memory_owner_id: "pisci".to_string(),
        pool_session_id: None,
        cancel: cancel.clone(),
    };

    // Create event channel
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);

    // Spawn the entire agent loop in a background task so chat_send returns immediately.
    // This allows the frontend event listener to be fully registered before events arrive.
    let app_clone = app.clone();
    let session_id_clone = session_id.clone();
    let db_arc = state.db.clone();
    let cancel_flags_arc = state.cancel_flags.clone();
    let model_clone = model.clone();
    let max_tokens_clone = max_tokens;
    let provider_clone = provider.clone();
    let api_key_clone = api_key.clone();
    let base_url_clone = base_url.clone();
    let effective_content_clone = effective_content.clone();
    tracing::info!(
        "chat_send: spawning agent background task for session={}",
        session_id
    );

    tokio::spawn(async move {
        tracing::info!("agent task started for session={}", session_id_clone);

        // Forward events to frontend
        let app_fwd = app_clone.clone();
        let sid_fwd = session_id_clone.clone();
        let forward_handle = tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                tracing::debug!("forwarding event to frontend: session={}", sid_fwd);
                let payload = serde_json::to_value(&event).unwrap_or_default();
                let emit_result =
                    app_fwd.emit(&format!("agent_event_{}", sid_fwd), payload.clone());
                if let Err(e) = emit_result {
                    tracing::warn!("failed to emit event: {}", e);
                }
                // Broadcast to overlay window (subscribes to "agent_broadcast")
                let _ = app_fwd.emit("agent_broadcast", payload);
            }
        });

        // NOTE: agent.run() no longer emits Done — we do it here AFTER the DB write.
        // Agent handles complex tasks autonomously via its own tools (call_fish, plan_todo, etc.)
        let result = agent
            .run(llm_messages, event_tx.clone(), cancel.clone(), ctx)
            .await;

        tracing::info!(
            "agent.run completed for session={} ok={}",
            session_id_clone,
            result.is_ok()
        );

        // ── Critical: persist to DB BEFORE emitting Done ───────────────────────────
        // The frontend calls getMessages() on the Done event. If we emit Done first,
        // the frontend reads the DB before the write completes → empty history.
        match &result {
            Ok((final_messages, total_in, total_out)) => {
                // Persist the new messages produced by the agent during this run.
                // final_messages is the new_messages buffer from AgentLoop::run(), which only
                // contains messages appended during this run (immune to compaction).
                {
                    let db = db_arc.lock().await;
                    persist_agent_turn(&db, &session_id_clone, final_messages);
                    let _ = db.update_session_status(&session_id_clone, "idle");
                }
                persist_task_spine_from_plan_state(
                    &app_clone,
                    &db_arc,
                    &session_id_clone,
                    "session",
                    &session_id_clone,
                    &effective_content_clone,
                )
                .await;

                // Auto-extract memories from this conversation (non-blocking, best-effort)
                {
                    let db_for_mem = db_arc.clone();
                    let sid_for_mem = session_id_clone.clone();
                    let msgs_for_mem = final_messages.clone();
                    let model_for_mem = model_clone.clone();
                    let mem_client = build_client_with_timeout(
                        &provider_clone,
                        &api_key_clone,
                        if base_url_clone.is_empty() {
                            None
                        } else {
                            Some(&base_url_clone)
                        },
                        120, // memory extraction uses default timeout
                    );
                    tokio::spawn(async move {
                        auto_extract_memories(
                            db_for_mem,
                            sid_for_mem,
                            msgs_for_mem,
                            mem_client,
                            model_for_mem,
                            max_tokens_clone,
                            "pisci".to_string(),
                        )
                        .await;
                    });
                }

                // NOW emit Done — frontend getMessages() will see the persisted data
                let _ = event_tx
                    .send(AgentEvent::Done {
                        total_input_tokens: *total_in,
                        total_output_tokens: *total_out,
                    })
                    .await;
            }
            Err(e) => {
                tracing::warn!("Agent loop error for session {}: {}", session_id_clone, e);
                {
                    let db = db_arc.lock().await;
                    let _ = db.update_session_status(&session_id_clone, "idle");
                }
                // Emit error event (Done is not sent on error)
                let _ = event_tx
                    .send(AgentEvent::Error {
                        message: e.to_string(),
                    })
                    .await;
            }
        }

        // Close the channel — forward_handle will drain remaining events (Done/Error)
        // and emit them to the frontend, then exit.
        drop(event_tx);

        // Wait for all events (including Done) to reach the frontend
        let _ = forward_handle.await;

        // Clean up cancel flag
        {
            let mut flags = cancel_flags_arc.lock().await;
            flags.remove(&session_id_clone);
        }
    });

    // Return immediately — agent runs in background, events streamed via Tauri events
    Ok(())
}

/// Run the agent for a single message (used by both frontend chat_send and IM gateway).
/// Returns the assistant response text.
/// Returns true if the given provider+model supports vision (image input).
pub fn model_supports_vision(provider: &str, model: &str) -> bool {
    let m = model.to_lowercase();
    let p = provider.to_lowercase();
    // OpenAI vision models
    if p == "openai" || p.contains("openai") {
        return m.contains("gpt-4o")
            || m.contains("gpt-4-vision")
            || m.contains("gpt-4-turbo")
            || m.contains("o1");
    }
    // Anthropic Claude 3+
    if p == "anthropic" || p.contains("claude") || m.contains("claude") {
        return m.contains("claude-3")
            || m.contains("claude-opus")
            || m.contains("claude-sonnet")
            || m.contains("claude-haiku");
    }
    // Google Gemini
    if p == "google" || p.contains("gemini") || m.contains("gemini") {
        return true;
    }
    // Qwen VL models
    if m.contains("qwen-vl") || m.contains("qwen2-vl") || m.contains("qvq") {
        return true;
    }
    // Kimi / Moonshot vision models
    if p.contains("kimi") || p.contains("moonshot") {
        return m.contains("vision") || m.contains("vl");
    }
    // Zhipu GLM vision models
    if p.contains("zhipu") || p.contains("glm") {
        return m.contains("vision") || m.contains("vl") || m.contains("glm-4v");
    }
    // MiniMax vision models
    if p.contains("minimax") {
        return m.contains("vision") || m.contains("vl");
    }
    // DeepSeek — no vision support currently
    false
}

/// Return value: (text_reply, optional_image_bytes, optional_image_mime)
#[derive(Debug, Clone, Default)]
pub struct HeadlessRunOptions {
    pub pool_session_id: Option<String>,
    pub extra_system_context: Option<String>,
    pub session_title: Option<String>,
    pub session_source: Option<String>,
    pub scene_kind: Option<SceneKind>,
    pub workspace_root_override: Option<String>,
    pub builtin_tool_overrides: HashMap<String, bool>,
    pub context_toggles: crate::headless_cli::HeadlessContextToggles,
}

pub(crate) const SESSION_SOURCE_IM_PREFIX: &str = "im_";
pub(crate) const SESSION_SOURCE_PISCI_POOL: &str = "pisci_pool";
pub(crate) const SESSION_SOURCE_PISCI_HEARTBEAT_GLOBAL: &str = "pisci_heartbeat_global";
pub(crate) const SESSION_SOURCE_PISCI_INTERNAL: &str = "pisci_internal";

pub(crate) fn pool_pisci_session_id(pool_id: &str) -> String {
    format!("pisci_pool_{}", pool_id)
}

fn is_pool_scoped_session_source(source: &str) -> bool {
    source == SESSION_SOURCE_PISCI_POOL
}

fn is_heartbeat_session_source(source: &str) -> bool {
    source == SESSION_SOURCE_PISCI_HEARTBEAT_GLOBAL
}

fn derive_headless_session_source(channel: &str, pool_session_id: Option<&str>) -> String {
    if pool_session_id.is_some() {
        return SESSION_SOURCE_PISCI_POOL.to_string();
    }
    if channel == "heartbeat" {
        return SESSION_SOURCE_PISCI_HEARTBEAT_GLOBAL.to_string();
    }
    match channel {
        "internal" => SESSION_SOURCE_PISCI_INTERNAL.to_string(),
        other if other.starts_with(SESSION_SOURCE_IM_PREFIX) => other.to_string(),
        other => format!("{}{}", SESSION_SOURCE_IM_PREFIX, other),
    }
}

fn resolve_headless_scene_kind(
    channel: &str,
    desired_source: &str,
    options: Option<&HeadlessRunOptions>,
) -> SceneKind {
    if let Some(kind) = options.and_then(|o| o.scene_kind) {
        return kind;
    }
    if is_heartbeat_session_source(desired_source) || channel == "heartbeat" {
        return SceneKind::HeartbeatSupervisor;
    }
    if options
        .and_then(|o| o.pool_session_id.as_deref())
        .filter(|pool_id| !pool_id.is_empty())
        .is_some()
    {
        return SceneKind::PoolCoordinator;
    }
    SceneKind::IMHeadless
}

pub(crate) fn validate_headless_session_scope(
    actual_source: &str,
    desired_source: &str,
    pool_session_id: Option<&str>,
) -> Result<(), String> {
    if actual_source != desired_source {
        return Err(format!(
            "Session source mismatch: session is '{}' but this run requires '{}'",
            actual_source, desired_source
        ));
    }

    if pool_session_id.is_some() && !is_pool_scoped_session_source(actual_source) {
        return Err(format!(
            "Pool-scoped run cannot reuse non-pool session source '{}'",
            actual_source
        ));
    }

    if pool_session_id.is_none() && is_pool_scoped_session_source(actual_source) {
        return Err(format!(
            "Non-pool run cannot reuse pool-scoped session source '{}'",
            actual_source
        ));
    }

    Ok(())
}

pub async fn run_agent_headless(
    state: &AppState,
    session_id: &str,
    user_message: &str,
    inbound_media: Option<crate::gateway::MediaAttachment>,
    channel: &str,
    options: Option<HeadlessRunOptions>,
) -> Result<(String, Option<Vec<u8>>, Option<String>), String> {
    let (
        provider,
        model,
        api_key,
        base_url,
        mut workspace_root,
        max_tokens,
        context_window,
        policy_mode,
        tool_rate_limit_per_minute,
        tool_settings,
        max_iterations,
        mut builtin_tool_enabled,
        allow_outside_workspace,
        vision_setting,
        vision_use_main_llm,
        vision_provider,
        vision_model,
        vision_api_key,
        llm_read_timeout_secs,
        auto_compact_input_tokens_threshold,
        project_instruction_budget_chars,
        enable_project_instructions,
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
            std::sync::Arc::new(pisci_kernel::agent::tool::ToolSettings::from_settings(
                &settings,
            )),
            settings.max_iterations,
            settings.builtin_tool_enabled.clone(),
            settings.allow_outside_workspace,
            settings.vision_enabled,
            settings.vision_use_main_llm,
            settings.vision_provider.clone(),
            settings.vision_model.clone(),
            settings.vision_api_key.clone(),
            settings.llm_read_timeout_secs,
            settings.auto_compact_input_tokens_threshold,
            settings.project_instruction_budget_chars,
            settings.enable_project_instructions,
        )
    };
    if api_key.is_empty() {
        return Err("API key not configured".into());
    }
    tracing::info!(
        "run_agent_headless: provider={} model={} channel={} session={}",
        provider,
        model,
        channel,
        session_id
    );

    if let Some(override_root) = options
        .as_ref()
        .and_then(|o| o.workspace_root_override.as_ref())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        workspace_root = override_root.to_string();
    }
    if let Some(extra_tools) = options
        .as_ref()
        .map(|o| o.builtin_tool_overrides.clone())
        .filter(|m| !m.is_empty())
    {
        builtin_tool_enabled.extend(extra_tools);
    }

    {
        let mut plans = state.plan_state.lock().await;
        plans.remove(session_id);
    }
    pisci_kernel::agent::vision::clear_selection(session_id).await;

    let pool_session_id = options.as_ref().and_then(|o| o.pool_session_id.clone());
    let context_toggles = options
        .as_ref()
        .map(|o| o.context_toggles.clone())
        .unwrap_or_default();
    let extra_system_context = options
        .as_ref()
        .and_then(|o| o.extra_system_context.clone())
        .unwrap_or_default();
    let desired_session_title = options
        .as_ref()
        .and_then(|o| o.session_title.clone())
        .unwrap_or_else(|| session_id.to_string());
    let desired_session_source = options
        .as_ref()
        .and_then(|o| o.session_source.clone())
        .unwrap_or_else(|| derive_headless_session_source(channel, pool_session_id.as_deref()));
    let scene_kind =
        resolve_headless_scene_kind(channel, &desired_session_source, options.as_ref());
    let scene_policy = ScenePolicy::for_kind(scene_kind);

    // vision_capable: user override OR auto-detection by provider/model name
    // Also accounts for separate vision model when vision_use_main_llm is false.
    let vision_capable = if vision_use_main_llm {
        vision_setting || model_supports_vision(&provider, &model)
    } else {
        !vision_provider.is_empty() && !vision_model.is_empty() && !vision_api_key.is_empty()
    };

    // Build the effective user message text, handling inbound media.
    // Non-image media is intentionally passed through as a local file/metadata prompt
    // so the agent can decide how to transcribe or inspect it with available tools.
    let effective_user_message = if let Some(ref media) = inbound_media {
        if let Some(ref data) = media.data {
            if media.media_type.starts_with("image/") && !vision_capable {
                // Non-vision model: save image to temp dir and inform agent honestly
                let ext = match media.media_type.as_str() {
                    "image/png" => "png",
                    "image/gif" => "gif",
                    "image/webp" => "webp",
                    _ => "jpg",
                };
                let default_filename = format!("im_image.{}", ext);
                let filename = media.filename.as_deref().unwrap_or(&default_filename);
                let tmp_path = std::env::temp_dir().join(filename);
                if let Ok(()) = std::fs::write(&tmp_path, data) {
                    let path_str = tmp_path.to_string_lossy();
                    if user_message.is_empty() || user_message == "[图片]" {
                        format!("用户通过 IM 发送了一张图片，文件已保存到本地：{}\n当前模型不支持图像识别，请如实告知用户，并询问是否需要对图片进行文件操作（如移动、重命名、查看文件信息等）。", path_str)
                    } else {
                        format!("{}\n[用户附带了一张图片，已保存到：{}。当前模型不支持图像识别，请告知用户并询问是否需要文件操作]", user_message, path_str)
                    }
                } else {
                    user_message.to_string()
                }
            } else {
                let default_filename = if media.media_type.starts_with("audio/") {
                    "im_audio.bin"
                } else {
                    "im_attachment.bin"
                };
                let filename = media
                    .filename
                    .as_deref()
                    .and_then(|name| Path::new(name).file_name())
                    .and_then(|name| name.to_str())
                    .unwrap_or(default_filename);
                let tmp_path = std::env::temp_dir().join(filename);
                if let Ok(()) = std::fs::write(&tmp_path, data) {
                    let path_str = tmp_path.to_string_lossy();
                    let prefix = if media.media_type.starts_with("audio/") {
                        "用户通过 IM 发送了一条语音消息"
                    } else {
                        "用户通过 IM 发送了一个媒体附件"
                    };
                    if user_message.trim().is_empty()
                        || user_message == "[语音消息]"
                        || user_message == "[音频消息]"
                    {
                        format!(
                            "{}，附件已保存到本地：{}\n媒体类型：{}\n请根据可用工具自行尝试转写、读取或处理该文件。",
                            prefix, path_str, media.media_type
                        )
                    } else {
                        format!(
                            "{}\n[{}，附件已保存到：{}，媒体类型：{}。请根据可用工具自行处理。]",
                            user_message, prefix, path_str, media.media_type
                        )
                    }
                } else {
                    format!(
                        "{}\n[IM 媒体附件：type={} filename={:?} url={:?}。附件未能保存到本地，请根据这些线索处理。]",
                        user_message, media.media_type, media.filename, media.url
                    )
                }
            }
        } else {
            let prefix = if media.media_type.starts_with("audio/") {
                "用户通过 IM 发送了一条语音消息"
            } else {
                "用户通过 IM 发送了一个媒体附件"
            };
            let details = format!(
                "{}：type={} filename={:?} url={:?}",
                prefix, media.media_type, media.filename, media.url
            );
            if user_message.trim().is_empty()
                || user_message == "[语音消息]"
                || user_message == "[音频消息]"
            {
                format!(
                    "{}\n当前网关没有内联音频字节，但已保留平台媒体线索；请根据可用工具自行尝试获取或转写。",
                    details
                )
            } else {
                format!("{}\n[{}]", user_message, details)
            }
        }
    } else {
        user_message.to_string()
    };

    {
        let db = state.db.lock().await;
        match db.get_session(session_id).map_err(|e| e.to_string())? {
            Some(existing) => {
                validate_headless_session_scope(
                    &existing.source,
                    &desired_session_source,
                    pool_session_id.as_deref(),
                )?;
            }
            None => {
                db.ensure_fixed_session(
                    session_id,
                    &desired_session_title,
                    &desired_session_source,
                )
                .map_err(|e| e.to_string())?;
            }
        }
        // Check if this user message was already pre-inserted by lib.rs (to ensure it's visible
        // in the frontend before the agent starts). Skip duplicate insertion if so.
        let already_inserted = db
            .get_messages_latest(session_id, 1)
            .ok()
            .and_then(|msgs| msgs.into_iter().last())
            .map(|m| m.role == "user" && m.content == effective_user_message)
            .unwrap_or(false);
        if already_inserted {
            tracing::info!(
                "run_agent_headless: user message already pre-inserted for {}, skipping",
                session_id
            );
        } else if effective_user_message.trim().is_empty() {
            tracing::warn!(
                "run_agent_headless: skipping empty user message for {}",
                session_id
            );
        } else {
            tracing::info!(
                "run_agent_headless: inserting user message for {}",
                session_id
            );
            let _ = db.append_message(session_id, "user", &effective_user_message);
        }
    }
    persist_session_task_contract(&state.db, session_id, &effective_user_message, true).await;

    let client = build_client_with_timeout(
        &provider,
        &api_key,
        if base_url.is_empty() {
            None
        } else {
            Some(&base_url)
        },
        llm_read_timeout_secs,
    );
    let user_tools_dir_h = state
        .app_handle
        .path()
        .app_data_dir()
        .map(|d| d.join("user-tools"))
        .ok();
    let app_data_dir_h = state.app_handle.path().app_data_dir().ok();
    let registry = build_registry_for_scene(
        scene_kind,
        state.browser.clone(),
        user_tools_dir_h.as_deref(),
        Some(state.db.clone()),
        Some(&builtin_tool_enabled),
        Some(state.app_handle.clone()),
        Some(state.settings.clone()),
        app_data_dir_h,
        None,
    )
    .await;
    let registry = Arc::new(registry);
    let policy = Arc::new(PolicyGate::with_profile_and_flags(
        &workspace_root,
        &policy_mode,
        tool_rate_limit_per_minute,
        allow_outside_workspace,
    ));

    let scoped_memory_context = if context_toggles.disable_memory_context {
        String::new()
    } else {
        match scene_policy.memory_slice_mode() {
            MemorySliceMode::Off => String::new(),
            MemorySliceMode::ScopedSearch | MemorySliceMode::ScopedPlusRecent => {
                let keywords: Vec<&str> =
                    effective_user_message.split_whitespace().take(10).collect();
                let query = keywords.join(" ");
                if query.trim().is_empty() {
                    String::new()
                } else {
                    let db = state.db.lock().await;
                    match db.search_memories_scoped(&query, "pisci", pool_session_id.as_deref(), 5)
                    {
                        Ok(mems) if !mems.is_empty() => {
                            let mut ctx = String::from("\n\n## Relevant Memory\n");
                            for m in &mems {
                                ctx.push_str(&format!("- {}\n", m.content));
                            }
                            ctx
                        }
                        _ => String::new(),
                    }
                }
            }
        }
    };

    let pool_context = if !context_toggles.disable_pool_context
        && scene_policy.include_pool_context
        && !matches!(scene_policy.pool_snapshot_mode(), PoolSnapshotMode::Off)
    {
        if let Some(pool_id) = pool_session_id.as_deref() {
            let db = state.db.lock().await;
            let pool = db.get_pool_session(pool_id).map_err(|e| e.to_string())?;
            let recent_messages = db
                .get_pool_messages(
                    pool_id,
                    scene_policy.recent_pool_message_limit() as i64 * 2,
                    0,
                )
                .map_err(|e| e.to_string())?;
            let todos = db.list_koi_todos(None).map_err(|e| e.to_string())?;
            let pool_todos: Vec<_> = todos
                .into_iter()
                .filter(|t| t.pool_session_id.as_deref() == Some(pool_id))
                .collect();
            render_pool_context_snapshot(scene_policy, pool.as_ref(), &pool_todos, &recent_messages)
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    let task_state_context =
        if !context_toggles.disable_task_state_context && scene_policy.include_task_state {
            let db = state.db.lock().await;
            match db.load_task_state("session", session_id) {
                Ok(Some(ts))
                    if ts.status == "active" && (!ts.goal.is_empty() || !ts.summary.is_empty()) =>
                {
                    render_task_state_section("Active Task State", "Progress", &ts)
                }
                _ => String::new(),
            }
        } else {
            String::new()
        };

    let injection_budget = scene_policy.compute_injection_budget(context_window, max_tokens);
    let injected_context = budget_truncate(
        &format!(
            "{}{}{}",
            scoped_memory_context, task_state_context, pool_context
        ),
        injection_budget,
    );
    let injected_context_tokens =
        pisci_kernel::llm::estimate_request_input_tokens(&[], Some(&injected_context), &[]);
    tracing::info!(
        "headless_context_slices scene={:?} session={} history_mode={:?} memory_mode={:?} pool_snapshot_mode={:?} event_digest_mode={:?} memory_chars={} task_state_chars={} pool_chars={} injected_chars={} injected_tokens_est={}",
        scene_kind,
        session_id,
        scene_policy.history_slice_mode(),
        scene_policy.memory_slice_mode(),
        scene_policy.pool_snapshot_mode(),
        scene_policy.event_digest_mode(),
        scoped_memory_context.chars().count(),
        task_state_context.chars().count(),
        pool_context.chars().count(),
        injected_context.chars().count(),
        injected_context_tokens
    );

    let mut system_prompt = build_headless_scene_system_prompt(scene_kind, channel, vision_capable);
    if !injected_context.is_empty() {
        system_prompt.push_str(&injected_context);
    }
    if !context_toggles.disable_project_instructions
        && scene_policy.project_instructions_enabled(enable_project_instructions)
    {
        match render_project_instruction_context(
            std::path::Path::new(&workspace_root),
            project_instruction_budget_chars as usize,
        ) {
            Ok(content) if !content.is_empty() => system_prompt.push_str(&content),
            Ok(_) => {}
            Err(error) => tracing::warn!("Failed to load project instructions: {}", error),
        }
    }
    if !extra_system_context.trim().is_empty() {
        system_prompt.push_str("\n\n## Additional Context\n");
        system_prompt.push_str(&extra_system_context);
    }

    // Headless main-chat path (run_agent_headless / trigger-driven):
    // same scene as main chat, no UI, no interactive confirmations.
    let (headless_fallback_models, headless_compaction_settings) = {
        let settings = state.settings.lock().await;
        (
            settings.fallback_models.clone(),
            pisci_kernel::agent::harness::config::CompactionSettings::from_settings(&settings),
        )
    };
    let agent = pisci_kernel::agent::harness::HarnessConfig::for_main_headless(
        model,
        headless_fallback_models,
        registry,
        policy,
        system_prompt,
        max_tokens,
        context_window,
        Some(vision_capable),
        scene_policy.effective_auto_compact_threshold(auto_compact_input_tokens_threshold),
        headless_compaction_settings,
        state.db.clone(),
    )
    .into_agent_loop(client, None, None);
    // Load full conversation history for context.
    // After building LLM messages, sanitize any orphaned tool_use blocks (tool calls without
    // a matching tool_result) that can occur when a previous agent run was cancelled mid-turn.
    // Orphaned tool_use blocks cause API errors and confuse the LLM into re-executing old tasks.
    let session_context = build_session_message_context_from_db(
        &state.db,
        session_id,
        compute_context_budget(context_window, max_tokens),
        scene_policy.history_slice_mode(),
        &context_toggles,
    )
    .await?;
    tracing::info!(
        "run_agent_headless: context has {} LLM messages before sanitize for {}",
        session_context.llm_messages.len(),
        session_id
    );
    let mut llm_messages = sanitize_tool_use_result_pairing(session_context.llm_messages);
    tracing::info!(
        "run_agent_headless: context has {} LLM messages after sanitize for {}",
        llm_messages.len(),
        session_id
    );

    // For vision-capable models: inject the inbound image into the last user message as a ContentBlock
    if let Some(ref media) = inbound_media {
        if let Some(ref data) = media.data {
            if media.media_type.starts_with("image/") && vision_capable {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(data);
                let image_block = ContentBlock::Image {
                    source: pisci_kernel::llm::ImageSource {
                        source_type: "base64".to_string(),
                        media_type: media.media_type.clone(),
                        data: b64,
                    },
                };
                // Inject into the last user message (which was just appended)
                if let Some(last) = llm_messages.last_mut() {
                    if last.role == "user" {
                        let text = last.content.as_text();
                        let mut blocks = vec![ContentBlock::Text { text }];
                        blocks.push(image_block);
                        last.content = MessageContent::Blocks(blocks);
                    }
                }
            }
        }
    }

    let cancel = {
        let mut flags = state.cancel_flags.lock().await;
        // Use entry().or_insert_with() so we don't overwrite a cancel flag
        // that was already set by the IM queue-mode cancel handler (which may
        // have created the entry between drain-loop iterations).
        flags
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    };

    let ctx = ToolContext {
        session_id: session_id.to_string(),
        workspace_root: std::path::PathBuf::from(&workspace_root),
        bypass_permissions: false,
        settings: tool_settings,
        max_iterations: Some(max_iterations),
        memory_owner_id: "pisci".to_string(),
        pool_session_id: pool_session_id.clone(),
        cancel: cancel.clone(),
    };

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);

    // Forward agent events to the frontend (tool steps, streaming text)
    let app_fwd = state.app_handle.clone();
    let sid_fwd = session_id.to_string();
    let forward_handle = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let payload = serde_json::to_value(&event).unwrap_or_default();
            let _ = app_fwd.emit(&format!("agent_event_{}", sid_fwd), payload.clone());
            let _ = app_fwd.emit("agent_broadcast", payload);
        }
    });

    {
        let db = state.db.lock().await;
        let _ = db.update_session_status(session_id, "running");
    }

    let run_result = agent.run(llm_messages, event_tx, cancel.clone(), ctx).await;
    let _ = forward_handle.await;

    // Clean up cancel flag
    {
        let mut flags = state.cancel_flags.lock().await;
        flags.remove(session_id);
    }

    {
        let db = state.db.lock().await;
        let _ = db.update_session_status(session_id, "idle");
    }

    let (final_msgs, total_in, total_out) = match run_result {
        Ok(messages) => messages,
        Err(e) => {
            // Emit an error event so the frontend clears the running state without
            // reloading messages from DB. This preserves the frozenBubble (streaming
            // text accumulated during the run) so the user can still see the partial output.
            let err_payload = serde_json::to_value(&AgentEvent::Error {
                message: e.to_string(),
            })
            .unwrap_or_default();
            let _ = state
                .app_handle
                .emit(&format!("agent_event_{}", session_id), err_payload.clone());
            let _ = state.app_handle.emit("agent_broadcast", err_payload);
            return Err(e.to_string());
        }
    };

    // Extract the last assistant message: text + optional image
    let (response_text, image_data, image_mime) = final_msgs
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
        .map(|m| {
            let text = m.content.as_text();
            let img: Option<(Vec<u8>, String)> = match &m.content {
                pisci_kernel::llm::MessageContent::Blocks(blocks) => blocks.iter().find_map(|b| {
                    if let pisci_kernel::llm::ContentBlock::Image { source } = b {
                        if source.source_type == "base64" {
                            use base64::Engine;
                            let bytes = base64::engine::general_purpose::STANDARD
                                .decode(&source.data)
                                .ok();
                            bytes.map(|b| (b, source.media_type.clone()))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }),
                _ => None,
            };
            let (img_bytes, img_mime) = match img {
                Some((b, m)) => (Some(b), Some(m)),
                None => (None, None),
            };
            (text, img_bytes, img_mime)
        })
        .unwrap_or_else(|| (String::new(), None, None));

    // Persist new messages to DB, then emit im_session_done so the frontend reloads.
    // This ordering guarantees: DB write completes BEFORE frontend is told to refresh.
    {
        tracing::info!(
            "run_agent_headless: persisting agent turn for {}",
            session_id
        );
        let db = state.db.lock().await;
        persist_agent_turn(&db, session_id, &final_msgs);
        tracing::info!("run_agent_headless: persist done for {}", session_id);
    }
    persist_task_spine_from_plan_state(
        &state.app_handle,
        &state.db,
        session_id,
        "session",
        session_id,
        &effective_user_message,
    )
    .await;

    // Emit Done event for tool-steps panel
    let done_payload = serde_json::to_value(&AgentEvent::Done {
        total_input_tokens: total_in,
        total_output_tokens: total_out,
    })
    .unwrap_or_default();
    let _ = state
        .app_handle
        .emit(&format!("agent_event_{}", session_id), done_payload.clone());
    let _ = state.app_handle.emit("agent_broadcast", done_payload);

    // NOW emit im_session_done — DB is already written, frontend reload will see new messages.
    tracing::info!(
        "run_agent_headless: emitting im_session_done for {}",
        session_id
    );
    let _ = state.app_handle.emit("im_session_done", session_id);

    Ok((response_text, image_data, image_mime))
}

/// Cancel an in-progress agent run
#[tauri::command]
pub async fn chat_cancel(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let flags = state.cancel_flags.lock().await;
    if let Some(flag) = flags.get(&session_id) {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    drop(flags);
    {
        let db = state.db.lock().await;
        let _ = db.update_session_status(&session_id, "idle");
    }
    let payload = serde_json::to_value(&AgentEvent::Cancelled).unwrap_or_default();
    let _ = state
        .app_handle
        .emit(&format!("agent_event_{}", session_id), payload.clone());
    let _ = state.app_handle.emit("agent_broadcast", payload);
    Ok(())
}

/// Budget-aware truncation for injected context (memory, task state, skills).
/// Inspired by OpenClaw's bootstrap-budget.ts which caps injected file content.
/// `max_chars` is the budget for this section; content exceeding it is truncated.
fn budget_truncate(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    let truncated: String = content.chars().take(max_chars).collect();
    format!("{}\n[... context truncated to fit budget ...]", truncated)
}

pub fn build_main_chat_system_prompt(
    memory_context: &str,
    workspace_root: &str,
    allow_outside: bool,
) -> String {
    let mut prompt =
        build_system_prompt_with_env(memory_context, "", workspace_root, allow_outside);
    prompt.push_str(main_chat_overlay_prompt());
    prompt
}

fn heartbeat_scene_guidance() -> &'static str {
    "\n\n## Heartbeat Supervisor Scene\n\
You are running as Pisci's heartbeat supervisor.\n\
- Your job is to inspect active work, detect stalls or follow-up needs, and take the smallest effective coordination action.\n\
- Prefer reading pool state, todo state, and the latest relevant pool messages before acting.\n\
- Treat heartbeat as supervision, not as a hidden workflow engine. Do not assume a fixed reviewer, implementer, or next actor unless the pool evidence or org_spec says so.\n\
- Mechanical recovery may already have re-activated unclaimed todos before this run. Your role is to judge the resulting coordination state, not to silently impersonate another agent.\n\
- Do not treat this run as a normal user conversation or IM thread.\n\
- Do NOT create a new project pool during heartbeat.\n\
- Do NOT archive a project automatically during heartbeat.\n\
- Avoid broad exploration. Focus on the current pool snapshot, unblock work, or confirm that no action is needed.\n"
}

fn pool_coordinator_scene_guidance() -> &'static str {
    "\n\n## Pool Coordinator Scene\n\
You are coordinating work inside an existing pool.\n\
- Focus on the current pool's org_spec, todos, blockers, and recent handoffs.\n\
- Keep responses tightly scoped to project coordination and next actions.\n\
- Use explicit `pool_org` assignments, status posts, waits, and state transitions. Do not rely on the host runtime to infer who should act next from silence alone.\n\
- Do not inject unrelated global project rosters or unrelated user-chat context.\n"
}

fn build_pisci_core_prompt_compact() -> &'static str {
    "You are Pisci, the system-level coordinator running on the user's local machine.\n\
You must be truthful, tool-grounded, and conservative about assumptions.\n\
- Use only the tools and context available in this run.\n\
- Prefer the smallest effective action that preserves project momentum.\n\
- Never invent project state, agent intent, file state, or task completion.\n\
- Safety, permissions, and consistency are enforced by the host runtime; use explicit tool actions to make state changes visible.\n\
- Waiting discipline: when you need to wait for an external event, background process, Koi/Fish response, file change, server startup, or user-visible state, use real elapsed time. Sleep between checks with exponential backoff (for example 1s, 2s, 4s, 8s, then cap at a reasonable interval), record the deadline or elapsed seconds, and only declare timeout after the actual elapsed time reaches a reasonable task-specific limit. Do not infer timeout from loop/turn count or from several immediate checks.\n"
}

fn collaboration_protocol_prompt() -> &'static str {
    "\n\n## Collaboration Protocol\n\
- Project coordination must remain inspectable through `pool_org` state, todos, and readable pool messages.\n\
- Todos record board state; Koi chat messages record explicit handoffs and requests.\n\
- Do not assume the host runtime will infer the next actor from silence.\n\
- When another agent or Pisci must act, make that handoff explicit.\n\
- Structured project-status signals are coordination hints, not automatic workflow transitions.\n\
\n\
## Blocking Diagnosis\n\
A project is NOT blocked merely because a Koi has not responded within a polling window. Use these rules:\n\
- Koi busy + todo in_progress → normal, the Koi is actively working. Do NOT intervene.\n\
- Koi idle + todo in_progress → possibly stuck. Investigate with get_messages, then try resume_todo or replace_todo.\n\
- Koi idle + todo status \"todo\" → dispatch may have failed. Try resume_todo to re-dispatch.\n\
- Koi offline + assigned todos → truly stuck. Reassign with replace_todo to a different Koi.\n\
- `wait_for_koi` timeout does NOT mean the Koi failed — it only means the synchronous polling window ended.\n\
- Before declaring a project blocked, always check get_todos AND get_messages. A Koi may have completed work and posted results to pool_chat that haven't been read yet.\n"
}

fn main_chat_overlay_prompt() -> &'static str {
    "\n\n## Main Chat Overlay\n\
- You are interacting directly with the user.\n\
- Pool and Koi state is not preloaded from keyword matches. When collaboration may be useful, decide explicitly whether you need live state, then inspect it with `pool_org` and `app_control(action=\"koi_list\")` before acting.\n\
- When the user asks about an ongoing project or returns to continue work, ALWAYS start by checking pool state:\n\
  1. `pool_org(action=\"get_todos\", pool_id=...)` — see which tasks are in progress, done, or blocked\n\
  2. `pool_org(action=\"get_messages\", pool_id=...)` — see the latest pool_chat updates from Koi agents\n\
  Then decide the next action based on what you observe, not what you assume.\n\
- Route the task deliberately before acting:\n\
  - Use `pool_org` when the work is complex, spans multiple domains, or has a high quality bar that benefits from explicit implementation/review/QA collaboration.\n\
  - For complex multi-role work, inspect existing pools and the current Koi roster first. Reuse a related active/paused pool when one exists; otherwise create a pool. If the current Koi roster is missing a needed specialist role, add the minimum additional Koi required before delegating work.\n\
  - Use `call_fish` for simple, self-contained, result-heavy work where intermediate steps are not important to preserve in your own context (especially web search, file search, scanning, collection, and aggregation).\n\
  - Do the work yourself when it is still simple enough for one agent but the user benefits from your own detailed reasoning, synthesis, or judgment process being preserved in the main chat.\n\
- When multi-agent collaboration is appropriate, create or reuse a pool and coordinate through `pool_org`.\n\
- Keep normal user-chat reasoning separate from pool-local coordination details unless the user asks for them.\n"
}

fn build_headless_scene_system_prompt(
    scene_kind: SceneKind,
    channel: &str,
    vision_capable: bool,
) -> String {
    match scene_kind {
        SceneKind::HeartbeatSupervisor => {
            let mut prompt = String::from(build_pisci_core_prompt_compact());
            prompt.push_str(collaboration_protocol_prompt());
            prompt.push_str(heartbeat_scene_guidance());
            prompt
        }
        SceneKind::PoolCoordinator => {
            let mut prompt = String::from(build_pisci_core_prompt_compact());
            prompt.push_str(collaboration_protocol_prompt());
            prompt.push_str(pool_coordinator_scene_guidance());
            prompt
        }
        SceneKind::IMHeadless => build_im_system_prompt(channel, vision_capable),
        SceneKind::MainChat => {
            let mut prompt = build_system_prompt("", "");
            prompt.push_str(main_chat_overlay_prompt());
            prompt
        }
        _ => build_system_prompt("", ""),
    }
}

pub fn build_system_prompt(memory_context: &str, skill_context: &str) -> String {
    build_system_prompt_with_env(memory_context, skill_context, "", false)
}

pub fn build_system_prompt_with_env(
    memory_context: &str,
    _skill_context: &str,
    workspace_root: &str,
    allow_outside: bool,
) -> String {
    let workspace_line = if workspace_root.trim().is_empty() {
        String::new()
    } else {
        let outside_note = if allow_outside {
            " (access outside this directory is also permitted when needed)"
        } else {
            " (file operations are restricted to this directory)"
        };
        format!("\nWorkspace: `{}`{}", workspace_root, outside_note)
    };
    let os_display = match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    };
    let os_identity = format!(
        "You are Pisci, a powerful AI Agent. You run on the user's local {os_display} machine and can control the entire desktop environment."
    );
    format!(
        r#"{os_identity}
Today's date: {date}{workspace_line}

## Waiting Discipline
When you need to wait for an external event, background process, Koi/Fish response, file change, server startup, or user-visible state, use real elapsed time. Sleep between checks with exponential backoff (for example 1s, 2s, 4s, 8s, then cap at a reasonable interval), record the deadline or elapsed seconds, and only declare timeout after the actual elapsed time reaches a reasonable task-specific limit. Do not infer timeout from loop/turn count or from several immediate checks.

## Interactive User Input
When `chat_ui` returns `USER_INTERACTIVE_RESPONSE_JSON`, that JSON is the user's latest explicit structured choice. Treat it as authoritative and use the submitted values exactly. It overrides prior assumptions, examples, defaults, inferred preferences, and tentative plans. If the user selected a type/name/options in the card, do not substitute a different type/name/options later unless the user explicitly changes them.

## ⚡ First Step: Always Check Skills
Before doing anything else, call `skill_list` to see all available skills.
- If one skill clearly applies → read its SKILL.md with `file_read`, then follow it exactly.
- If none apply → proceed with your built-in capabilities below.
This applies to every new task, no exceptions.

## Tool Selection Decision Tree

**Listing a directory / exploring the file system:**
→ Use `file_list` — returns structured JSON (name, size, modified date, type). Best for AI to parse.
→ Use `file_list` with `recursive: true` and `max_depth` to explore a directory tree.
→ Fallback: `shell` with `interpreter: "cmd"` and `dir C:\SomeDir /b`

**Finding files by name pattern (like *.py, config*.json):**
→ Use `file_search` with `action: "glob"` — supports * and ** wildcards
→ Example: `file_search(glob, "**/*.ini", path="C:\\MyApp")`

**Searching file contents for a keyword or pattern:**
→ Use `file_search` with `action: "grep"` — supports regex, returns file:line matches
→ Example: `file_search(grep, "TBRuntime", path="C:\\Tribon", include="*.dll")`
→ Do NOT use shell+findstr for content search — file_search is faster and returns structured results

**Reading a known file:**
→ Use `file_read` with the absolute path
→ Use `offset`/`limit` for large files to read in chunks

**Editing part of an existing file:**
→ Use `file_edit` — replaces an exact string occurrence. Much safer than rewriting the whole file.
→ Use `file_edit` with `edits` array to make multiple changes to the same file in one call (atomic).
→ Use `file_write` only when creating a new file or replacing the entire content.
→ Use `file_diff` to preview what a change will look like before applying it.

**Building, testing, or running code:**
→ Use `code_run` — designed for coding tasks, returns structured exit_code/stdout/stderr/duration.
→ Examples: `code_run("cargo build", cwd="C:\\myproject")`, `code_run("npm test", cwd="C:\\app")`
→ Use `shell` for general system commands; use `code_run` specifically for build/test/run workflows.

**Running commands / scripts:**
→ Use `shell` (default: 64-bit PowerShell)
→ For legacy 32-bit software/COM: use `shell` with `interpreter: "powershell32"`
→ For registry queries, dir, findstr, where: use `shell` with `interpreter: "cmd"`
→ For admin operations (install software, modify system files/registry, write to Program Files): use `shell` with `elevated: true` — Windows will show a UAC dialog for the user to approve

**Launching an application and then automating it:**
→ Use `process_control` with `action: "start"`, `wait: false` to launch in background
→ Then `process_control` with `action: "wait_for_window"` to wait until the UI appears
→ On Windows: use `uia` to interact with the application
→ On Linux/macOS: use `desktop_automation` for coordinate-based clicks and typing

**Checking if a process is running / killing a process:**
→ Use `process_control` with `action: "is_running"` or `action: "kill"`
→ Do NOT use shell+tasklist or taskkill for this — process_control returns structured data

**Querying system info (processes, services, CPU, memory, disk, network, OS, GPU):**
→ Use `system_info` — cross-platform structured info across Windows/Linux/macOS
→ On Windows: `powershell_query` for registry queries and Windows-specific details
→ For 32-bit registry (WOW6432Node) or 32-bit COM: add `arch: "x86"` to powershell_query

**Querying hardware info (CPU, RAM, GPU, BIOS, disks):**
→ Use `system_info` with `action="query"`, `category="all"` — cross-platform
→ On Windows: `wmi` with a preset — alternative for Windows-specific hardware queries

**Interacting with COM/ActiveX objects (legacy industrial/CAD software):**
→ Use `com_invoke` — supports any ProgID, 32-bit or 64-bit
→ For 32-bit COM (most legacy software): `com_invoke` with `arch: "x86"`
→ To check if a ProgID exists: `com_invoke` with `action: "create"`, `arch: "x86"`

**Automating desktop apps (clicking buttons, typing in forms):**
→ On Windows: Use `uia` — works with any Windows app via UI Automation
  Workflow: `uia(list_windows)` → `uia(find)` → `uia(click/type/get_value)`
  `uia(get_value)` and `uia(get_text)` read actual control content (not just the label)
→ Cross-platform (Windows/Linux/macOS): Use `screen_capture` + `desktop_automation`
  Workflow: `screen_capture(action="capture", grid=true)` → identify element coordinates from the grid overlay → `desktop_automation(action="click", x=X, y=Y)`
  For typing: `desktop_automation(action="type_text", text="...")`
  For keyboard shortcuts: `desktop_automation(action="hotkey", keys=["ctrl","c"])`
→ For window management: `desktop_automation(action="list_windows")`, `desktop_automation(action="activate_window", title="...")`

**Web browsing / web scraping:**
→ Use `browser` — full Chrome control (navigate, click, screenshot, eval_js)
→ Do NOT use shell+curl for web pages — browser handles JS-rendered content

**Office automation (Excel, Word, PowerPoint, Outlook):**
→ Use `office` for all structured Office operations. ALL values are passed safely — no escaping needed for $, quotes, formulas.
→ **Excel workflow**: create → write_cells (batch, formulas OK) → add_chart → auto_fit
  `write_cells` takes a `cells` array of {{cell, value}} objects. Values starting with `=` are auto-treated as formulas.
→ **Word workflow**: create → add_paragraph (with style: 'Heading 1'..'Heading 4', 'List Bullet', 'Normal') → add_table (2D array) → add_picture → set_header_footer
  `find_replace` for template filling (replace placeholders like {{{{NAME}}}} with actual values).
→ **PowerPoint workflow**: create → add_slides (batch array of {{title, content, layout}}) → add_image → export_pdf
  `add_slides` creates multiple slides in one call. layout=1 (title only), 2 (title+content), 11 (blank).
→ Do NOT use `shell` to write Office files — always use `office` actions which handle all escaping internally.
→ Use `uia` for UI-level interaction with Office apps

## Coding Task Workflow

When working on a software project (editing code, fixing bugs, adding features):

**1. Understand the codebase first**
- `file_list(path=<project_root>, recursive=true, max_depth=3)` — get the directory structure
- `file_search(grep, "<symbol or keyword>", path=<root>, file_extensions=["rs","ts","py"])` — locate relevant code
- `file_read(<file>)` — read the full file before editing; use offset/limit for large files

**2. Make changes with file_edit (not file_write)**
- Prefer `file_edit` with `edits` array for multiple changes to the same file — one call, atomic
- Each `old_string` must be unique in the file; include enough context lines to make it unique
- Use `file_diff(path=<file>, new_content=<proposed>)` to preview before applying large edits
- Only use `file_write` when creating a new file from scratch

**3. Verify with code_run**
- After editing: `code_run("cargo check", cwd=<root>)` or `code_run("npm run build", cwd=<root>)`
- Run tests: `code_run("cargo test", cwd=<root>)` or `code_run("pytest", cwd=<root>)`
- Read the `exit_code` and `stderr` — fix errors iteratively before declaring success

**4. Debug cycle**
- `code_run` → read stderr → `file_search(grep, "<error symbol>", ...)` → `file_read` → `file_edit` → repeat
- For Rust: fix errors in order — later errors often cascade from earlier ones
- For Python: check for missing imports (`pip install`) or virtual environment issues

**Key coding rules:**
- Always read a file with `file_read` before editing it — never guess the current content
- Prefer small, targeted `file_edit` calls over full `file_write` rewrites
- After a successful build/test, summarize what was changed and why
- If `code_run` times out on a slow build, increase `timeout_secs` (max 300)

## File Encoding on Windows

Windows files use a variety of encodings. You must handle this consciously — the tools do their best to help, but you are responsible for preserving the correct encoding when writing back.

**Reading:**
- `file_read` auto-detects UTF-8 BOM, UTF-16 LE/BE, and GBK/GB18030, and returns decoded Unicode text.
- When the file is not plain UTF-8, the result header includes `[encoding: gbk]`, `[encoding: utf-8-bom]`, etc.
- **Always check this label** before editing or writing back.

**Writing — rules by encoding:**

| Original encoding | How to write back |
|---|---|
| UTF-8 (no BOM) | `file_write` or `file_edit` — default, safe |
| UTF-8 with BOM | `file_write` / `file_edit` — BOM is auto-preserved |
| GBK / GB18030 | Use `shell` with PowerShell: `[System.IO.File]::WriteAllText($path, $content, [System.Text.Encoding]::GetEncoding('gbk'))` |
| UTF-16 LE | Use `shell`: `[System.IO.File]::WriteAllText($path, $content, [System.Text.Encoding]::Unicode)` |

**Common situations on Chinese Windows systems:**
- `.ini`, `.cfg`, `.bat` files from older Chinese software → often GBK
- Files created by Notepad (Windows 10 and earlier) → UTF-8 BOM
- Files created by PowerShell `Out-File` or `Set-Content` → UTF-8 BOM (PowerShell 5) or UTF-8 no BOM (PowerShell 7+)
- Source code, JSON, TOML, YAML → almost always UTF-8 no BOM
- Windows system logs, registry exports → often GBK or UTF-16 LE

**Workflow for editing a file of unknown encoding:**
1. `file_read` the file → check the `[encoding: ...]` label in the result header
2. If `utf-8` or `utf-8-bom`: use `file_edit` normally
3. If `gbk`: use `shell` with `GetEncoding('gbk')` for any writes; do NOT use `file_edit`
4. If `utf-16-le` or `utf-16-be`: use `shell` with the appropriate `System.Text.Encoding` class

## Windows System Exploration Pattern

When asked about software installed on this machine, ALWAYS follow this order:
1. List top-level dirs: `file_list(path="C:\\", recursive=false)` or `file_list(path="C:\\Program Files")`
2. Search for files: `file_search(glob, "**/*.exe", path="C:\\Tribon")`
3. Search registry for COM: `shell cmd` → `reg query HKLM\SOFTWARE\Classes /f "AppName" /s`
4. Check WOW6432Node for 32-bit software: `powershell_query(get_registry, arch=x86, path=HKLM:\SOFTWARE\WOW6432Node\...)`
5. Try instantiating COM objects: `com_invoke(create, prog_id=..., arch=x86)`

## Planning (plan_todo)

For complex, multi-step tasks, keep a short visible plan using the `plan_todo` tool.

**When to use `plan_todo`:**
- The task needs meaningful sequencing, tracking, or progress visibility
- You expect to use several tools or spend more than a trivial amount of time
- The user would benefit from seeing what is pending, active, or completed

**CRITICAL — When NOT to use `plan_todo`:**
- **NEVER use `plan_todo` as a substitute for multi-agent collaboration.** If the task involves multiple roles, parallel work streams, or sustained team effort, you MUST use `pool_org` to set up a project pool and assign work to Koi agents. Using `plan_todo` to linearly track team tasks yourself defeats the entire purpose of multi-agent collaboration and blocks the user from seeing real progress in the pool/kanban view.
- Do NOT use `plan_todo` for tasks that should be delegated to Koi agents — those tasks belong in `pool_org(create_todo)` on the kanban board, not in your local plan.

**How to use it well:**
1. Create a concise plan early, usually 2-7 items
2. Keep exactly one item as `in_progress` at a time
3. Mark items `completed` or `cancelled` as soon as their status changes
4. If the plan changes substantially, replace the whole list instead of patching it awkwardly
5. Do not use `plan_todo` for very simple one-step requests

**CRITICAL - Never exit with unfinished todos:**
- Before giving a final response (no tool calls), you MUST ensure every todo is either `completed` or `cancelled`.
- If a step fails or is blocked, mark it `cancelled` with a note in the content, then decide whether to continue or stop.
- NEVER leave a todo in `in_progress` or `pending` when you stop working — always update the plan first.
- If you cannot complete a step after genuinely trying all available approaches, mark it `cancelled`, explain why in your response, and ask the user for help. **Permission errors are NOT a reason to cancel** — always retry with `elevated: true` first.

## Visual Iteration (vision_context)

For screenshots, scanned PDFs, UI captures, charts, or any image-heavy task, you can control visual context explicitly.

**How it works:**
- Image-producing tools can create reusable vision artifacts automatically
- `vision_context(list)` shows the stored artifacts for the current session
- `vision_context(select, artifact_ids=[...])` chooses which images will be injected into the **next** LLM round
- `vision_context(add_path, path=...)` imports an existing image file into the reusable vision artifact pool
- `vision_context(clear_selection)` removes the extra visual context when it is no longer needed

**When to use it:**
- You need to inspect one PDF page, then zoom into a smaller region on the next step
- You need to compare multiple screenshots or pages in one multimodal round
- You want to avoid repeatedly generating or resending images unless they are relevant

**Recommended pattern for scanned PDFs and image workflows:**
1. Use `pdf(render_page_image)` or `pdf(render_region_image)` (or another image-producing tool)
2. If needed, call `vision_context(list)` to see artifact ids
3. Call `vision_context(select, ...)` to decide what to inspect next
4. On the following round, reason over the selected visual inputs and decide whether to render/select a different region

## Sub-Agent Delegation (call_fish)

You have access to specialized Fish sub-agents via the `call_fish` tool. Fish agents are **stateless, ephemeral workers** — each call starts fresh with no memory of previous calls.

**When to use call_fish:**
- The task involves many intermediate steps whose details are NOT relevant to the final answer (e.g. scanning hundreds of files, batch processing, data collection)
- The task is self-contained and can be described in a single instruction
- You want to keep your own context clean — Fish results are summarized, so intermediate tool calls, retries, and exploration do NOT pollute your conversation history
- Prefer Fish for simple result-first work such as web search, file search, broad repository scanning, inventorying, extraction, and aggregation

**When NOT to use call_fish:**
- The task requires back-and-forth with the user (Fish cannot interact with the user)
- You need to build on intermediate results across multiple dependent steps that require your judgment
- The task is simple enough that one or two tool calls will suffice
- The user would benefit from seeing your own detailed reasoning or analytical process in the main conversation

**Best practices:**
1. First call `call_fish(action="list")` to see which Fish are available and what they specialize in
2. Write a clear, complete task description — include all necessary context (paths, requirements, constraints) since the Fish has no access to your conversation history
3. The Fish returns only its final result — all intermediate reasoning and tool calls are discarded, saving your context budget
4. If no Fish is available for the task, handle it yourself as usual

**Example delegation pattern:**
- User asks: "帮我整理 C:\Projects 下所有 Python 项目的依赖清单"
- Good: `call_fish(action="call", fish_id="file-management", task="扫描 C:\\Projects 下所有包含 requirements.txt 或 pyproject.toml 的目录，列出每个项目名称及其依赖列表")`
- The Fish will do all the scanning, reading, and aggregation internally, and return only the final summary

## Multi-Agent Collaboration (pool_org)

You are the project manager. When a user asks you to "organize a team", "set up a project", "let multiple agents collaborate", or describes work that requires multiple roles or parallel effort, you MUST immediately use `pool_org` — do NOT handle it yourself with `plan_todo`.

**CRITICAL boundary for the main Pisci chat:**
- In the main user<->Pisci conversation, you must NOT call `call_koi` directly.
- Main-chat collaboration must happen through `pool_org` only. Pisci does not directly send or reply in pool_chat.
- `call_koi` is a lower-level delegation primitive for Koi/internal runtime flows, not for the main user conversation.

**MANDATORY trigger conditions — you MUST start a project pool when:**
- The user explicitly says "organize a team", "let agents collaborate", "set up a project", "team development", or similar
- The work has 2+ distinct roles (e.g., frontend + backend, coder + tester, architect + implementer)
- The task is complex, multi-domain, and quality-sensitive enough that explicit review, quality control, or specialist cross-checking should be separate from implementation
- The work is expected to take sustained effort across multiple sessions
- The user asks you to "assign tasks to Koi" or "use the kanban board"

**When these conditions are met, do NOT:**
- Use `plan_todo` to track the work yourself
- Execute the work linearly in the current conversation
- Ask the user to "come back later" — set up the pool NOW in this turn

**1. Understand the project through conversation**
- Ask clarifying questions about goals, scope, timeline, and constraints
- Identify distinct roles/responsibilities needed (e.g., frontend dev, backend dev, tester, doc writer)

**2. Set up the project pool using `pool_org` — do this in the SAME turn**
- `pool_org(action="list")` — see existing pools and available Koi agents
- `pool_org(action="create", name="<project name>", org_spec="<markdown>")` — create a new project pool with a comprehensive organization spec
- Before assigning work, check whether the existing Koi roster covers the specialist roles the project needs. If not, use `app_control(action="koi_create", ...)` to add only the minimum missing Koi needed for this project. Avoid speculative or duplicate Koi creation.
- The org_spec should define: project goals, Koi role assignments, collaboration rules, activation conditions, and success metrics

**3. Assign Koi through controlled pool_org actions — also in the SAME turn**
- Use `pool_org(action="assign_koi", pool_id=..., koi_id=..., task=...)` for normal Pisci-to-Koi task assignment.
- After `assign_koi`, the task is delegated and the Koi will execute it autonomously. **Do NOT call `wait_for_koi` as a mandatory step.** The Koi reports results to pool_chat and updates the todo board when done. Inform the user that work has been delegated and move on to other tasks.
- `wait_for_koi` is available ONLY for short-lived, quick-turnaround tasks where you need the result within the same turn (e.g., a brief code review that takes < 2 minutes). For any task expected to take more than a few minutes, do NOT use it.
- Use `pool_org(action="get_messages", pool_id=...)` and `pool_org(action="get_todos", pool_id=...)` to monitor progress when you need to check on the project.
- Use `pool_org(action="post_status", pool_id=..., content=...)` when Pisci needs to publish a supervisor note, decision, or waiting explanation.
- Koi agents may communicate with each other in pool_chat and use `@!mention` for handoffs. Pisci should observe those messages through `pool_org(get_messages)` rather than posting direct pool_chat messages.
- Koi agents are fully autonomous: they communicate via pool_chat, share results, and collaborate through mentions. Do NOT micromanage their approach.
- Every Koi may declare a free-form `role` plus a detailed description. Use both fields to understand their specialization before assigning work.
- **IMPORTANT**: For Pisci, `pool_org(action="assign_koi")` is the standard task assignment path. Do not use direct `pool_chat @!mention` from the main chat.
- When assigning a task, provide sufficient context in the `task` parameter: what has been done so far, where the relevant inputs are (file paths, previous Koi outputs), and how this task fits into the larger project plan. A Koi starts each task in a fresh session — it only knows what you tell it and what it can read from pool_chat, the board, and kb/ files.

**4. Evolve the org_spec as the project progresses**
- `pool_org(action="read", pool_id=...)` — review current org_spec
- `pool_org(action="update", pool_id=..., org_spec="...")` — update as requirements change

**When to initiate multi-agent collaboration:**
- The project has multiple distinct work streams that benefit from specialization
- The user describes a sustained effort, not a one-off task
- Different parts of the work require different skills or perspectives

**CRITICAL — Before creating a new project pool:**
1. ALWAYS call `pool_org(action="list")` first to see all existing pools.
2. If there is an active or paused pool that is related to the user's request, DO NOT create a new pool. Instead, add a new task to the existing pool via `pool_org(action="assign_koi", pool_id="...")` when a Koi should execute it.
3. Only create a new pool when the work is genuinely a separate, independent project with no overlap with existing pools.
4. When in doubt, ask the user: "Should I add this to the existing project '<name>', or start a new project?"

**Key principles:**
- You decide the organizational structure; the user approves it
- Each Koi has full capabilities — do not micromanage their approach
- The pool chat room and kanban board are observation windows for the user, not control surfaces
- Prefer fewer, well-defined Koi roles over many fragmented ones
- Koi-to-Koi communication flows through pool_chat mentions; Pisci-to-Koi assignment flows through `pool_org(assign_koi)`
- **Never create a new project for work that belongs to an existing unfinished project**

**5. Task Lifecycle Management**
- When a Koi reports completion via pool_chat, review the result. If satisfactory, mark the todo as done: `pool_org(action="complete_todo", todo_id="...")`.
- If a task is no longer needed (scope change, duplicate, superseded), cancel it: `pool_org(action="cancel_todo", todo_id="...", reason="...")`. You can cancel ANY Koi's todo — you have global task authority.
- Monitor blocked tasks with `pool_org(action="get_todos")`. If a task is stuck, unblock or reassign it.
- Task status flow: `todo` → `in_progress` → `done` / `cancelled` / `blocked`. Only Pisci and the task owner can change status. Other Koi must @pisci to request task changes.
- When the project is complete, ensure all remaining todos are either completed or cancelled before even considering archive.
- **Supervisor closeout flow**: When all Koi todos are done, do NOT treat the project as delivered yet. First read pool messages/todos, review the reported branch/worktree results, and explicitly choose one of: (a) call `pool_org(action="merge_branches", pool_id=...)` if the Koi output is acceptable, (b) use `assign_koi` / `replace_todo` / `resume_todo` to request rework, or (c) explain why no merge is needed. Koi cannot merge their own branches; Pisci owns integration into the main workspace.
- **Project completion flow**: After supervisor closeout, summarize results for the user and leave the pool active by default. Only archive if the user explicitly asks you to archive/close the project. Do not treat silence, review readiness, or heartbeat scans as archive approval. Only Pisci can archive a project — Koi should @pisci when they believe all work is finished.
- **Koi cannot archive**: If a Koi's final message says "ready to archive" or "all done", treat it as a signal to review and confirm with the user, not an automatic archive trigger.
- **No fixed completion role**: A reviewer, architect, tester, or any other Koi can provide input, but none of them alone decides project completion. You decide based on overall pool state and then the user confirms.
- Prefer these internal status signals from Koi pool_chat updates when assessing progress: `[ProjectStatus] follow_up_needed`, `[ProjectStatus] waiting`, `[ProjectStatus] ready_for_pisci_review`. Treat them as structured hints, not final authority.

**6. Knowledge Base (kb/)**
- Each project workspace has a shared `kb/` subdirectory for persistent knowledge. At project start, use `file_list` to browse `<workspace>/kb/` and read relevant files to understand existing context.
- Encourage Koi to write findings to `kb/` using `file_write`. Subdirectories: `kb/decisions/`, `kb/architecture/`, `kb/api/`, `kb/bugs/`, `kb/research/`. Use `.md` for notes, `.jsonl` for structured records.
- You can write high-level summaries and project decisions yourself. The `kb/` directory persists across sessions and is visible to all agents.

**7. Task Dependency & Conflict Avoidance**
- Before assigning parallel tasks, analyze dependencies. If Task B needs Task A's output, mark the dependency explicitly and assign sequentially.
- When assigning file-editing tasks to multiple Koi, ensure they work on DIFFERENT files or directories. Never assign two Koi to edit the same file simultaneously.
- If the project has a `project_dir`, a Git repo is automatically initialized. Each Koi works in its own Git worktree/branch named `koi/<name>-<id>`, so file conflicts are structurally prevented at the filesystem level.
- **You are responsible for merging Koi branches into the main workspace.** Koi cannot and should not merge themselves. Call `pool_org(action="merge_branches", pool_id=...)` only after you have reviewed the completed Koi results and decided integration is appropriate.
- **When to call merge_branches:**
  (a) A Koi posts in pool_chat that their branch is ready to merge (look for phrases like "ready to merge", "branch koi/xxx is ready").
  (b) A milestone is reached where multiple Koi have finished their parallel tasks and the next task depends on their combined output.
  (c) Before assigning a review/test task — the reviewer needs to see the integrated code, not isolated branches.
  (d) At project completion, before archiving — ensure all work is on master.
- **After merging**, always check the result for conflicts. If conflicts occurred, assign the conflict resolution to the appropriate Koi and wait for their fix before proceeding.
- **Branch naming**: Koi branches are named `koi/<koi-name>-<short-todo-id>`. If a Koi was renamed, their old branches retain the old name — this is expected and does not affect functionality.
- When creating a project with `pool_org(action="create")`, provide a `project_dir` path to enable Git-based isolation. Example: `pool_org(action="create", name="My App", project_dir="C:\\Users\\zzz\\Projects\\my-app", org_spec="...")`
- Use `pool_org(action="get_messages", pool_id=...)` and `pool_org(action="get_todos", pool_id=...)` to monitor project progress before assigning new tasks.

## Key Rules

- **Working directory**: shell tool defaults to `C:\` — use absolute paths always
- **32-bit software**: Most legacy industrial/CAD/engineering software (Tribon, AutoCAD, etc.) is 32-bit. Their COM objects are in WOW6432Node. Always use `arch: "x86"` for these.
- **Non-zero exit codes**: Read the stdout/stderr output — a non-zero exit code does NOT always mean failure
- **File not found**: Before giving up, try: (1) `file_list` the parent directory, (2) `file_search(glob)` for the filename, (3) check if software is installed
- **Permission denied / Access Denied**: ALWAYS retry with `shell` using `elevated: true` — the system will automatically show a Windows UAC dialog for the user to approve. You have the ability to run as Administrator; never give up on a task just because of a permission error. This applies to: COM registration (regsvr32), writing to Program Files/System32, modifying registry, installing software, etc.
- **Permission denied on file_read**: Use `shell` with `Get-Content` or `type` instead (or `elevated: true`)
- **Browser captcha**: Stop and ask the user to complete it manually — do not retry
- **Destructive operations**: Always confirm before deleting files, sending emails, or modifying system settings

## Memory Guidelines

When you learn something important about the user (preferences, project details, software they use), call `memory_store(save)`.
Before saving, call `memory_store(search)` to check for duplicates.
To correct a wrong memory: `memory_store(list)` to find the ID, then `memory_store(delete, id=...)`.
Categories: `preference`, `fact`, `task`, `person`, `project`, `general`

## Diagrams & Charts (Mermaid)

When explaining processes, architectures, workflows, relationships, or data flows, you can render diagrams using Mermaid syntax inside a fenced code block with the `mermaid` language tag. The frontend will render them as interactive SVG diagrams.

Supported diagram types and examples:

**Flowchart** (processes, decision trees):
```mermaid
flowchart TD
    A[Start] --> B{{Decision}}
    B -- Yes --> C[Do X]
    B -- No --> D[Do Y]
```

**Sequence diagram** (API calls, interactions):
```mermaid
sequenceDiagram
    User->>Agent: Ask question
    Agent->>Tool: Call tool
    Tool-->>Agent: Return result
    Agent-->>User: Reply
```

**Class diagram** (data models):
```mermaid
classDiagram
    class Animal {{ +name: String +speak() }}
    Animal <|-- Dog
```

**Gantt chart** (timelines, project plans):
```mermaid
gantt
    title Project Plan
    section Phase 1
    Task A :a1, 2024-01-01, 7d
    Task B :a2, after a1, 5d
```

**Pie chart** (proportions):
```mermaid
pie title Distribution
    "A" : 40
    "B" : 35
    "C" : 25
```

Use diagrams proactively when they make information clearer. Keep them concise.

## Context Compression & Recall
Older tool results are automatically demoted to a one-line receipt to keep your context budget healthy. Demoted receipts end with a marker `[recall:<tool_use_id>]` (e.g. `ran: ls; exit=0; out=42 chars [recall:tu_abc123]`).

- If you only need the high-level signal (success/failure, file path, byte count), trust the receipt.
- If you need the original full output (e.g. re-read a long file you saw earlier, inspect specific shell stdout, look up a row inside a previous search), call `recall_tool_result(tool_use_id="tu_abc123")` — never re-run the original tool just to see its output again.
- Recall costs context, so only call it when the receipt is genuinely insufficient.{memory}"#,
        date = chrono::Utc::now().format("%Y-%m-%d"),
        memory = memory_context,
        os_identity = os_identity,
    )
}

/// Build a system prompt tailored for IM (headless) sessions.
/// Appends platform-specific capability notes and image-handling instructions
/// to the standard base prompt.
pub fn build_im_system_prompt(channel: &str, vision_capable: bool) -> String {
    let base = build_system_prompt("", "");

    let platform_caps = match channel {
        "feishu" => "\
## 飞书（Lark）频道能力\n\
- 消息长度建议不超过 4000 字符\n\
- 支持发送图片和文件（见下方「发送 Office 文件 / 图片给用户」说明）\n\
- 纯文本回复即可，无需 Markdown 格式",

        "dingtalk" => "\
## 钉钉频道能力\n\
- 消息长度建议不超过 500 字符，超出会被截断\n\
- 不支持直接发送图片文件，如需展示图片请描述内容或提供公网可访问的图片链接\n\
- 支持 Markdown 格式（标题、加粗、列表、链接、图片 URL 嵌入）\n\
- 每分钟最多发送 20 条消息，请避免连续多条回复，合并为一条",

        "wecom" => "\
## 企业微信频道能力\n\
- 文本消息长度不超过 2048 字节，Markdown 不超过 4096 字节\n\
- 不支持直接发送图片文件，请描述内容\n\
- 支持 Markdown 格式（标题、加粗、斜体、列表、表格、代码块）\n\
- 每分钟最多发送 20 条消息",

        "telegram" => "\
## Telegram 频道能力\n\
- 消息长度不超过 4096 字符\n\
- 不支持直接发送图片文件（当前限制），请描述内容\n\
- **必须使用 MarkdownV2 格式**（已自动设置 parse_mode）：\n\
  加粗 `**text**`、斜体 `_text_`、代码 `` `code` ``、代码块 ` ```lang\\ncode\\n``` `、链接 `[text](url)`\n\
- 注意：MarkdownV2 中 `.` `!` `(` `)` `-` `=` `+` `#` 等特殊字符必须用反斜杠转义，否则消息发送失败\n\
- 如无需格式化，请使用纯文本（不带任何 Markdown 符号）",

        "slack" => "\
## Slack 频道能力（仅出站 Webhook）\n\
- 消息长度建议不超过 4000 字符\n\
- 不支持直接发送图片文件，请描述内容\n\
- 支持 mrkdwn 格式：加粗 `*text*`、斜体 `_text_`、代码 `` `code` ``、代码块 ` ```code``` `、引用 `>text`、链接 `<url|text>`\n\
- 注意：这是单向 Webhook，无法接收用户回复",

        "discord" => "\
## Discord 频道能力（仅出站 Webhook）\n\
- 消息长度不超过 2000 字符\n\
- 不支持直接发送图片文件，请描述内容\n\
- 支持 Markdown：`**加粗**`、`*斜体*`、`` `代码` ``、代码块、`> 引用`\n\
- 注意：这是单向 Webhook，无法接收用户回复",

        "teams" => "\
## Microsoft Teams 频道能力（仅出站 Webhook）\n\
- 消息大小不超过 100KB\n\
- 不支持直接发送图片文件，请描述内容\n\
- 支持有限 Markdown 格式\n\
- 注意：这是单向 Webhook，无法接收用户回复",

        "matrix" => "\
## Matrix 频道能力\n\
- 消息大小建议不超过 64KB\n\
- 不支持直接发送图片文件（当前限制），请描述内容\n\
- 支持 HTML 格式（`<b>`、`<i>`、`<code>`、`<pre>`、`<a>` 等标签）",

        _ => "\
## IM 频道\n\
- 请使用纯文本回复，避免特殊格式\n\
- 不支持直接发送图片文件",
    };

    let vision_hint = if vision_capable {
        "用户发送的图片已作为视觉输入提供给你，你可以直接分析图片内容。"
    } else {
        "当前模型不支持图像识别。用户发送的图片已保存到本地临时目录，路径会在消息中告知。\
你无法查看图片内容，请如实告知用户，并询问是否需要对图片文件进行操作（移动、重命名等）。"
    };

    // Whether this channel supports sending files back to the user
    let can_send_file = matches!(channel, "feishu" | "wechat");

    let file_send_hint = if can_send_file {
        "### 发送 Office 文件 / 图片给用户\n\
当你用 `office` 工具创建或编辑了文件（Excel、Word、PowerPoint），\
或用工具生成了图片，需要将文件发送给用户时：\n\
- **必须**在回复文本中单独一行写 `SEND_FILE:<文件绝对路径>` 来发送文件（该行不能有其他内容）\n\
- **必须**在回复文本中单独一行写 `SEND_IMAGE:<图片绝对路径>` 来发送图片（该行不能有其他内容）\n\
- 建议将文件保存到 `C:\\Users\\Public\\` 目录，路径中**不要包含中文或空格**\n\
- 正确示例（注意 SEND_FILE: 单独占一行）：\n\
  ```\n\
  已为您创建回归分析表格，请查收！\n\
  SEND_FILE:C:\\Users\\Public\\regression.xlsx\n\
  ```\n\
- 错误示例（不要把 SEND_FILE: 和其他文字混在同一行）：\n\
  ```\n\
  已完成！SEND_FILE:C:\\Users\\Public\\regression.xlsx 请查收\n\
  ```"
    } else {
        "### 发送 Office 文件给用户\n\
当前 IM 频道不支持直接发送文件。\
如果你用 `office` 工具创建了文件，请告知用户文件已保存的本地路径，\
让用户自行打开。建议将文件保存到桌面或 `C:\\Users\\Public\\` 等易找到的位置。"
    };

    format!(
        "{base}\n\n## IM 会话上下文\n\
你正在通过 **{channel}** IM 频道与用户对话，你的回复将直接发送到该平台。\n\n\
{platform_caps}\n\n\
### 接收图片\n{vision_hint}\n\n\
{file_send_hint}\n\n\
### 图表说明\n\
**不要在 IM 回复中使用 Mermaid 图表**（IM 平台无法渲染 mermaid 代码块）。\
如需展示流程或结构，请用文字、ASCII 图或简单列表代替。",
        base = base,
        channel = channel,
        platform_caps = platform_caps,
        vision_hint = vision_hint,
        file_send_hint = file_send_hint,
    )
}

/// Estimate token count for a string. Delegates to `llm::estimate_tokens`.
pub fn estimate_tokens(text: &str) -> usize {
    pisci_kernel::llm::estimate_tokens(text)
}

// ---------------------------------------------------------------------------
// Context management helpers
// ---------------------------------------------------------------------------

/// Compute the token budget for `build_context_messages` from settings.
/// Delegates to `llm::compute_context_budget`.
pub fn compute_context_budget(context_window: u32, max_tokens: u32) -> usize {
    pisci_kernel::llm::compute_context_budget(context_window, max_tokens)
}

pub use pisci_kernel::agent::compaction::{
    CTX_COMPACT_AFTER, CTX_FULL_TURNS, CTX_TRIM_HEAD, CTX_TRIM_TAIL,
};

/// Persist the completed agent turn to the database with full tool call structure.
///
/// Writes one row per logical message:
/// - intermediate assistant messages (with tool_calls_json)
/// - intermediate tool-result user messages (with tool_results_json)
/// - final assistant text message (plain content, no tool data)
///
/// All rows for this turn share the same `turn_index` derived from the current
/// message count in the session.
/// No-op: messages are now persisted in real-time by `AgentLoop::persist_message()`
/// during the run, so there is nothing left to write here.
/// The `final_messages` parameter is kept for logging only.
pub fn persist_agent_turn(
    _db: &crate::store::Database,
    session_id: &str,
    final_messages: &[LlmMessage],
) {
    tracing::info!(
        "persist_agent_turn: session={} new_messages={} (already written in real-time)",
        session_id,
        final_messages.len()
    );
}

/// A single conversation turn: one user message + all subsequent agent messages
/// up to (but not including) the next user message.
struct ConvTurn {
    /// The user message that started this turn.
    user_msg: ChatMessage,
    /// All agent messages in this turn (assistant text, tool calls, tool results).
    agent_msgs: Vec<ChatMessage>,
    /// 1-based turn index.
    index: usize,
}

fn split_history_into_turns(history: &[ChatMessage]) -> Vec<ConvTurn> {
    let mut turns: Vec<ConvTurn> = Vec::new();
    for msg in history {
        let starts_new_turn = msg.role == "user" && msg.tool_results_json.is_none();
        if starts_new_turn {
            turns.push(ConvTurn {
                user_msg: msg.clone(),
                agent_msgs: Vec::new(),
                index: turns.len() + 1,
            });
        } else if let Some(last) = turns.last_mut() {
            last.agent_msgs.push(msg.clone());
        }
    }
    turns
}

/// Trim a tool result string to `head` + `[trimmed: N chars]` + `tail`.
fn trim_tool_result(content: &str, head: usize, tail: usize) -> String {
    let total = content.len();
    if total <= head + tail + 40 {
        return content.to_string();
    }
    let head_str: String = content.chars().take(head).collect();
    let tail_str: String = content
        .chars()
        .rev()
        .take(tail)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let trimmed_chars = total.saturating_sub(head + tail);
    format!(
        "{}\n...[trimmed: {} chars]...\n{}",
        head_str, trimmed_chars, tail_str
    )
}

/// Build a two-message summary of a conversation turn for use in compressed context.
/// Returns (user_summary, assistant_summary) preserving the correct role structure.
///
/// Inspired by OpenClaw's compaction MERGE_SUMMARIES_INSTRUCTIONS which prioritizes:
/// - Active tasks and their current status
/// - Decisions made and their rationale
/// - Key artifacts (file paths, URLs, identifiers)
/// - What was being done and what the outcome was
fn summarize_turn(turn: &ConvTurn) -> (String, String) {
    let mut tool_entries: Vec<String> = Vec::new();
    let mut error_count = 0usize;
    let mut success_count = 0usize;
    let mut key_artifacts: Vec<String> = Vec::new();

    for msg in &turn.agent_msgs {
        if let Some(ref calls_json) = msg.tool_calls_json {
            if let Ok(calls) = serde_json::from_str::<Vec<serde_json::Value>>(calls_json) {
                for call in &calls {
                    let name = call.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                    let input = call
                        .get("input")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let artifact = extract_key_artifact(name, &input);
                    if let Some(a) = artifact {
                        key_artifacts.push(a);
                    }
                    tool_entries.push(name.to_string());
                }
            }
        }
        if let Some(ref results_json) = msg.tool_results_json {
            if let Ok(results) = serde_json::from_str::<Vec<serde_json::Value>>(results_json) {
                for (i, result) in results.iter().enumerate() {
                    let content = result.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    let is_error = result
                        .get("is_error")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if is_error {
                        error_count += 1;
                    } else {
                        success_count += 1;
                    }
                    let snippet: String = content.chars().take(120).collect();
                    let snippet = snippet.replace('\n', " ");
                    let status = if is_error { "ERR" } else { "OK" };
                    if let Some(entry) = tool_entries.get_mut(i) {
                        *entry = format!("{}[{}]→\"{}\"", entry, status, snippet);
                    }
                }
            }
        }
    }

    let final_answer = turn
        .agent_msgs
        .iter()
        .rev()
        .find(|m| m.role == "assistant" && !m.content.is_empty() && m.tool_calls_json.is_none())
        .map(|m| m.content.as_str())
        .unwrap_or("");
    let answer_snippet: String = final_answer.chars().take(300).collect();

    let tools_part = if tool_entries.is_empty() {
        String::new()
    } else {
        let stats = format!("{}ok/{}err", success_count, error_count);
        format!(" [tools({}): {}]", stats, tool_entries.join(", "))
    };

    let artifacts_part = if key_artifacts.is_empty() {
        String::new()
    } else {
        let deduped: Vec<_> = key_artifacts
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        format!(
            " [artifacts: {}]",
            deduped
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let user_summary = format!(
        "[历史第{}轮] {}",
        turn.index,
        turn.user_msg.content.chars().take(150).collect::<String>(),
    );
    let assistant_summary = format!(
        "[历史第{}轮回复]{}{} {}",
        turn.index, tools_part, artifacts_part, answer_snippet,
    );

    (user_summary, assistant_summary)
}

/// Extract key identifiers (file paths, URLs, queries) from tool input for summary.
fn extract_key_artifact(tool_name: &str, input: &serde_json::Value) -> Option<String> {
    match tool_name {
        "file_read" | "file_write" | "file_edit" => input["path"].as_str().map(|p| {
            let short: String = p
                .chars()
                .rev()
                .take(60)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            if short.len() < p.len() {
                format!("...{}", short)
            } else {
                short
            }
        }),
        "shell" | "powershell_query" => input["command"]
            .as_str()
            .or_else(|| input["query"].as_str())
            .map(|c| {
                let s: String = c.chars().take(50).collect();
                format!("cmd:{}", s)
            }),
        "web_search" => input["query"]
            .as_str()
            .map(|q| format!("search:{}", q.chars().take(40).collect::<String>())),
        "browser" => input["url"].as_str().map(|u| u.chars().take(60).collect()),
        _ => None,
    }
}

pub(crate) fn rolling_summary_message(summary: &str) -> LlmMessage {
    LlmMessage {
        role: "user".into(),
        content: MessageContent::text(format!(
            "[会话滚动摘要]\n{}\n\n[系统提示] 上述摘要覆盖了更早的对话历史，请结合后续真实消息继续任务，不要重复已完成的工作。",
            summary.trim()
        )),
    }
}

/// Build LLM context messages from stored history using layered compression.
///
/// Strategy (from newest to oldest):
/// - Last `CTX_FULL_TURNS` turns: full ContentBlock reconstruction (tool calls + results)
/// - Middle turns (up to `CTX_COMPACT_AFTER`): tool results trimmed to head+tail
/// - Older turns: entire turn collapsed to a single summary message
/// - Token budget exceeded: stop adding older turns
pub fn build_context_messages(
    history: &[ChatMessage],
    budget: usize,
    rolling_summary: Option<&str>,
) -> Vec<LlmMessage> {
    let rolling_summary = rolling_summary
        .map(str::trim)
        .filter(|summary| !summary.is_empty());
    if history.is_empty() {
        return rolling_summary
            .map(rolling_summary_message)
            .into_iter()
            .collect();
    }

    // Split history into turns (each turn starts at a user message that has content,
    // i.e. not a tool-result carrier message).
    let mut turns: Vec<ConvTurn> = Vec::new();
    let mut current_user: Option<ChatMessage> = None;
    let mut current_agents: Vec<ChatMessage> = Vec::new();
    let mut turn_idx = 0usize;

    for msg in history {
        let is_real_user = msg.role == "user" && msg.tool_results_json.is_none();
        if is_real_user {
            if let Some(user) = current_user.take() {
                turns.push(ConvTurn {
                    user_msg: user,
                    agent_msgs: std::mem::take(&mut current_agents),
                    index: turn_idx,
                });
            }
            turn_idx += 1;
            current_user = Some(msg.clone());
        } else {
            current_agents.push(msg.clone());
        }
    }
    // Push the last (current) turn
    if let Some(user) = current_user {
        turns.push(ConvTurn {
            user_msg: user,
            agent_msgs: current_agents,
            index: turn_idx,
        });
    }

    let total_turns = turns.len();
    let turn_slice = if rolling_summary.is_some() && total_turns > CTX_COMPACT_AFTER {
        &turns[total_turns - CTX_COMPACT_AFTER..]
    } else {
        &turns[..]
    };
    // Collect each turn's messages as a separate group so we can prepend older turns
    // without reversing the internal message order within each turn.
    let mut turn_groups: Vec<Vec<LlmMessage>> = Vec::new();
    let mut token_est: usize = rolling_summary
        .map(rolling_summary_message)
        .map(|message| pisci_kernel::llm::estimate_message_tokens(&message))
        .unwrap_or(0);

    // Process turns from newest to oldest; we prepend each group later.
    for (rev_idx, turn) in turn_slice.iter().rev().enumerate() {
        let turn_age = rev_idx; // 0 = most recent turn

        if token_est >= budget {
            break;
        }

        if turn_age < CTX_FULL_TURNS {
            // ── Full fidelity: reconstruct ContentBlocks ──────────────────
            let mut turn_tokens = estimate_tokens(&turn.user_msg.content);
            let mut turn_msgs: Vec<LlmMessage> = vec![LlmMessage {
                role: "user".into(),
                content: MessageContent::text(&turn.user_msg.content),
            }];
            for msg in &turn.agent_msgs {
                let blocks = reconstruct_blocks(msg);
                let text_for_tokens = blocks_to_token_text(&blocks);
                turn_tokens += estimate_tokens(&text_for_tokens);
                turn_msgs.push(LlmMessage {
                    role: msg.role.clone(),
                    content: if blocks.is_empty() {
                        MessageContent::text(&msg.content)
                    } else {
                        MessageContent::Blocks(blocks)
                    },
                });
            }
            if token_est + turn_tokens > budget && !turn_groups.is_empty() {
                break;
            }
            turn_groups.push(turn_msgs);
            token_est += turn_tokens;
        } else if turn_age < CTX_COMPACT_AFTER {
            // ── Trimmed: tool results head+tail, rest full ─────────────────
            let mut turn_tokens = estimate_tokens(&turn.user_msg.content);
            let mut turn_msgs: Vec<LlmMessage> = vec![LlmMessage {
                role: "user".into(),
                content: MessageContent::text(&turn.user_msg.content),
            }];
            for msg in &turn.agent_msgs {
                if let Some(ref results_json) = msg.tool_results_json {
                    let trimmed_blocks = minimal_tool_result_blocks(results_json);
                    let text_for_tokens = trimmed_blocks
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::ToolResult { content, .. } = b {
                                Some(content.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    turn_tokens += estimate_tokens(&text_for_tokens);
                    turn_msgs.push(LlmMessage {
                        role: "user".into(),
                        content: MessageContent::Blocks(trimmed_blocks),
                    });
                } else {
                    let blocks = reconstruct_blocks(msg);
                    let text_for_tokens = blocks_to_token_text(&blocks);
                    turn_tokens += estimate_tokens(&text_for_tokens);
                    turn_msgs.push(LlmMessage {
                        role: msg.role.clone(),
                        content: if blocks.is_empty() {
                            MessageContent::text(&msg.content)
                        } else {
                            MessageContent::Blocks(blocks)
                        },
                    });
                }
            }
            if token_est + turn_tokens > budget && !turn_groups.is_empty() {
                break;
            }
            turn_groups.push(turn_msgs);
            token_est += turn_tokens;
        } else {
            // ── Compact: entire turn → user + assistant summary pair ───────
            let (user_summary, assistant_summary) = summarize_turn(turn);
            let t = estimate_tokens(&user_summary) + estimate_tokens(&assistant_summary);
            if token_est + t > budget && !turn_groups.is_empty() {
                break;
            }
            turn_groups.push(vec![
                LlmMessage {
                    role: "user".into(),
                    content: MessageContent::text(&user_summary),
                },
                LlmMessage {
                    role: "assistant".into(),
                    content: MessageContent::text(&assistant_summary),
                },
            ]);
            token_est += t;
        }
    }

    // turn_groups was built newest-first; reverse the *groups* (not the messages
    // inside each group) to restore chronological turn order.
    turn_groups.reverse();
    let mut result: Vec<LlmMessage> = turn_groups.into_iter().flatten().collect();
    if let Some(summary) = rolling_summary {
        result.insert(0, rolling_summary_message(summary));
    }

    // Post-process: remove trailing orphaned tool_call messages (interrupted mid-turn).
    result = sanitize_tool_call_pairs(result);

    // Strip orphaned ToolUse blocks inside assistant messages that lack a matching
    // tool_result in the next message. Previously only applied in the headless path.
    result = sanitize_tool_use_result_pairing(result);

    // If a later retry of the same tool call succeeded, remove the earlier failed
    // ToolUse/ToolResult pair from context so the agent sees the corrected state.
    result = collapse_superseded_tool_failures(result);

    tracing::debug!(
        "build_context_messages: {} turns → {} LlmMessages, ~{} tokens (budget={})",
        total_turns,
        result.len(),
        token_est,
        budget
    );

    result
}

/// Remove only the TRAILING orphaned tool_call messages at the end of the context.
/// An orphaned tool_call is an assistant message with ToolUse blocks that is NOT
/// immediately followed by a matching tool-result message.
///
/// This handles the case where the last agent turn was interrupted mid-tool-call,
/// leaving dangling tool_call entries at the end of the history.
/// We do NOT touch tool_call/result pairs in the middle of history — those are valid.
pub(crate) fn sanitize_tool_call_pairs(messages: Vec<LlmMessage>) -> Vec<LlmMessage> {
    let n = messages.len();
    if n == 0 {
        return messages;
    }

    // Walk from the end, collecting indices to drop.
    // We only remove trailing orphans: once we see a properly-paired tool_call/result
    // or any non-tool message, we stop.
    let mut drop_from = n; // index from which to truncate (exclusive end kept)

    let mut i = n;
    while i > 0 {
        i -= 1;
        let m = &messages[i];

        // Collect ToolUse ids in this message
        let tool_use_ids: Vec<String> = if let MessageContent::Blocks(blocks) = &m.content {
            blocks
                .iter()
                .filter_map(|b| {
                    if let ContentBlock::ToolUse { id, .. } = b {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            vec![]
        };

        if tool_use_ids.is_empty() {
            // Not a tool_call message. Check if it's a tool_result (orphaned result after we
            // already removed its tool_call). If so, keep removing. Otherwise stop.
            let is_tool_result = if let MessageContent::Blocks(blocks) = &m.content {
                blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
            } else {
                false
            };
            if is_tool_result && drop_from == i + 1 {
                // This result is dangling (its tool_call was already marked for removal)
                drop_from = i;
                continue;
            }
            // Regular message — stop scanning
            break;
        }

        // This is a tool_call message. Check if the IMMEDIATELY following messages
        // contain tool_results that satisfy ALL of its tool_use_ids.
        let mut satisfied: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut j = i + 1;
        while j < n {
            if let MessageContent::Blocks(blocks) = &messages[j].content {
                let has_result = blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
                if has_result {
                    for b in blocks {
                        if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                            satisfied.insert(tool_use_id.clone());
                        }
                    }
                    j += 1;
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        let all_satisfied = tool_use_ids.iter().all(|id| satisfied.contains(id));
        if !all_satisfied {
            tracing::warn!(
                "sanitize_tool_call_pairs: trailing orphaned tool_call at index {} (ids={:?}, satisfied={:?}), removing",
                i, tool_use_ids, satisfied
            );
            drop_from = i;
            // Continue scanning backwards in case there are more trailing orphans
        } else {
            // This tool_call is properly paired — stop here
            break;
        }
    }

    if drop_from < n {
        tracing::warn!(
            "sanitize_tool_call_pairs: truncating {} trailing orphaned messages (kept {}/{})",
            n - drop_from,
            drop_from,
            n
        );
        messages.into_iter().take(drop_from).collect()
    } else {
        messages
    }
}

/// Reconstruct ContentBlocks from a stored ChatMessage.
fn reconstruct_blocks(msg: &ChatMessage) -> Vec<ContentBlock> {
    let mut blocks: Vec<ContentBlock> = Vec::new();

    // Text content
    if !msg.content.is_empty() {
        blocks.push(ContentBlock::Text {
            text: msg.content.clone(),
        });
    }

    // Tool calls (for assistant messages)
    if let Some(ref json) = msg.tool_calls_json {
        if let Ok(calls) = serde_json::from_str::<Vec<ContentBlock>>(json) {
            blocks.extend(calls);
        }
    }

    // Tool results (for user/tool messages).
    // When tool_results_json is present this message IS a tool-result carrier.
    // Return ONLY the ToolResult blocks — any text in msg.content is a DB
    // artefact and must NOT be mixed in, because inserting a Text/user block
    // inside a tool-result sequence breaks the OpenAI API contract
    // ("tool messages must immediately follow the assistant tool_calls message").
    if let Some(ref json) = msg.tool_results_json {
        if let Ok(results) = serde_json::from_str::<Vec<ContentBlock>>(json) {
            return results;
        }
    }

    blocks
}

/// Build tool result blocks using the dual-version *minimal* representation
/// for middle-tier turns.
///
/// Priority order per entry:
/// 1. `content_minimal` field from the persisted JSON (new rows written after
///    the dual-version migration).
/// 2. Rule-based backfill via `tool_receipt::render_receipt` using `tool_name`
///    when available, or `"unknown"` otherwise (legacy rows).
/// 3. Legacy head+tail char trim as the very last fallback (should only happen
///    when the JSON is malformed enough that we cannot extract content).
///
/// Non-ToolResult blocks (e.g. images) are passed through unchanged.
fn minimal_tool_result_blocks(results_json: &str) -> Vec<ContentBlock> {
    let raw: serde_json::Value = match serde_json::from_str(results_json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let arr = match raw.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };

    let mut out: Vec<ContentBlock> = Vec::with_capacity(arr.len());
    for item in arr {
        // Detect the block type via the embedded "type" tag.
        let kind = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if kind == "tool_result" {
            let tool_use_id = item
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let full = item
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let is_error = item
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let minimal = item
                .get("content_minimal")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let tool_name = item.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");

            let demoted = match minimal {
                Some(m) if !m.is_empty() => m,
                _ => {
                    // Backfill a receipt from the full content. If tool_name is
                    // unknown (legacy rows), use the fallback branch of the
                    // renderer (generic "called X" template).
                    let name = if tool_name.is_empty() {
                        "unknown"
                    } else {
                        tool_name
                    };
                    pisci_kernel::agent::tool_receipt::render_receipt(
                        name,
                        &serde_json::Value::Null,
                        &full,
                        is_error,
                        None,
                    )
                }
            };
            let content = if demoted.is_empty() {
                // Defensive last resort: if receipt generation somehow yielded
                // an empty string, fall back to the legacy head/tail char trim
                // so the LLM still sees *something*.
                trim_tool_result(&full, CTX_TRIM_HEAD, CTX_TRIM_TAIL)
            } else {
                demoted
            };
            // p11: append `[recall:<tool_use_id>]` so the agent can re-fetch
            // the original full content via the recall_tool_result tool.
            let content =
                pisci_kernel::agent::tool_receipt::with_recall_hint(&content, &tool_use_id);
            out.push(ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            });
        } else {
            // Non-tool_result blocks: deserialize normally and keep them.
            if let Ok(b) = serde_json::from_value::<ContentBlock>(item.clone()) {
                out.push(b);
            }
        }
    }
    out
}

fn extract_tool_minimals_from_history(history: &[ChatMessage]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for msg in history {
        if let Some(results_json) = msg.tool_results_json.as_deref() {
            extract_tool_minimals_from_results_json(results_json, &mut out);
        }
    }
    out
}

fn extract_tool_minimals_from_results_json(results_json: &str, out: &mut HashMap<String, String>) {
    let raw: serde_json::Value = match serde_json::from_str(results_json) {
        Ok(v) => v,
        Err(_) => return,
    };
    let Some(arr) = raw.as_array() else {
        return;
    };
    for item in arr {
        let kind = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if kind != "tool_result" {
            continue;
        }
        let Some(tool_use_id) = item.get("tool_use_id").and_then(|v| v.as_str()) else {
            continue;
        };
        if tool_use_id.is_empty() {
            continue;
        }
        let full = item
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let is_error = item
            .get("is_error")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let tool_name = item.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
        let demoted = item
            .get("content_minimal")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                pisci_kernel::agent::tool_receipt::render_receipt(
                    if tool_name.is_empty() {
                        "unknown"
                    } else {
                        tool_name
                    },
                    &serde_json::Value::Null,
                    &full,
                    is_error,
                    None,
                )
            });
        let content = if demoted.is_empty() {
            trim_tool_result(&full, CTX_TRIM_HEAD, CTX_TRIM_TAIL)
        } else {
            demoted
        };
        out.insert(tool_use_id.to_string(), content);
    }
}

/// Extract a representative text string from blocks for token estimation.
fn blocks_to_token_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.as_str(),
            ContentBlock::ToolResult { content, .. } => content.as_str(),
            ContentBlock::ToolUse { name, .. } => name.as_str(),
            ContentBlock::Image { .. } => "[image]",
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn tool_call_signature(name: &str, input: &serde_json::Value) -> String {
    let mut normalized = input.clone();
    if let Some(obj) = normalized.as_object_mut() {
        obj.remove("_trace_id");
    }
    let input_json = serde_json::to_string(&normalized).unwrap_or_default();
    format!("{}::{}", name, input_json)
}

const SUPERSEDE_RETRY_WINDOW_MSGS: usize = 6;

fn is_tool_result_carrier(msg: &LlmMessage) -> bool {
    matches!(
        &msg.content,
        MessageContent::Blocks(blocks)
            if blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
    )
}

fn has_real_user_turn_between(msgs: &[LlmMessage], start_idx: usize, end_idx: usize) -> bool {
    msgs.iter()
        .enumerate()
        .skip(start_idx.saturating_add(1))
        .take(end_idx.saturating_sub(start_idx.saturating_add(1)))
        .any(|(_, msg)| msg.role == "user" && !is_tool_result_carrier(msg))
}

fn is_retryable_tool_failure(content: &str) -> bool {
    let lower = content.to_lowercase();
    content.contains("[schema_correction tool=")
        || lower.contains("schema 不匹配")
        || lower.contains("missing field")
        || lower.contains("missing required")
        || lower.contains("invalid type")
        || lower.contains("invalid value")
        || lower.contains("unknown field")
        || lower.contains("unknown variant")
        || lower.contains("did not match any variant")
        || lower.contains("additional properties are not allowed")
        || lower.contains("tool '") && lower.contains("does not exist. available tools:")
        || lower.contains("工具 '") && (lower.contains("未找到") || lower.contains("当前可用工具"))
}

pub(crate) fn collapse_superseded_tool_failures(mut msgs: Vec<LlmMessage>) -> Vec<LlmMessage> {
    let mut tool_use_meta: HashMap<String, (usize, String, String)> = HashMap::new();
    let mut last_success_pos: HashMap<String, usize> = HashMap::new();
    let mut success_by_tool_name: HashMap<String, Vec<usize>> = HashMap::new();

    for (msg_idx, msg) in msgs.iter().enumerate() {
        let MessageContent::Blocks(blocks) = &msg.content else {
            continue;
        };

        for block in blocks {
            if let ContentBlock::ToolUse { id, name, input } = block {
                tool_use_meta.insert(
                    id.clone(),
                    (msg_idx, tool_call_signature(name, input), name.clone()),
                );
            }
        }

        for block in blocks {
            if let ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                ..
            } = block
            {
                if *is_error {
                    continue;
                }
                if let Some((tool_msg_idx, signature, tool_name)) = tool_use_meta.get(tool_use_id) {
                    last_success_pos
                        .entry(signature.clone())
                        .and_modify(|pos| *pos = (*pos).max(*tool_msg_idx))
                        .or_insert(*tool_msg_idx);
                    success_by_tool_name
                        .entry(tool_name.clone())
                        .or_default()
                        .push(*tool_msg_idx);
                }
            }
        }
    }

    if last_success_pos.is_empty() {
        return msgs;
    }

    let mut superseded_tool_use_ids: HashSet<String> = HashSet::new();
    for msg in &msgs {
        let MessageContent::Blocks(blocks) = &msg.content else {
            continue;
        };
        for block in blocks {
            if let ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                content,
                ..
            } = block
            {
                if !*is_error {
                    continue;
                }
                if let Some((tool_msg_idx, signature, tool_name)) = tool_use_meta.get(tool_use_id) {
                    if last_success_pos.get(signature).is_some_and(|success_pos| {
                        tool_msg_idx < success_pos
                            && !has_real_user_turn_between(&msgs, *tool_msg_idx, *success_pos)
                    }) {
                        superseded_tool_use_ids.insert(tool_use_id.clone());
                        continue;
                    }

                    if !is_retryable_tool_failure(content) {
                        continue;
                    }

                    if let Some(success_positions) = success_by_tool_name.get(tool_name) {
                        let matched_retry = success_positions.iter().any(|success_pos| {
                            *success_pos > *tool_msg_idx
                                && success_pos.saturating_sub(*tool_msg_idx)
                                    <= SUPERSEDE_RETRY_WINDOW_MSGS
                                && !has_real_user_turn_between(&msgs, *tool_msg_idx, *success_pos)
                        });
                        if matched_retry {
                            superseded_tool_use_ids.insert(tool_use_id.clone());
                        }
                    }
                }
            }
        }
    }

    if superseded_tool_use_ids.is_empty() {
        return msgs;
    }

    for msg in msgs.iter_mut() {
        let MessageContent::Blocks(blocks) = &mut msg.content else {
            continue;
        };
        blocks.retain(|block| match block {
            ContentBlock::ToolUse { id, .. } => !superseded_tool_use_ids.contains(id),
            ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => !(*is_error && superseded_tool_use_ids.contains(tool_use_id)),
            _ => true,
        });
    }

    msgs.retain(|msg| match &msg.content {
        MessageContent::Blocks(blocks) => !blocks.is_empty(),
        MessageContent::Text(text) => !text.trim().is_empty(),
    });

    tracing::info!(
        "collapse_superseded_tool_failures: removed {} superseded failed tool attempt(s)",
        superseded_tool_use_ids.len()
    );
    msgs
}

/// Remove orphaned tool_use blocks from LLM messages.
///
/// An orphaned tool_use occurs when a previous agent run was cancelled mid-turn:
/// the assistant message has ToolUse blocks but there is no following user message
/// with matching ToolResult blocks. Sending orphaned tool_use to the API causes errors
/// and makes the LLM think it needs to continue the old task.
///
/// Strategy (mirrors openclaw's sanitizeToolUseResultPairing):
/// Walk the message list; if an assistant message ends with ToolUse blocks but the
/// next message is not a tool-result carrier (or there is no next message), strip
/// the ToolUse blocks from that assistant message. If stripping leaves the message
/// empty, remove it entirely.
pub(crate) fn sanitize_tool_use_result_pairing(mut msgs: Vec<LlmMessage>) -> Vec<LlmMessage> {
    let mut i = 0;
    while i < msgs.len() {
        let has_tool_use = if msgs[i].role == "assistant" {
            match &msgs[i].content {
                pisci_kernel::llm::MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolUse { .. })),
                _ => false,
            }
        } else {
            i += 1;
            continue;
        };

        if !has_tool_use {
            i += 1;
            continue;
        }

        // Check if the next message is a tool-result carrier
        let next_is_tool_result = msgs
            .get(i + 1)
            .map(|next| {
                next.role == "user"
                    && match &next.content {
                        pisci_kernel::llm::MessageContent::Blocks(blocks) => blocks
                            .iter()
                            .any(|b| matches!(b, ContentBlock::ToolResult { .. })),
                        _ => false,
                    }
            })
            .unwrap_or(false);

        if !next_is_tool_result {
            // Strip ToolUse blocks from this assistant message
            tracing::warn!(
                "sanitize_tool_use_result_pairing: stripping orphaned ToolUse at index {}",
                i
            );
            if let pisci_kernel::llm::MessageContent::Blocks(ref mut blocks) = msgs[i].content {
                blocks.retain(|b| !matches!(b, ContentBlock::ToolUse { .. }));
            }
            // If the message is now empty, remove it
            let is_empty = match &msgs[i].content {
                pisci_kernel::llm::MessageContent::Blocks(blocks) => blocks.is_empty(),
                pisci_kernel::llm::MessageContent::Text(t) => t.trim().is_empty(),
            };
            if is_empty {
                msgs.remove(i);
                continue; // don't increment, re-check same index
            }
        }
        i += 1;
    }
    msgs
}

/// After an agent run, use LLM to extract 1-3 key memories from the conversation.
/// Only triggers when the conversation has substantive content.
/// Takes Arc<Mutex<Database>> so it can be called from tokio::spawn safely.
pub async fn auto_extract_memories(
    db_arc: Arc<tokio::sync::Mutex<crate::store::Database>>,
    session_id: String,
    messages: Vec<pisci_kernel::llm::LlmMessage>,
    client: Box<dyn pisci_kernel::llm::LlmClient>,
    model: String,
    max_tokens: u32,
    owner_id: String,
) {
    // Only extract if there's meaningful assistant content
    let assistant_chars: usize = messages
        .iter()
        .filter(|m| m.role == "assistant")
        .map(|m| m.content.as_text().chars().count())
        .sum();

    if assistant_chars < 100 {
        return;
    }

    // Build a compact conversation summary for the extraction prompt.
    // Take the LAST messages (most recent) rather than the first, since recent
    // context is far more likely to contain extractable memories.
    let relevant_msgs: Vec<_> = messages
        .iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .collect();
    let start = relevant_msgs.len().saturating_sub(12);
    let conv_summary: String = relevant_msgs[start..]
        .iter()
        .map(|m| {
            let text = m.content.as_text();
            let truncated: String = text.chars().take(400).collect();
            format!("{}: {}", m.role, truncated)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let extraction_prompt = format!(
        "Based on this conversation, extract 0-3 important facts worth remembering about the user \
         (preferences, goals, personal info, project details). \
         If nothing significant was revealed, output exactly: NONE\n\
         Otherwise output one memory per line, prefixed with the category in brackets like:\n\
         [preference] User prefers dark mode\n\
         [project] Working on a Rust desktop app called OpenPisci\n\n\
         Conversation:\n{}\n\nMemories (or NONE):",
        conv_summary
    );

    let req = pisci_kernel::llm::LlmRequest {
        messages: vec![pisci_kernel::llm::LlmMessage {
            role: "user".into(),
            content: pisci_kernel::llm::MessageContent::text(&extraction_prompt),
        }],
        system: Some("You are a memory extraction assistant. Be concise and only extract genuinely useful personal information.".into()),
        tools: vec![],
        model: model.clone(),
        max_tokens: max_tokens.min(512),
        stream: false,
        vision_override: None,
    };

    match client.complete(req).await {
        Ok(resp) if !resp.content.is_empty() && resp.content.trim() != "NONE" => {
            let db = db_arc.lock().await;
            for line in resp.content.lines() {
                let line = line.trim();
                if line.is_empty() || line == "NONE" {
                    continue;
                }

                let (category, content) = if line.starts_with('[') {
                    if let Some(end) = line.find(']') {
                        let cat = &line[1..end];
                        let cont = line[end + 1..].trim();
                        (cat, cont)
                    } else {
                        ("general", line)
                    }
                } else {
                    ("general", line)
                };

                let valid_categories =
                    ["preference", "fact", "task", "person", "project", "general"];
                let category = if valid_categories.contains(&category) {
                    category
                } else {
                    "general"
                };

                if !content.is_empty() {
                    let _ = db.save_memory(
                        content,
                        category,
                        0.75,
                        Some(&session_id),
                        &owner_id,
                        "private",
                        &owner_id,
                        None,
                    );
                    tracing::info!("Auto-extracted memory [{category}] for {owner_id}: {content}");
                }
            }
        }
        Ok(_) => {} // NONE or empty — nothing to save
        Err(e) => tracing::warn!("Memory auto-extraction failed: {}", e),
    }
}

// ─── Context Preview (Debug) ──────────────────────────────────────────────────

/// A single content block within a preview message.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContextPreviewBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        /// JSON-serialised input (full, not truncated)
        input: String,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
        /// true when content was truncated to fit display
        truncated: bool,
    },
    Image {
        note: String,
    },
}

/// Serialisable representation of one LLM message for the debug preview.
#[derive(Debug, Serialize)]
pub struct ContextPreviewMessage {
    pub role: String,
    pub blocks: Vec<ContextPreviewBlock>,
    /// Estimated token count for this message.
    pub tokens: usize,
}

#[derive(Debug, Serialize)]
pub struct ContextPreview {
    pub messages: Vec<ContextPreviewMessage>,
    pub messages_tokens: usize,
    pub total_tokens: usize,
    pub request_view_tokens: usize,
    pub idle_indicator_tokens: usize,
    pub model: String,
    pub context_budget: usize,
    pub total_input_budget: usize,
    pub request_overhead_tokens: usize,
    pub tool_count: usize,
    pub rolling_summary_version: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub last_compacted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Build and return the exact context (system prompt + messages + tool list)
/// that would be sent to the LLM on the next turn for the given session.
/// No LLM call is made — this is read-only and safe to call at any time.
#[tauri::command]
pub async fn get_context_preview(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<ContextPreview, String> {
    // Load settings
    let (
        model,
        max_tokens,
        context_window,
        workspace_root,
        allow_outside_workspace,
        builtin_tool_enabled,
        project_instruction_budget_chars,
        enable_project_instructions,
    ) = {
        let settings = state.settings.lock().await;
        (
            settings.model.clone(),
            settings.max_tokens,
            settings.context_window,
            settings.workspace_root.clone(),
            settings.allow_outside_workspace,
            settings.builtin_tool_enabled.clone(),
            settings.project_instruction_budget_chars,
            settings.enable_project_instructions,
        )
    };

    // Build context messages from history — this is the exact payload sent to the LLM
    let budget = compute_context_budget(context_window, max_tokens);
    let session_context = build_session_message_context(&state, &session_id, budget).await?;
    let prompt_artifacts = build_chat_prompt_artifacts(
        &state.app_handle,
        &state,
        &session_id,
        &session_context.latest_user_text,
        &workspace_root,
        context_window,
        max_tokens,
        allow_outside_workspace,
        &builtin_tool_enabled,
        project_instruction_budget_chars,
        enable_project_instructions,
    )
    .await?;
    let base_llm_messages = session_context.llm_messages;
    let tool_minimals = session_context.tool_minimals;
    let session_state = session_context.session_state;
    let llm_messages =
        pisci_kernel::agent::vision::inject_selected_context(&base_llm_messages, &session_id).await;

    // Convert LlmMessages to preview-friendly structs with structured blocks
    let messages: Vec<ContextPreviewMessage> = llm_messages
        .iter()
        .map(|m| {
            let blocks: Vec<ContextPreviewBlock> = match &m.content {
                pisci_kernel::llm::MessageContent::Text(t) => {
                    if t.is_empty() {
                        vec![]
                    } else {
                        vec![ContextPreviewBlock::Text { text: t.clone() }]
                    }
                }
                pisci_kernel::llm::MessageContent::Blocks(raw_blocks) => raw_blocks
                    .iter()
                    .map(|b| match b {
                        pisci_kernel::llm::ContentBlock::Text { text } => {
                            ContextPreviewBlock::Text { text: text.clone() }
                        }
                        pisci_kernel::llm::ContentBlock::ToolUse { id, name, input } => {
                            ContextPreviewBlock::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: serde_json::to_string_pretty(input).unwrap_or_default(),
                            }
                        }
                        pisci_kernel::llm::ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            const PREVIEW_LIMIT: usize = 4000;
                            let truncated = content.len() > PREVIEW_LIMIT;
                            let display = if truncated {
                                let head: String =
                                    content.chars().take(PREVIEW_LIMIT * 3 / 4).collect();
                                let tail_start = content
                                    .char_indices()
                                    .rev()
                                    .nth(PREVIEW_LIMIT / 4)
                                    .map(|(i, _)| i)
                                    .unwrap_or(content.len());
                                format!("{}\n\n… [truncated] …\n\n{}", head, &content[tail_start..])
                            } else {
                                content.clone()
                            };
                            ContextPreviewBlock::ToolResult {
                                tool_use_id: tool_use_id.clone(),
                                content: display,
                                is_error: *is_error,
                                truncated,
                            }
                        }
                        pisci_kernel::llm::ContentBlock::Image { .. } => {
                            ContextPreviewBlock::Image {
                                note: "[image attachment]".to_string(),
                            }
                        }
                    })
                    .collect(),
            };
            let tokens = pisci_kernel::llm::estimate_message_tokens(m);
            ContextPreviewMessage {
                role: m.role.clone(),
                blocks,
                tokens,
            }
        })
        .collect();

    let messages_tokens: usize = messages.iter().map(|m| m.tokens).sum();
    let request_overhead_tokens = pisci_kernel::llm::estimate_request_overhead_tokens(
        Some(&prompt_artifacts.system_prompt),
        &prompt_artifacts.tool_defs,
    );
    let total_input_budget =
        pisci_kernel::llm::compute_total_input_budget(context_window, max_tokens);
    let message_budget = total_input_budget.saturating_sub(request_overhead_tokens);
    let request_view_messages = pisci_kernel::agent::loop_::build_request_view_messages(
        &base_llm_messages,
        &tool_minimals,
        pisci_kernel::agent::compaction::CTX_PRESERVE_RECENT_TURNS,
        pisci_kernel::agent::compaction::CTX_KEEP_RECENT_TOOL_CARRIERS,
        message_budget,
    );
    let request_view_messages =
        pisci_kernel::agent::vision::inject_selected_context(&request_view_messages, &session_id)
            .await;
    let total_tokens = pisci_kernel::llm::estimate_request_input_tokens(
        &llm_messages,
        Some(&prompt_artifacts.system_prompt),
        &prompt_artifacts.tool_defs,
    );
    let request_view_tokens = pisci_kernel::llm::estimate_request_input_tokens(
        &request_view_messages,
        Some(&prompt_artifacts.system_prompt),
        &prompt_artifacts.tool_defs,
    );
    let trigger_threshold = ((total_input_budget as f64) * 0.60).round() as usize;
    let mut idle_indicator_tokens = request_view_tokens;

    // Idle-session indicator should reflect the compacted request shape users
    // just experienced during the last run, not the raw DB replay. The live
    // agent keeps an in-memory compacted timeline plus rolling summary/state
    // frame, but only the summary/frame persist. On resume, rebuilding from
    // DB can therefore briefly re-inflate the estimate for a few giant turns.
    // If we already have a rolling summary and the replayed request is still
    // above the proactive compaction trigger, estimate the "post-refresh"
    // compacted view using the existing summary-only history slice.
    if session_state
        .as_ref()
        .is_some_and(|state| state.rolling_summary_version > 0)
        && request_view_tokens > trigger_threshold
    {
        let compacted_context = build_session_message_context_from_db(
            &state.db,
            &session_id,
            budget,
            HistorySliceMode::SummaryOnly,
            &crate::headless_cli::HeadlessContextToggles::default(),
        )
        .await?;
        let compacted_request_messages = pisci_kernel::agent::loop_::build_request_view_messages(
            &compacted_context.llm_messages,
            &compacted_context.tool_minimals,
            pisci_kernel::agent::compaction::CTX_PRESERVE_RECENT_TURNS,
            pisci_kernel::agent::compaction::CTX_KEEP_RECENT_TOOL_CARRIERS,
            message_budget,
        );
        let compacted_request_messages = pisci_kernel::agent::vision::inject_selected_context(
            &compacted_request_messages,
            &session_id,
        )
        .await;
        let compacted_tokens = pisci_kernel::llm::estimate_request_input_tokens(
            &compacted_request_messages,
            Some(&prompt_artifacts.system_prompt),
            &prompt_artifacts.tool_defs,
        );
        idle_indicator_tokens = compacted_tokens.min(request_view_tokens);
    }

    Ok(ContextPreview {
        messages,
        messages_tokens,
        total_tokens,
        request_view_tokens,
        idle_indicator_tokens,
        model,
        context_budget: budget,
        total_input_budget,
        request_overhead_tokens,
        tool_count: prompt_artifacts.tool_defs.len(),
        rolling_summary_version: session_state
            .as_ref()
            .map(|state| state.rolling_summary_version)
            .unwrap_or(0),
        total_input_tokens: session_state
            .as_ref()
            .map(|state| state.total_input_tokens)
            .unwrap_or(0),
        total_output_tokens: session_state
            .as_ref()
            .map(|state| state.total_output_tokens)
            .unwrap_or(0),
        last_compacted_at: session_state.and_then(|state| state.last_compacted_at),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        build_context_messages, build_main_chat_system_prompt, collapse_superseded_tool_failures,
        derive_headless_session_source, extract_tool_minimals_from_history,
        minimal_tool_result_blocks, resolve_headless_scene_kind, HeadlessRunOptions,
        SESSION_SOURCE_PISCI_HEARTBEAT_GLOBAL, SESSION_SOURCE_PISCI_POOL,
    };
    use crate::commands::config::scene::SceneKind;
    use crate::store::db::ChatMessage;
    use chrono::Utc;
    use pisci_kernel::llm::{ContentBlock, LlmMessage, MessageContent};
    use serde_json::json;

    fn make_chat_message(
        session_id: &str,
        role: &str,
        content: &str,
        turn_index: i64,
    ) -> ChatMessage {
        ChatMessage {
            id: format!("{}-{}-{}", session_id, role, turn_index),
            session_id: session_id.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            created_at: Utc::now(),
            tool_calls_json: None,
            tool_results_json: None,
            turn_index: Some(turn_index),
        }
    }

    fn assistant_tool_use(id: &str, name: &str, input: serde_json::Value) -> LlmMessage {
        LlmMessage {
            role: "assistant".to_string(),
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input,
            }]),
        }
    }

    fn user_tool_result(id: &str, content: &str, is_error: bool) -> LlmMessage {
        LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: content.to_string(),
                is_error,
            }]),
        }
    }

    fn text_msg(role: &str, text: &str) -> LlmMessage {
        LlmMessage {
            role: role.to_string(),
            content: MessageContent::Text(text.to_string()),
        }
    }

    fn has_tool_result_content(msgs: &[LlmMessage], needle: &str) -> bool {
        msgs.iter().any(|msg| match &msg.content {
            MessageContent::Blocks(blocks) => blocks.iter().any(|block| match block {
                ContentBlock::ToolResult { content, .. } => content.contains(needle),
                _ => false,
            }),
            MessageContent::Text(_) => false,
        })
    }

    #[test]
    fn build_context_messages_prepends_rolling_summary() {
        let history = vec![
            make_chat_message("s1", "user", "用户请求 A", 1),
            make_chat_message("s1", "assistant", "回复 A", 1),
            make_chat_message("s1", "user", "用户请求 B", 2),
            make_chat_message("s1", "assistant", "回复 B", 2),
        ];

        let messages = build_context_messages(&history, 20_000, Some("用户目标[修复问题]"));
        assert!(!messages.is_empty());
        assert!(messages[0].content.as_text().contains("[会话滚动摘要]"));
    }

    #[test]
    fn build_context_messages_limits_older_turns_when_rolling_summary_exists() {
        let mut history = Vec::new();
        for turn in 1..=12 {
            history.push(make_chat_message(
                "s2",
                "user",
                &format!("[turn:{}] 用户请求", turn),
                turn,
            ));
            history.push(make_chat_message(
                "s2",
                "assistant",
                &format!("[turn:{}] 回复", turn),
                turn,
            ));
        }

        let messages = build_context_messages(&history, 50_000, Some("已有滚动摘要"));
        assert!(messages[0].content.as_text().contains("[会话滚动摘要]"));
        assert!(
            !messages
                .iter()
                .any(|msg| msg.content.as_text().contains("[turn:1]")),
            "oldest turn should be omitted once rolling summary is present"
        );
        assert!(
            messages
                .iter()
                .any(|msg| msg.content.as_text().contains("[turn:12]")),
            "recent turns should still be preserved"
        );
    }

    #[test]
    fn heartbeat_and_pool_sessions_use_expected_sources() {
        assert_eq!(
            derive_headless_session_source("heartbeat", None),
            SESSION_SOURCE_PISCI_HEARTBEAT_GLOBAL
        );
        assert_eq!(
            derive_headless_session_source("heartbeat", Some("pool-1")),
            SESSION_SOURCE_PISCI_POOL
        );
        assert_eq!(
            derive_headless_session_source("feishu", Some("pool-1")),
            SESSION_SOURCE_PISCI_POOL
        );
    }

    #[test]
    fn resolve_headless_scene_kind_respects_channel_scope_and_explicit_override() {
        let explicit = HeadlessRunOptions {
            scene_kind: Some(SceneKind::KoiTask),
            ..HeadlessRunOptions::default()
        };
        assert_eq!(
            resolve_headless_scene_kind("internal", "im_internal", Some(&explicit)),
            SceneKind::KoiTask
        );

        let heartbeat = HeadlessRunOptions {
            pool_session_id: Some("pool-1".into()),
            ..HeadlessRunOptions::default()
        };
        assert_eq!(
            resolve_headless_scene_kind(
                "heartbeat",
                SESSION_SOURCE_PISCI_HEARTBEAT_GLOBAL,
                Some(&heartbeat)
            ),
            SceneKind::HeartbeatSupervisor
        );

        let pool = HeadlessRunOptions {
            pool_session_id: Some("pool-1".into()),
            ..HeadlessRunOptions::default()
        };
        assert_eq!(
            resolve_headless_scene_kind("internal", SESSION_SOURCE_PISCI_POOL, Some(&pool)),
            SceneKind::PoolCoordinator
        );

        assert_eq!(
            resolve_headless_scene_kind("feishu", "im_feishu", None),
            SceneKind::IMHeadless
        );
    }

    #[test]
    fn main_prompt_preserves_pisci_routing_heuristics() {
        let prompt = build_main_chat_system_prompt("", "", false);
        for required in [
            "Pool and Koi state is not preloaded from keyword matches",
            "Use `pool_org` when the work is complex, spans multiple domains, or has a high quality bar",
            "inspect existing pools and the current Koi roster first",
            "If the current Koi roster is missing a needed specialist role, add the minimum additional Koi required before delegating work",
            "Use `call_fish` for simple, self-contained, result-heavy work",
            "Do the work yourself when it is still simple enough for one agent but the user benefits from your own detailed reasoning",
            "Prefer Fish for simple result-first work such as web search, file search",
            "Sleep between checks with exponential backoff",
            "Do not infer timeout from loop/turn count",
        ] {
            assert!(
                prompt.contains(required),
                "main prompt lost routing heuristic literal: {}",
                required
            );
        }
    }

    #[test]
    fn main_prompt_requires_pool_for_quality_sensitive_multi_role_work() {
        let prompt = build_main_chat_system_prompt("", "", false);
        assert!(
            prompt.contains(
                "The task is complex, multi-domain, and quality-sensitive enough that explicit review, quality control, or specialist cross-checking should be separate from implementation"
            ),
            "main prompt must force pool routing for quality-sensitive multi-role work"
        );
        assert!(
            prompt.contains(
                "Before assigning work, check whether the existing Koi roster covers the specialist roles the project needs"
            ),
            "main prompt must require checking the Koi roster before delegating complex multi-role work"
        );
    }

    #[test]
    fn minimal_blocks_use_content_minimal_when_present() {
        // New-format row: tool_results_json carries both `content` (full) and
        // `content_minimal` (rule-based receipt). The middle-tier reader must
        // emit the minimal payload.
        let json = r#"[
            {
                "type": "tool_result",
                "tool_use_id": "call-1",
                "tool_name": "shell",
                "content": "total 1234\nexit code: 0\nverbose output line 1\nverbose output line 2",
                "content_minimal": "ran: ls; exit=0; out=60 chars",
                "is_error": false
            }
        ]"#;
        let blocks = minimal_tool_result_blocks(json);
        assert_eq!(blocks.len(), 1);
        if let ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } = &blocks[0]
        {
            assert_eq!(tool_use_id, "call-1");
            // p11: demoted receipts now carry a `[recall:<tool_use_id>]` suffix.
            assert_eq!(content, "ran: ls; exit=0; out=60 chars [recall:call-1]");
            assert!(!is_error);
        } else {
            panic!("expected a ToolResult block");
        }
    }

    #[test]
    fn minimal_blocks_backfill_receipt_for_legacy_rows() {
        // Legacy row: no `content_minimal`, no `tool_name`. Reader must fall
        // back to the rule-based receipt generator via the generic "unknown"
        // tool template so the LLM still sees a concise timeline entry
        // instead of the untrimmed full content.
        let long_body: String = "x".repeat(5_000);
        let json = serde_json::to_string(&serde_json::json!([{
            "type": "tool_result",
            "tool_use_id": "call-2",
            "content": long_body,
            "is_error": false
        }]))
        .unwrap();
        let blocks = minimal_tool_result_blocks(&json);
        assert_eq!(blocks.len(), 1);
        if let ContentBlock::ToolResult { content, .. } = &blocks[0] {
            assert!(
                content.chars().count() < 500,
                "legacy row should be demoted to a short receipt, got {} chars",
                content.chars().count()
            );
        } else {
            panic!("expected a ToolResult block");
        }
    }

    #[test]
    fn extract_tool_minimals_reads_persisted_receipts_from_history() {
        let mut msg = make_chat_message("s-tool", "user", "", 1);
        msg.tool_results_json = Some(
            serde_json::to_string(&vec![json!({
                "type": "tool_result",
                "tool_use_id": "tool-1",
                "content": "very long full output",
                "is_error": false,
                "tool_name": "shell",
                "content_minimal": "ran shell command"
            })])
            .expect("json"),
        );

        let minimals = extract_tool_minimals_from_history(&[msg]);
        assert_eq!(
            minimals.get("tool-1").map(String::as_str),
            Some("ran shell command")
        );
    }

    #[test]
    fn collapse_superseded_tool_failures_removes_structural_retry_lineage() {
        let msgs = vec![
            text_msg("user", "请读一下 config"),
            assistant_tool_use("call-1", "file_read", json!({"pth":"C:\\temp\\a.txt"})),
            user_tool_result(
                "call-1",
                "[file_read] 工具输入与 schema 不匹配。\n\n[schema_correction tool=file_read]\n{\"type\":\"object\",\"required\":[\"path\"]}\n[/schema_correction]",
                true,
            ),
            text_msg("assistant", "参数名错了，我改成 path 重试。"),
            assistant_tool_use("call-2", "file_read", json!({"path":"C:\\temp\\a.txt"})),
            user_tool_result("call-2", "file content", false),
        ];

        let collapsed = collapse_superseded_tool_failures(msgs);
        assert!(!has_tool_result_content(
            &collapsed,
            "schema_correction tool=file_read"
        ));
        assert!(has_tool_result_content(&collapsed, "file content"));
    }

    #[test]
    fn collapse_superseded_tool_failures_keeps_non_retryable_failures() {
        let msgs = vec![
            text_msg("user", "先写 Program Files"),
            assistant_tool_use(
                "call-1",
                "file_write",
                json!({"path":"C:\\Program Files\\a.txt","content":"x"}),
            ),
            user_tool_result("call-1", "permission denied", true),
            text_msg("assistant", "那我换个文件继续当前任务。"),
            assistant_tool_use(
                "call-2",
                "file_write",
                json!({"path":"C:\\temp\\a.txt","content":"x"}),
            ),
            user_tool_result("call-2", "ok", false),
        ];

        let collapsed = collapse_superseded_tool_failures(msgs);
        assert!(has_tool_result_content(&collapsed, "permission denied"));
        assert!(has_tool_result_content(&collapsed, "ok"));
    }

    #[test]
    fn collapse_superseded_tool_failures_keeps_failures_across_real_user_turns() {
        let msgs = vec![
            text_msg("user", "读 a.txt"),
            assistant_tool_use("call-1", "file_read", json!({"pth":"C:\\temp\\a.txt"})),
            user_tool_result(
                "call-1",
                "[schema_correction tool=file_read]\n{\"type\":\"object\",\"required\":[\"path\"]}\n[/schema_correction]",
                true,
            ),
            text_msg("user", "算了，接着读 b.txt"),
            assistant_tool_use("call-2", "file_read", json!({"path":"C:\\temp\\b.txt"})),
            user_tool_result("call-2", "b content", false),
        ];

        let collapsed = collapse_superseded_tool_failures(msgs);
        assert!(has_tool_result_content(
            &collapsed,
            "schema_correction tool=file_read"
        ));
        assert!(has_tool_result_content(&collapsed, "b content"));
    }
}
