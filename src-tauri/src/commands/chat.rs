use crate::agent::loop_::AgentLoop;
use crate::agent::messages::AgentEvent;
use crate::agent::plan::summarize_todos;
use crate::agent::tool::ToolContext;
use crate::llm::{build_client_with_timeout, ContentBlock, LlmMessage, MessageContent, ToolDef};
use crate::policy::PolicyGate;
use crate::project_context::render_project_instruction_context;
use crate::store::{
    db::ChatMessage, db::Session, db::SessionContextState, db::TaskSpine, db::TaskState, AppState,
};
use crate::tools;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
}

struct ChatPromptArtifacts {
    system_prompt: String,
    registry: Arc<crate::agent::tool::ToolRegistry>,
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
) -> Result<SessionMessageContext, String> {
    let db = db_arc.lock().await;
    let history = db
        .get_messages_latest(session_id, 2000)
        .map_err(|e| e.to_string())?;
    let session_state = db
        .get_session_context_state(session_id)
        .map_err(|e| e.to_string())?;
    let rolling_summary = session_state
        .as_ref()
        .map(|s| s.rolling_summary.as_str())
        .unwrap_or("");
    let latest_user_text = history
        .iter()
        .rev()
        .find(|m| m.role == "user" && !m.content.trim().is_empty())
        .map(|m| m.content.clone())
        .unwrap_or_default();
    let llm_messages = build_context_messages(
        &history,
        budget,
        (!rolling_summary.trim().is_empty()).then_some(rolling_summary),
    );
    Ok(SessionMessageContext {
        llm_messages,
        session_state,
        latest_user_text,
    })
}

async fn build_session_message_context(
    state: &State<'_, AppState>,
    session_id: &str,
    budget: usize,
) -> Result<SessionMessageContext, String> {
    build_session_message_context_from_db(&state.db, session_id, budget).await
}

async fn build_chat_prompt_artifacts(
    app: &AppHandle,
    state: &State<'_, AppState>,
    session_id: &str,
    query_text: &str,
    workspace_root: &str,
    context_window: u32,
    allow_outside_workspace: bool,
    builtin_tool_enabled: &std::collections::HashMap<String, bool>,
    project_instruction_budget_chars: u32,
    enable_project_instructions: bool,
) -> Result<ChatPromptArtifacts, String> {
    let user_tools_dir = app.path().app_data_dir().map(|d| d.join("user-tools")).ok();
    let app_data_dir = app.path().app_data_dir().ok();

    let (skill_context, skill_loader_arc) = {
        let app_dir = app
            .path()
            .app_data_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
        let skills_dir = app_dir.join("skills");
        let mut loader = crate::skills::loader::SkillLoader::new(skills_dir);
        if let Err(e) = loader.load_all() {
            tracing::warn!("Failed to load skills: {}", e);
        }
        let enabled_names: Vec<String> = {
            let db = state.db.lock().await;
            db.list_skills()
                .unwrap_or_default()
                .into_iter()
                .filter(|s| s.enabled)
                .map(|s| s.name)
                .collect()
        };
        let dir = loader.generate_skill_directory(&enabled_names);
        let arc = Arc::new(tokio::sync::Mutex::new(loader));
        (dir, arc)
    };

    let registry = Arc::new(tools::build_registry(
        state.browser.clone(),
        user_tools_dir.as_deref(),
        Some(state.db.clone()),
        Some(builtin_tool_enabled),
        Some(app.clone()),
        Some(state.settings.clone()),
        app_data_dir,
        Some(skill_loader_arc),
    ));
    let tool_defs = registry.to_tool_defs();

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

    let task_state_context = {
        let db = state.db.lock().await;
        match db.load_task_state("session", session_id) {
            Ok(Some(ts))
                if ts.status == "active" && (!ts.goal.is_empty() || !ts.summary.is_empty()) =>
            {
                render_task_state_section("Active Task State", "Progress", &ts)
            }
            _ => String::new(),
        }
    };

    let injection_budget = compute_injection_budget(context_window);
    let full_memory_context = budget_truncate(
        &format!("{}{}", memory_context, task_state_context),
        injection_budget,
    );
    let project_instruction_context = if enable_project_instructions {
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

    let mut system_prompt = build_system_prompt_with_env(
        &full_memory_context,
        &skill_context,
        workspace_root,
        allow_outside_workspace,
    );
    if !project_instruction_context.is_empty() {
        system_prompt.push_str(&project_instruction_context);
    }
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
            std::sync::Arc::new(crate::agent::tool::ToolSettings::from_settings(&settings)),
            settings.max_iterations,
            settings.builtin_tool_enabled.clone(),
            settings.allow_outside_workspace,
            settings.vision_enabled,
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
            crate::policy::PolicyDecision::Deny(reason) => {
                tracing::warn!(
                    "chat_send: user input rejected by injection detection: {}",
                    reason
                );
                return Err(format!("Input rejected: {}", reason));
            }
            crate::policy::PolicyDecision::Warn(reason) => {
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
            crate::policy::PolicyDecision::Allow => {}
        }
    }

    // Resolve attachment: convert FrontendAttachment → MediaAttachment
    // For non-vision models or non-image files, we append the path to the message text.
    // For vision models + image data, we pass through as MediaAttachment for inline injection.
    let vision_capable = vision_enabled || model_supports_vision(&provider, &model);
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
    crate::agent::vision::clear_selection(&session_id).await;

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
                    source: crate::llm::ImageSource {
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

    let agent = AgentLoop {
        client,
        registry,
        policy,
        system_prompt: prompt_artifacts.system_prompt,
        model: model.clone(),
        max_tokens,
        context_window,
        fallback_models: state.settings.lock().await.fallback_models.clone(),
        db: Some(state.db.clone()),
        app_handle: Some(state.app_handle.clone()),
        confirmation_responses: Some(state.confirmation_responses.clone()),
        confirm_flags: crate::agent::loop_::ConfirmFlags {
            confirm_shell,
            confirm_file_write,
        },
        vision_override: Some(vision_capable),
        notification_rx: None,
        auto_compact_input_tokens_threshold,
    };

    let ctx = ToolContext {
        session_id: session_id.clone(),
        workspace_root: std::path::PathBuf::from(&workspace_root),
        bypass_permissions: false,
        settings: tool_settings,
        max_iterations: Some(max_iterations),
        memory_owner_id: "pisci".to_string(),
        pool_session_id: None,
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
}

pub(crate) const SESSION_SOURCE_IM_PREFIX: &str = "im_";
pub(crate) const SESSION_SOURCE_PISCI_INBOX_GLOBAL: &str = "pisci_inbox_global";
pub(crate) const SESSION_SOURCE_PISCI_INBOX_POOL: &str = "pisci_inbox_pool";
pub(crate) const SESSION_SOURCE_PISCI_INTERNAL: &str = "pisci_internal";

fn normalize_session_source_compat(source: &str) -> &str {
    match source {
        "heartbeat" => SESSION_SOURCE_PISCI_INBOX_GLOBAL,
        "heartbeat_pool" => SESSION_SOURCE_PISCI_INBOX_POOL,
        other => other,
    }
}

fn is_pool_scoped_session_source(source: &str) -> bool {
    normalize_session_source_compat(source) == SESSION_SOURCE_PISCI_INBOX_POOL
}

fn derive_headless_session_source(channel: &str, pool_session_id: Option<&str>) -> String {
    if pool_session_id.is_some() {
        return SESSION_SOURCE_PISCI_INBOX_POOL.to_string();
    }
    match channel {
        "heartbeat" => SESSION_SOURCE_PISCI_INBOX_GLOBAL.to_string(),
        "internal" => SESSION_SOURCE_PISCI_INTERNAL.to_string(),
        other if other.starts_with(SESSION_SOURCE_IM_PREFIX) => other.to_string(),
        other => format!("{}{}", SESSION_SOURCE_IM_PREFIX, other),
    }
}

pub(crate) fn validate_headless_session_scope(
    actual_source: &str,
    desired_source: &str,
    pool_session_id: Option<&str>,
) -> Result<(), String> {
    let actual = normalize_session_source_compat(actual_source);
    let desired = normalize_session_source_compat(desired_source);

    if actual != desired {
        return Err(format!(
            "Session source mismatch: session is '{}' but this run requires '{}'",
            actual_source, desired_source
        ));
    }

    if pool_session_id.is_some() && !is_pool_scoped_session_source(actual) {
        return Err(format!(
            "Pool-scoped run cannot reuse non-pool session source '{}'",
            actual_source
        ));
    }

    if pool_session_id.is_none() && is_pool_scoped_session_source(actual) {
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
        workspace_root,
        max_tokens,
        context_window,
        policy_mode,
        tool_rate_limit_per_minute,
        tool_settings,
        max_iterations,
        builtin_tool_enabled,
        allow_outside_workspace,
        vision_setting,
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
            std::sync::Arc::new(crate::agent::tool::ToolSettings::from_settings(&settings)),
            settings.max_iterations,
            settings.builtin_tool_enabled.clone(),
            settings.allow_outside_workspace,
            settings.vision_enabled,
            settings.llm_read_timeout_secs,
            settings.auto_compact_input_tokens_threshold,
            settings.project_instruction_budget_chars,
            settings.enable_project_instructions,
        )
    };
    if api_key.is_empty() {
        return Err("API key not configured".into());
    }

    {
        let mut plans = state.plan_state.lock().await;
        plans.remove(session_id);
    }
    crate::agent::vision::clear_selection(session_id).await;

    let pool_session_id = options.as_ref().and_then(|o| o.pool_session_id.clone());
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

    // vision_capable: user override OR auto-detection by provider/model name
    let vision_capable = vision_setting || model_supports_vision(&provider, &model);

    // Build the effective user message text, handling inbound media
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
                user_message.to_string()
            }
        } else {
            user_message.to_string()
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
    let registry = Arc::new(tools::build_registry(
        state.browser.clone(),
        user_tools_dir_h.as_deref(),
        Some(state.db.clone()),
        Some(&builtin_tool_enabled),
        Some(state.app_handle.clone()),
        Some(state.settings.clone()),
        app_data_dir_h,
        None, // skill_search not used in IM headless sessions
    ));
    let policy = Arc::new(PolicyGate::with_profile_and_flags(
        &workspace_root,
        &policy_mode,
        tool_rate_limit_per_minute,
        allow_outside_workspace,
    ));

    let scoped_memory_context = {
        let keywords: Vec<&str> = effective_user_message.split_whitespace().take(10).collect();
        let query = keywords.join(" ");
        if query.trim().is_empty() {
            String::new()
        } else {
            let db = state.db.lock().await;
            match db.search_memories_scoped(&query, "pisci", pool_session_id.as_deref(), 5) {
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
    };

    let pool_context = if let Some(pool_id) = pool_session_id.as_deref() {
        let db = state.db.lock().await;
        let pool = db.get_pool_session(pool_id).map_err(|e| e.to_string())?;
        let recent_messages = db
            .get_pool_messages(pool_id, 12, 0)
            .map_err(|e| e.to_string())?;
        let todos = db.list_koi_todos(None).map_err(|e| e.to_string())?;
        let pool_todos: Vec<_> = todos
            .into_iter()
            .filter(|t| t.pool_session_id.as_deref() == Some(pool_id))
            .collect();

        let todo_summary = if pool_todos.is_empty() {
            "No pool todos.".to_string()
        } else {
            let mut counts = std::collections::BTreeMap::<String, usize>::new();
            for todo in &pool_todos {
                *counts.entry(todo.status.clone()).or_insert(0) += 1;
            }
            let parts = counts
                .into_iter()
                .map(|(status, count)| format!("{}={}", status, count))
                .collect::<Vec<_>>()
                .join(", ");
            format!("Pool todos: {}", parts)
        };

        let message_summary = if recent_messages.is_empty() {
            "No recent pool messages.".to_string()
        } else {
            let lines = recent_messages
                .iter()
                .rev()
                .take(6)
                .rev()
                .map(|m| {
                    let content = if m.content.chars().count() > 220 {
                        format!("{}...", m.content.chars().take(220).collect::<String>())
                    } else {
                        m.content.clone()
                    };
                    format!(
                        "- #{} {} [{}]: {}",
                        m.id,
                        m.sender_id,
                        m.msg_type,
                        content.replace('\n', " ")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("Recent pool messages:\n{}", lines)
        };

        let mut ctx = String::new();
        if let Some(pool) = pool {
            ctx.push_str("\n\n## Pool Context\n");
            ctx.push_str(&format!(
                "Pool: {} ({})\nStatus: {}",
                pool.name, pool.id, pool.status
            ));
            if let Some(project_dir) = pool.project_dir {
                ctx.push_str(&format!("\nProject dir: {}", project_dir));
            }
            if !pool.org_spec.trim().is_empty() {
                let org_preview = if pool.org_spec.chars().count() > 800 {
                    format!("{}...", pool.org_spec.chars().take(800).collect::<String>())
                } else {
                    pool.org_spec
                };
                ctx.push_str(&format!("\nOrg spec:\n{}", org_preview));
            }
        }
        ctx.push_str(&format!("\n{}\n{}", todo_summary, message_summary));
        ctx
    } else {
        String::new()
    };
    let task_state_context = {
        let db = state.db.lock().await;
        match db.load_task_state("session", session_id) {
            Ok(Some(ts))
                if ts.status == "active" && (!ts.goal.is_empty() || !ts.summary.is_empty()) =>
            {
                render_task_state_section("Active Task State", "Progress", &ts)
            }
            _ => String::new(),
        }
    };

    let mut system_prompt = build_im_system_prompt(channel, vision_capable);
    if !scoped_memory_context.is_empty() {
        system_prompt.push_str(&scoped_memory_context);
    }
    if !task_state_context.is_empty() {
        system_prompt.push_str(&task_state_context);
    }
    if !pool_context.is_empty() {
        system_prompt.push_str(&pool_context);
    }
    if enable_project_instructions {
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

    let agent = AgentLoop {
        client,
        registry,
        policy,
        system_prompt,
        model,
        max_tokens,
        context_window,
        fallback_models: state.settings.lock().await.fallback_models.clone(),
        db: Some(state.db.clone()),
        app_handle: None,
        confirmation_responses: None,
        confirm_flags: crate::agent::loop_::ConfirmFlags {
            confirm_shell: false,
            confirm_file_write: false,
        },
        vision_override: Some(vision_capable),
        notification_rx: None,
        auto_compact_input_tokens_threshold,
    };
    let ctx = ToolContext {
        session_id: session_id.to_string(),
        workspace_root: std::path::PathBuf::from(&workspace_root),
        bypass_permissions: false,
        settings: tool_settings,
        max_iterations: Some(max_iterations),
        memory_owner_id: "pisci".to_string(),
        pool_session_id: pool_session_id.clone(),
    };
    // Load full conversation history for context.
    // After building LLM messages, sanitize any orphaned tool_use blocks (tool calls without
    // a matching tool_result) that can occur when a previous agent run was cancelled mid-turn.
    // Orphaned tool_use blocks cause API errors and confuse the LLM into re-executing old tasks.
    let session_context = build_session_message_context_from_db(
        &state.db,
        session_id,
        compute_context_budget(context_window, max_tokens),
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
                    source: crate::llm::ImageSource {
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

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut flags = state.cancel_flags.lock().await;
        flags.insert(session_id.to_string(), cancel.clone());
    }

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
                crate::llm::MessageContent::Blocks(blocks) => blocks.iter().find_map(|b| {
                    if let crate::llm::ContentBlock::Image { source } = b {
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

/// Max chars for memory + task_state injected into system prompt.
/// Roughly 15% of context window (in chars, ~4 chars/token).
fn compute_injection_budget(context_window: u32) -> usize {
    let budget_tokens = (context_window as f64 * 0.15) as usize;
    (budget_tokens * 4).max(2_000)
}

pub fn build_system_prompt(memory_context: &str, skill_context: &str) -> String {
    build_system_prompt_with_env(memory_context, skill_context, "", false)
}

pub fn build_system_prompt_with_env(
    memory_context: &str,
    skill_context: &str,
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
    format!(
        r#"You are Pisci, a powerful Windows AI Agent. You run on the user's local Windows machine and can control the entire desktop environment.
Today's date: {date}{workspace_line}

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
→ Then `uia` to interact with the application

**Checking if a process is running / killing a process:**
→ Use `process_control` with `action: "is_running"` or `action: "kill"`
→ Do NOT use shell+tasklist or taskkill for this — process_control returns structured data

**Querying Windows system info (processes, services, registry, installed apps):**
→ Use `powershell_query` — returns structured JSON, faster than raw shell
→ For 32-bit registry (WOW6432Node) or 32-bit COM: add `arch: "x86"` to powershell_query

**Querying hardware info (CPU, RAM, GPU, BIOS, disks):**
→ Use `wmi` with a preset — faster and more reliable than PowerShell for hardware

**Interacting with COM/ActiveX objects (legacy industrial/CAD software):**
→ Use `com_invoke` — supports any ProgID, 32-bit or 64-bit
→ For 32-bit COM (most legacy software): `com_invoke` with `arch: "x86"`
→ To check if a ProgID exists: `com_invoke` with `action: "create"`, `arch: "x86"`

**Automating desktop apps (clicking buttons, typing in forms):**
→ Use `uia` — works with any Windows app via UI Automation
→ Workflow: `uia(list_windows)` → `uia(find)` → `uia(click/type/get_value)`
→ `uia(get_value)` and `uia(get_text)` now read actual control content (not just the label)
→ If uia cannot find an element: use `screen_capture` to see the screen, then `uia` with x/y coords

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

**When NOT to use call_fish:**
- The task requires back-and-forth with the user (Fish cannot interact with the user)
- You need to build on intermediate results across multiple dependent steps that require your judgment
- The task is simple enough that one or two tool calls will suffice

**Best practices:**
1. First call `call_fish(action="list")` to see which Fish are available and what they specialize in
2. Write a clear, complete task description — include all necessary context (paths, requirements, constraints) since the Fish has no access to your conversation history
3. The Fish returns only its final result — all intermediate reasoning and tool calls are discarded, saving your context budget
4. If no Fish is available for the task, handle it yourself as usual

**Example delegation pattern:**
- User asks: "帮我整理 C:\Projects 下所有 Python 项目的依赖清单"
- Good: `call_fish(action="call", fish_id="file-management", task="扫描 C:\\Projects 下所有包含 requirements.txt 或 pyproject.toml 的目录，列出每个项目名称及其依赖列表")`
- The Fish will do all the scanning, reading, and aggregation internally, and return only the final summary

## Multi-Agent Collaboration (call_koi + pool_org)

You are the project manager. When a user discusses a project that requires sustained, multi-role effort, you should proactively organize a collaborative project:

**1. Understand the project through conversation**
- Ask clarifying questions about goals, scope, timeline, and constraints
- Identify distinct roles/responsibilities needed (e.g., frontend dev, backend dev, tester, doc writer)

**2. Set up the project pool using `pool_org`**
- `pool_org(action="list")` — see existing pools and available Koi agents
- `pool_org(action="create", name="<project name>", org_spec="<markdown>")` — create a new project pool with a comprehensive organization spec
- The org_spec should define: project goals, Koi role assignments, collaboration rules, activation conditions, and success metrics

**3. Communicate with Koi agents via @mention**
- Use `pool_chat(action="send", pool_id=..., content="@KoiName your task description...")` to assign work. The @mention system automatically activates the Koi and assigns the task.
- You can @mention multiple Koi in one message, or use `@all` to broadcast to all agents.
- Example: `pool_chat(action="send", pool_id="abc", sender_id="pisci", content="@Architect Design the database schema for user authentication. When done, hand off to @Coder for implementation.")`
- After the Koi completes, its result @mentions cascade automatically — if Architect writes "@Coder please implement", Coder is activated without your intervention.
- Use `pool_chat(action="read", pool_id=...)` or `pool_org(action="get_todos", pool_id=...)` to monitor progress.
- Koi agents are fully autonomous: they communicate via pool_chat, share results, and collaborate with each other through @mentions. Do NOT micromanage their approach.
- Every Koi may declare a free-form `role` plus a detailed description. Use both fields to understand their specialization before assigning work.
- **IMPORTANT**: Do NOT use `pool_org(action="assign_koi")` for normal task assignment. Use @mention in pool_chat instead — this is the natural communication channel that all agents share.

**4. Evolve the org_spec as the project progresses**
- `pool_org(action="read", pool_id=...)` — review current org_spec
- `pool_org(action="update", pool_id=..., org_spec="...")` — update as requirements change

**When to initiate multi-agent collaboration:**
- The project has multiple distinct work streams that benefit from specialization
- The user describes a sustained effort, not a one-off task
- Different parts of the work require different skills or perspectives

**CRITICAL — Before creating a new project pool:**
1. ALWAYS call `pool_org(action="list")` first to see all existing pools.
2. If there is an active or paused pool that is related to the user's request, DO NOT create a new pool. Instead, add a new task to the existing pool via pool_chat @mention or `pool_org(action="create_todo", pool_id="...")`.
3. Only create a new pool when the work is genuinely a separate, independent project with no overlap with existing pools.
4. When in doubt, ask the user: "Should I add this to the existing project '<name>', or start a new project?"

**Key principles:**
- You decide the organizational structure; the user approves it
- Each Koi has full capabilities — do not micromanage their approach
- The pool chat room and kanban board are observation windows for the user, not control surfaces
- Prefer fewer, well-defined Koi roles over many fragmented ones
- All agent communication flows through pool_chat @mentions — this is how Koi hand off work, ask questions, and collaborate naturally
- **Never create a new project for work that belongs to an existing unfinished project**

**5. Task Lifecycle Management**
- When a Koi reports completion via pool_chat, review the result. If satisfactory, mark the todo as done: `pool_org(action="complete_todo", todo_id="...")`.
- If a task is no longer needed (scope change, duplicate, superseded), cancel it: `pool_org(action="cancel_todo", todo_id="...", reason="...")`. You can cancel ANY Koi's todo — you have global task authority.
- Monitor blocked tasks with `pool_org(action="get_todos")`. If a task is stuck, unblock or reassign it.
- Task status flow: `todo` → `in_progress` → `done` / `cancelled` / `blocked`. Only Pisci and the task owner can change status. Other Koi must @pisci to request task changes.
- When the project is complete, ensure all remaining todos are either completed or cancelled before archiving the pool.
- **Project completion flow**: When all tasks are done, summarize results for the user and ask for confirmation before archiving. Then call `pool_org(action="archive", pool_id=...)`. Only Pisci can archive a project — Koi should @pisci when they believe all work is finished.
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
- **You are responsible for merging Koi branches into master.** Koi cannot and should not merge themselves. Call `pool_org(action="merge_branches", pool_id=...)` to merge all completed `koi/*` branches into master.
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

Use diagrams proactively when they make information clearer. Keep them concise.{memory}{skills}"#,
        date = chrono::Utc::now().format("%Y-%m-%d"),
        memory = memory_context,
        skills = if skill_context.is_empty() {
            String::new()
        } else {
            format!(
                "\n\n## Available Skills\n\
                 Call `skill_list` to browse all skills (name + description + SKILL.md path).\n\
                 Once you identify a matching skill, use `file_read` to load its SKILL.md and follow it.\n\n\
                 {}",
                skill_context
            )
        }
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
    let can_send_file = matches!(channel, "feishu");

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
    crate::llm::estimate_tokens(text)
}

// ---------------------------------------------------------------------------
// Context management helpers
// ---------------------------------------------------------------------------

/// Compute the token budget for `build_context_messages` from settings.
/// Delegates to `llm::compute_context_budget`.
pub fn compute_context_budget(context_window: u32, max_tokens: u32) -> usize {
    crate::llm::compute_context_budget(context_window, max_tokens)
}

/// How many recent conversation turns to keep with full tool call detail.
const CTX_FULL_TURNS: usize = 3;
/// Turns beyond this count are compressed to a single summary message.
const CTX_COMPACT_AFTER: usize = 8;
/// Characters to keep from the head of a trimmed tool result.
const CTX_TRIM_HEAD: usize = 1000;
/// Characters to keep from the tail of a trimmed tool result.
const CTX_TRIM_TAIL: usize = 300;

/// Persist the completed agent turn to the database with full tool call structure.
///
/// Writes one row per logical message:
/// - intermediate assistant messages (with tool_calls_json)
/// - intermediate tool-result user messages (with tool_results_json)
/// - final assistant text message (plain content, no tool data)
///
/// All rows for this turn share the same `turn_index` derived from the current
/// message count in the session.
/// Persist only the *new* messages produced by the agent loop.
/// Strip `SEND_FILE:` and `SEND_IMAGE:` marker lines from text before persisting to DB.
/// These lines are consumed by the gateway for file dispatch and must not appear in chat history.
pub(crate) fn strip_send_markers(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("SEND_FILE:") && !text.contains("SEND_IMAGE:") {
        return std::borrow::Cow::Borrowed(text);
    }
    let cleaned: String = text
        .lines()
        .filter(|line| {
            let t = line.trim();
            !t.starts_with("SEND_FILE:") && !t.starts_with("SEND_IMAGE:")
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::borrow::Cow::Owned(cleaned.trim().to_string())
}

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
        .map(|message| crate::llm::estimate_message_tokens(&message))
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
                    let trimmed_blocks = trim_tool_result_blocks(results_json);
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
fn sanitize_tool_call_pairs(messages: Vec<LlmMessage>) -> Vec<LlmMessage> {
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

/// Build tool result blocks with trimmed content for middle-tier turns.
fn trim_tool_result_blocks(results_json: &str) -> Vec<ContentBlock> {
    let blocks: Vec<ContentBlock> = serde_json::from_str(results_json).unwrap_or_default();
    blocks
        .into_iter()
        .map(|b| {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = b
            {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content: trim_tool_result(&content, CTX_TRIM_HEAD, CTX_TRIM_TAIL),
                    is_error,
                }
            } else {
                b
            }
        })
        .collect()
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

pub(crate) fn collapse_superseded_tool_failures(mut msgs: Vec<LlmMessage>) -> Vec<LlmMessage> {
    let mut tool_use_meta: HashMap<String, (usize, String)> = HashMap::new();
    let mut last_success_pos: HashMap<String, usize> = HashMap::new();

    for (msg_idx, msg) in msgs.iter().enumerate() {
        let MessageContent::Blocks(blocks) = &msg.content else {
            continue;
        };

        for block in blocks {
            if let ContentBlock::ToolUse { id, name, input } = block {
                tool_use_meta.insert(id.clone(), (msg_idx, tool_call_signature(name, input)));
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
                if let Some((tool_msg_idx, signature)) = tool_use_meta.get(tool_use_id) {
                    last_success_pos
                        .entry(signature.clone())
                        .and_modify(|pos| *pos = (*pos).max(*tool_msg_idx))
                        .or_insert(*tool_msg_idx);
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
                ..
            } = block
            {
                if !*is_error {
                    continue;
                }
                if let Some((tool_msg_idx, signature)) = tool_use_meta.get(tool_use_id) {
                    if last_success_pos
                        .get(signature)
                        .is_some_and(|success_pos| tool_msg_idx < success_pos)
                    {
                        superseded_tool_use_ids.insert(tool_use_id.clone());
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
fn sanitize_tool_use_result_pairing(mut msgs: Vec<LlmMessage>) -> Vec<LlmMessage> {
    let mut i = 0;
    while i < msgs.len() {
        let has_tool_use = if msgs[i].role == "assistant" {
            match &msgs[i].content {
                crate::llm::MessageContent::Blocks(blocks) => blocks
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
                        crate::llm::MessageContent::Blocks(blocks) => blocks
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
            if let crate::llm::MessageContent::Blocks(ref mut blocks) = msgs[i].content {
                blocks.retain(|b| !matches!(b, ContentBlock::ToolUse { .. }));
            }
            // If the message is now empty, remove it
            let is_empty = match &msgs[i].content {
                crate::llm::MessageContent::Blocks(blocks) => blocks.is_empty(),
                crate::llm::MessageContent::Text(t) => t.trim().is_empty(),
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
    messages: Vec<crate::llm::LlmMessage>,
    client: Box<dyn crate::llm::LlmClient>,
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

    let req = crate::llm::LlmRequest {
        messages: vec![crate::llm::LlmMessage {
            role: "user".into(),
            content: crate::llm::MessageContent::text(&extraction_prompt),
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
        allow_outside_workspace,
        &builtin_tool_enabled,
        project_instruction_budget_chars,
        enable_project_instructions,
    )
    .await?;
    let llm_messages = session_context.llm_messages;
    let session_state = session_context.session_state;
    let llm_messages =
        crate::agent::vision::inject_selected_context(&llm_messages, &session_id).await;

    // Convert LlmMessages to preview-friendly structs with structured blocks
    let messages: Vec<ContextPreviewMessage> = llm_messages
        .iter()
        .map(|m| {
            let blocks: Vec<ContextPreviewBlock> = match &m.content {
                crate::llm::MessageContent::Text(t) => {
                    if t.is_empty() {
                        vec![]
                    } else {
                        vec![ContextPreviewBlock::Text { text: t.clone() }]
                    }
                }
                crate::llm::MessageContent::Blocks(raw_blocks) => raw_blocks
                    .iter()
                    .map(|b| match b {
                        crate::llm::ContentBlock::Text { text } => {
                            ContextPreviewBlock::Text { text: text.clone() }
                        }
                        crate::llm::ContentBlock::ToolUse { id, name, input } => {
                            ContextPreviewBlock::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: serde_json::to_string_pretty(input).unwrap_or_default(),
                            }
                        }
                        crate::llm::ContentBlock::ToolResult {
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
                        crate::llm::ContentBlock::Image { .. } => ContextPreviewBlock::Image {
                            note: "[image attachment]".to_string(),
                        },
                    })
                    .collect(),
            };
            let tokens = crate::llm::estimate_message_tokens(m);
            ContextPreviewMessage {
                role: m.role.clone(),
                blocks,
                tokens,
            }
        })
        .collect();

    let messages_tokens: usize = messages.iter().map(|m| m.tokens).sum();
    let request_overhead_tokens = crate::llm::estimate_request_overhead_tokens(
        Some(&prompt_artifacts.system_prompt),
        &prompt_artifacts.tool_defs,
    );
    let total_input_budget = crate::llm::compute_total_input_budget(context_window, max_tokens);
    let total_tokens = crate::llm::estimate_request_input_tokens(
        &llm_messages,
        Some(&prompt_artifacts.system_prompt),
        &prompt_artifacts.tool_defs,
    );

    Ok(ContextPreview {
        messages,
        messages_tokens,
        total_tokens,
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
    use super::build_context_messages;
    use crate::store::db::ChatMessage;
    use chrono::Utc;

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
}
