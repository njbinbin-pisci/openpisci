use crate::agent::loop_::AgentLoop;
use crate::agent::messages::AgentEvent;
use crate::agent::tool::ToolContext;
use crate::llm::{build_client, LlmMessage, MessageContent};
use crate::policy::PolicyGate;
use crate::store::{db::ChatMessage, db::Session, AppState};
use crate::tools;
use serde::Serialize;
use std::sync::{atomic::AtomicBool, Arc};
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Debug, Serialize)]
pub struct SessionList {
    pub sessions: Vec<Session>,
    pub total: usize,
}

#[tauri::command]
pub async fn create_session(
    state: State<'_, AppState>,
    title: Option<String>,
) -> Result<Session, String> {
    let db = state.db.lock().await;
    db.create_session(title.as_deref()).map_err(|e| e.to_string())
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
pub async fn delete_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
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
    db.rename_session(&session_id, &title).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_messages(
    state: State<'_, AppState>,
    session_id: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<ChatMessage>, String> {
    let db = state.db.lock().await;
    db.get_messages(&session_id, limit.unwrap_or(100), offset.unwrap_or(0))
        .map_err(|e| e.to_string())
}

/// Send a user message and run the agent loop.
/// Streams AgentEvents to the frontend via Tauri events.
#[tauri::command]
pub async fn chat_send(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    content: String,
) -> Result<(), String> {
    tracing::info!("chat_send called: session={} content_len={}", session_id, content.len());

    // Load settings
    let (provider, model, api_key, base_url, workspace_root, max_tokens, confirm_shell, confirm_file_write, policy_mode, tool_rate_limit_per_minute, tool_settings, max_iterations) = {
        let settings = state.settings.lock().await;
        (
            settings.provider.clone(),
            settings.model.clone(),
            settings.active_api_key().to_string(),
            settings.custom_base_url.clone(),
            settings.workspace_root.clone(),
            settings.max_tokens,
            settings.confirm_shell_commands,
            settings.confirm_file_writes,
            settings.policy_mode.clone(),
            settings.tool_rate_limit_per_minute,
            std::sync::Arc::new(crate::agent::tool::ToolSettings::from_settings(&settings)),
            settings.max_iterations,
        )
    };

    tracing::info!("chat_send: provider={} model={} api_key_empty={}", provider, model, api_key.is_empty());

    if api_key.is_empty() {
        tracing::warn!("chat_send: API key not configured");
        return Err("API key not configured. Please open Settings to configure your API key.".into());
    }

    // Prompt injection detection on user input
    {
        let gate = PolicyGate::with_profile(&workspace_root, &policy_mode, tool_rate_limit_per_minute);
        let decision = gate.check_user_input(&content);
        match decision {
            crate::policy::PolicyDecision::Deny(reason) => {
                tracing::warn!("chat_send: user input rejected by injection detection: {}", reason);
                return Err(format!("Input rejected: {}", reason));
            }
            crate::policy::PolicyDecision::Warn(reason) => {
                tracing::warn!("chat_send: potential injection detected (proceeding): {}", reason);
                let db = state.db.lock().await;
                let _ = db.append_audit(&session_id, "injection_detection", "warn", Some(&reason), None, false);
            }
            crate::policy::PolicyDecision::Allow => {}
        }
    }

    // Save user message to DB
    {
        let db = state.db.lock().await;
        db.append_message(&session_id, "user", &content)
            .map_err(|e| e.to_string())?;
        db.update_session_status(&session_id, "running")
            .map_err(|e| e.to_string())?;
    }

    // Load message history and trim to fit context window.
    // Token estimate: CJK chars ≈ 1 token each, ASCII ≈ 1 token per 4 chars.
    // Reserve 25% for response + system prompt overhead (~2000 tokens).
    let context_limit = match max_tokens {
        t if t >= 8192 => 100_000usize,
        t if t >= 4096 => 60_000,
        _ => 30_000,
    };
    let system_prompt_overhead = 2_000usize;
    let budget = ((context_limit as f64 * 0.75) as usize).saturating_sub(system_prompt_overhead);

    let llm_messages = {
        let db = state.db.lock().await;
        let history = db.get_messages(&session_id, 200, 0)
            .map_err(|e| e.to_string())?;

        let mut msgs: Vec<LlmMessage> = Vec::new();
        let mut token_est: usize = 0;
        for m in history.iter().rev() {
            let tokens = estimate_tokens(&m.content);
            if token_est + tokens > budget && !msgs.is_empty() {
                break;
            }
            msgs.push(LlmMessage {
                role: m.role.clone(),
                content: MessageContent::text(&m.content),
            });
            token_est += tokens;
        }
        msgs.reverse();
        msgs
    };

    // Build cancellation token
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut flags = state.cancel_flags.lock().await;
        flags.insert(session_id.clone(), cancel.clone());
    }

    // Build agent components
    let client = build_client(
        &provider,
        &api_key,
        if base_url.is_empty() { None } else { Some(&base_url) },
    );

    let user_tools_dir = app
        .path()
        .app_data_dir()
        .map(|d| d.join("user-tools"))
        .ok();
    let registry = Arc::new(tools::build_registry(
        state.browser.clone(),
        user_tools_dir.as_deref(),
        Some(state.db.clone()),
    ));

    let policy = Arc::new(PolicyGate::with_profile(&workspace_root, &policy_mode, tool_rate_limit_per_minute));

    // Load skills context
    let skill_context = {
        let app_dir = app.path().app_data_dir().unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
        let skills_dir = app_dir.join("skills");
        let mut loader = crate::skills::loader::SkillLoader::new(skills_dir);
        if let Err(e) = loader.load_all() {
            tracing::warn!("Failed to load skills: {}", e);
        }
        let all_names: Vec<String> = loader.list_skills().iter().map(|s| s.name.clone()).collect();
        loader.generate_skill_prompt(&all_names)
    };

    // Inject relevant memories into the system prompt
    let memory_context = {
        let db = state.db.lock().await;
        let keywords: Vec<&str> = content.split_whitespace().take(10).collect();
        let query = keywords.join(" ");
        match db.search_memories_fts(&query, 5) {
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

    let model_for_host = model.clone();
    let agent = AgentLoop {
        client,
        registry,
        policy,
        system_prompt: build_system_prompt(&memory_context, &skill_context),
        model: model.clone(),
        max_tokens,
        db: Some(state.db.clone()),
        app_handle: Some(state.app_handle.clone()),
        confirmation_responses: Some(state.confirmation_responses.clone()),
        confirm_flags: crate::agent::loop_::ConfirmFlags {
            confirm_shell,
            confirm_file_write,
        },
    };

    let ctx = ToolContext {
        session_id: session_id.clone(),
        workspace_root: std::path::PathBuf::from(&workspace_root),
        bypass_permissions: false,
        settings: tool_settings,
        max_iterations: Some(max_iterations),
    };

    // Check if the request should be decomposed by HostAgent
    let sub_tasks = if crate::agent::host::HostAgent::should_decompose(&content) {
        let host_client = build_client(
            &provider, &api_key,
            if base_url.is_empty() { None } else { Some(&base_url) },
        );
        let host_agent = crate::agent::host::HostAgent::new(host_client, model_for_host.clone(), max_tokens);
        match host_agent.decompose_task(&content).await {
            Ok(tasks) if tasks.len() > 1 => {
                tracing::info!("HostAgent decomposed into {} sub-tasks", tasks.len());
                Some(tasks)
            }
            _ => None,
        }
    } else {
        None
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

    tracing::info!("chat_send: spawning agent background task for session={}", session_id);

    tokio::spawn(async move {
        tracing::info!("agent task started for session={}", session_id_clone);

        // Forward events to frontend
        let app_fwd = app_clone.clone();
        let sid_fwd = session_id_clone.clone();
        let forward_handle = tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                tracing::debug!("forwarding event to frontend: session={}", sid_fwd);
                let payload = serde_json::to_value(&event).unwrap_or_default();
                let emit_result = app_fwd.emit(&format!("agent_event_{}", sid_fwd), payload.clone());
                if let Err(e) = emit_result {
                    tracing::warn!("failed to emit event: {}", e);
                }
                // Broadcast to overlay window (subscribes to "agent_broadcast")
                let _ = app_fwd.emit("agent_broadcast", payload);
            }
        });

        // Run agent loop — optionally with HostAgent sub-task decomposition
        // NOTE: agent.run() no longer emits Done — we do it here AFTER the DB write.
        let result = if let Some(tasks) = sub_tasks {
            let plan_text = tasks.iter().enumerate()
                .map(|(i, t)| format!("{}. {}", i + 1, t.description))
                .collect::<Vec<_>>().join("\n");
            let _ = event_tx.send(AgentEvent::TextDelta {
                delta: format!("📋 Task decomposed into {} sub-tasks:\n{}\n\n", tasks.len(), plan_text),
            }).await;

            let mut all_messages = llm_messages;
            let mut total_in = 0u32;
            let mut total_out = 0u32;
            let mut last_err: Option<anyhow::Error> = None;
            for task in &tasks {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) { break; }
                let _ = event_tx.send(AgentEvent::TextDelta {
                    delta: format!("▶ Sub-task: {}\n", task.description),
                }).await;
                let sub_msgs = [LlmMessage {
                    role: "user".into(),
                    content: MessageContent::text(format!(
                        "Execute this sub-task: {}\n\nContext from previous steps is available in the conversation.",
                        task.description
                    )),
                }];
                let combined: Vec<LlmMessage> = all_messages.iter().chain(sub_msgs.iter()).cloned().collect();
                let (sub_tx, mut sub_rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);
                let event_tx_c = event_tx.clone();
                let fwd = tokio::spawn(async move {
                    while let Some(ev) = sub_rx.recv().await {
                        let _ = event_tx_c.send(ev).await;
                    }
                });
                match agent.run(combined, sub_tx, cancel.clone(), ctx.clone()).await {
                    Ok((msgs, ti, to)) => {
                        all_messages = msgs;
                        total_in += ti;
                        total_out += to;
                    }
                    Err(e) => { last_err = Some(e); break; }
                }
                let _ = fwd.await;
            }
            match last_err {
                Some(e) => Err(e),
                None => Ok((all_messages, total_in, total_out)),
            }
        } else {
            tracing::info!("calling agent.run for session={}", session_id_clone);
            agent.run(llm_messages, event_tx.clone(), cancel.clone(), ctx).await
        };

        tracing::info!("agent.run completed for session={} ok={}", session_id_clone, result.is_ok());

        // ── Critical: persist to DB BEFORE emitting Done ───────────────────────────
        // The frontend calls getMessages() on the Done event. If we emit Done first,
        // the frontend reads the DB before the write completes → empty history.
        match &result {
            Ok((final_messages, total_in, total_out)) => {
                // Save the last assistant text response (the final answer).
                let assistant_text = final_messages.iter().rev()
                    .find(|m| m.role == "assistant")
                    .map(|m| m.content.as_text())
                    .unwrap_or_default();

                {
                    let db = db_arc.lock().await;
                    if !assistant_text.is_empty() {
                        let _ = db.append_message(&session_id_clone, "assistant", &assistant_text);
                        tracing::info!("Saved assistant message ({} chars) for session={}", assistant_text.len(), session_id_clone);
                    } else {
                        tracing::warn!("Assistant produced empty text for session={}", session_id_clone);
                    }
                    let _ = db.update_session_status(&session_id_clone, "idle");
                }

                // Auto-extract memories from this conversation (non-blocking, best-effort)
                {
                    let db_for_mem = db_arc.clone();
                    let sid_for_mem = session_id_clone.clone();
                    let msgs_for_mem = final_messages.clone();
                    let model_for_mem = model_clone.clone();
                    let mem_client = build_client(
                        &provider_clone,
                        &api_key_clone,
                        if base_url_clone.is_empty() { None } else { Some(&base_url_clone) },
                    );
                    tokio::spawn(async move {
                        auto_extract_memories(db_for_mem, sid_for_mem, msgs_for_mem, mem_client, model_for_mem, max_tokens_clone).await;
                    });
                }

                // NOW emit Done — frontend getMessages() will see the persisted data
                let _ = event_tx.send(AgentEvent::Done {
                    total_input_tokens: *total_in,
                    total_output_tokens: *total_out,
                }).await;
            }
            Err(e) => {
                tracing::warn!("Agent loop error for session {}: {}", session_id_clone, e);
                {
                    let db = db_arc.lock().await;
                    let _ = db.update_session_status(&session_id_clone, "idle");
                }
                // Emit error event (Done is not sent on error)
                let _ = event_tx.send(AgentEvent::Error {
                    message: e.to_string(),
                }).await;
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
pub async fn run_agent_headless(
    state: &AppState,
    session_id: &str,
    user_message: &str,
) -> Result<String, String> {
    let (provider, model, api_key, base_url, workspace_root, max_tokens, policy_mode, tool_rate_limit_per_minute, tool_settings, max_iterations) = {
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
            std::sync::Arc::new(crate::agent::tool::ToolSettings::from_settings(&settings)),
            settings.max_iterations,
        )
    };
    if api_key.is_empty() {
        return Err("API key not configured".into());
    }
    {
        let db = state.db.lock().await;
        let _ = db.append_message(session_id, "user", user_message);
    }

    let client = build_client(
        &provider, &api_key,
        if base_url.is_empty() { None } else { Some(&base_url) },
    );
    let user_tools_dir_h = state
        .app_handle
        .path()
        .app_data_dir()
        .map(|d| d.join("user-tools"))
        .ok();
    let registry = Arc::new(tools::build_registry(
        state.browser.clone(),
        user_tools_dir_h.as_deref(),
        Some(state.db.clone()),
    ));
    let policy = Arc::new(PolicyGate::with_profile(&workspace_root, &policy_mode, tool_rate_limit_per_minute));

    let agent = AgentLoop {
        client, registry, policy,
        system_prompt: build_system_prompt("", ""),
        model, max_tokens,
        db: Some(state.db.clone()),
        app_handle: None,
        confirmation_responses: None,
        confirm_flags: crate::agent::loop_::ConfirmFlags {
            confirm_shell: false,
            confirm_file_write: false,
        },
    };
    let ctx = ToolContext {
        session_id: session_id.to_string(),
        workspace_root: std::path::PathBuf::from(&workspace_root),
        bypass_permissions: false,
        settings: tool_settings,
        max_iterations: Some(max_iterations),
    };
    let msgs = vec![LlmMessage {
        role: "user".into(),
        content: MessageContent::text(user_message),
    }];
    let cancel = Arc::new(AtomicBool::new(false));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);
    tokio::spawn(async move { while event_rx.recv().await.is_some() {} });

    let (final_msgs, _, _) = agent.run(msgs, event_tx, cancel, ctx).await.map_err(|e| e.to_string())?;

    let response_text = final_msgs.iter().rev()
        .find(|m| m.role == "assistant")
        .map(|m| m.content.as_text())
        .unwrap_or_default();

    {
        let db = state.db.lock().await;
        if !response_text.is_empty() {
            let _ = db.append_message(session_id, "assistant", &response_text);
        }
    }
    Ok(response_text)
}

/// Fish-specific chat send: same as chat_send but with a custom system prompt and tool filter.
/// Called by `commands::fish::fish_chat_send`.
pub async fn fish_chat_send_impl(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    content: String,
    system_prompt_override: Option<String>,
    max_iterations_override: u32,
    allowed_tools: Vec<String>,
) -> Result<(), String> {
    tracing::info!("fish_chat_send_impl: session={} fish_tools={:?}", session_id, allowed_tools);

    let (provider, model, api_key, base_url, workspace_root, max_tokens, confirm_shell, confirm_file_write, policy_mode, tool_rate_limit_per_minute, tool_settings) = {
        let settings = state.settings.lock().await;
        (
            settings.provider.clone(),
            settings.model.clone(),
            settings.active_api_key().to_string(),
            settings.custom_base_url.clone(),
            settings.workspace_root.clone(),
            settings.max_tokens,
            settings.confirm_shell_commands,
            settings.confirm_file_writes,
            settings.policy_mode.clone(),
            settings.tool_rate_limit_per_minute,
            std::sync::Arc::new(crate::agent::tool::ToolSettings::from_settings(&settings)),
        )
    };

    if api_key.is_empty() {
        return Err("API key not configured".into());
    }

    {
        let db = state.db.lock().await;
        db.append_message(&session_id, "user", &content).map_err(|e| e.to_string())?;
        db.update_session_status(&session_id, "running").map_err(|e| e.to_string())?;
    }

    let llm_messages = {
        let db = state.db.lock().await;
        let history = db.get_messages(&session_id, 100, 0).map_err(|e| e.to_string())?;
        let budget = 40_000usize;
        let mut msgs: Vec<LlmMessage> = Vec::new();
        let mut token_est: usize = 0;
        for m in history.iter().rev() {
            let tokens = estimate_tokens(&m.content);
            if token_est + tokens > budget && !msgs.is_empty() { break; }
            msgs.push(LlmMessage { role: m.role.clone(), content: MessageContent::text(&m.content) });
            token_est += tokens;
        }
        msgs.reverse();
        msgs
    };

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut flags = state.cancel_flags.lock().await;
        flags.insert(session_id.clone(), cancel.clone());
    }

    let client = build_client(&provider, &api_key, if base_url.is_empty() { None } else { Some(&base_url) });

    let user_tools_dir = app.path().app_data_dir().map(|d| d.join("user-tools")).ok();
    // Build full registry — tool filtering for Fish is enforced via the allowed_tools list
    // passed to the LLM tool definitions (future enhancement); for now use full registry.
    let registry = Arc::new(tools::build_registry(state.browser.clone(), user_tools_dir.as_deref(), Some(state.db.clone())));

    let policy = Arc::new(PolicyGate::with_profile(&workspace_root, &policy_mode, tool_rate_limit_per_minute));

    // Build system prompt: fish override or default
    let memory_context = {
        let db = state.db.lock().await;
        let keywords: Vec<&str> = content.split_whitespace().take(10).collect();
        match db.search_memories_fts(&keywords.join(" "), 5) {
            Ok(mems) if !mems.is_empty() => {
                let mut ctx = String::from("\n\n## Personal Context (from memory)\n");
                for m in &mems { ctx.push_str(&format!("- {}\n", m.content)); }
                ctx
            }
            _ => String::new(),
        }
    };

    let system_prompt = match system_prompt_override {
        Some(p) => format!("{}{}", p, memory_context),
        None => build_system_prompt(&memory_context, ""),
    };

    let agent = crate::agent::loop_::AgentLoop {
        client,
        registry,
        policy,
        system_prompt,
        model: model.clone(),
        max_tokens,
        db: Some(state.db.clone()),
        app_handle: Some(state.app_handle.clone()),
        confirmation_responses: Some(state.confirmation_responses.clone()),
        confirm_flags: crate::agent::loop_::ConfirmFlags { confirm_shell, confirm_file_write },
    };

    let ctx = crate::agent::tool::ToolContext {
        session_id: session_id.clone(),
        workspace_root: std::path::PathBuf::from(&workspace_root),
        bypass_permissions: false,
        settings: tool_settings,
        max_iterations: Some(max_iterations_override),
    };

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);
    let app_clone = app.clone();
    let session_id_clone = session_id.clone();
    let db_arc = state.db.clone();
    let cancel_flags_arc = state.cancel_flags.clone();
    let model_clone = model.clone();
    let max_tokens_clone = max_tokens;
    let provider_clone2 = provider.clone();
    let api_key_clone2 = api_key.clone();
    let base_url_clone2 = base_url.clone();

    tokio::spawn(async move {
        let app_fwd = app_clone.clone();
        let sid_fwd = session_id_clone.clone();
        let forward_handle = tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                let payload = serde_json::to_value(&event).unwrap_or_default();
                let _ = app_fwd.emit(&format!("agent_event_{}", sid_fwd), payload.clone());
                let _ = app_fwd.emit("agent_broadcast", payload);
            }
        });

        let result = agent.run(llm_messages, event_tx.clone(), cancel.clone(), ctx).await;

        match &result {
            Ok((final_messages, total_in, total_out)) => {
                let assistant_text = final_messages.iter().rev()
                    .find(|m| m.role == "assistant")
                    .map(|m| m.content.as_text())
                    .unwrap_or_default();
                {
                    let db = db_arc.lock().await;
                    if !assistant_text.is_empty() {
                        let _ = db.append_message(&session_id_clone, "assistant", &assistant_text);
                    }
                    let _ = db.update_session_status(&session_id_clone, "idle");
                }
                // Auto-extract memories in background
                {
                    let db_for_mem = db_arc.clone();
                    let sid_for_mem = session_id_clone.clone();
                    let msgs_for_mem = final_messages.clone();
                    let model_for_mem = model_clone.clone();
                    let mem_client = build_client(
                        &provider_clone2,
                        &api_key_clone2,
                        if base_url_clone2.is_empty() { None } else { Some(&base_url_clone2) },
                    );
                    tokio::spawn(async move {
                        auto_extract_memories(db_for_mem, sid_for_mem, msgs_for_mem, mem_client, model_for_mem, max_tokens_clone).await;
                    });
                }
                let _ = event_tx.send(AgentEvent::Done {
                    total_input_tokens: *total_in,
                    total_output_tokens: *total_out,
                }).await;
            }
            Err(e) => {
                let db = db_arc.lock().await;
                let _ = db.update_session_status(&session_id_clone, "idle");
                drop(db);
                let _ = event_tx.send(AgentEvent::Error { message: e.to_string() }).await;
            }
        }

        drop(event_tx);
        let _ = forward_handle.await;
        let mut flags = cancel_flags_arc.lock().await;
        flags.remove(&session_id_clone);
    });

    Ok(())
}

/// Cancel an in-progress agent run
#[tauri::command]
pub async fn chat_cancel(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
    let flags = state.cancel_flags.lock().await;
    if let Some(flag) = flags.get(&session_id) {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

pub fn build_system_prompt(memory_context: &str, skill_context: &str) -> String {
    format!(
        "You are Pisci, a powerful Windows AI Agent (part of OpenPisci). \
         You can control the entire Windows desktop environment.\
         \n\nAvailable capabilities:\
         \n- file_read / file_write: Read and write files in the workspace\
         \n- shell: Execute PowerShell commands\
         \n- powershell_query: Query system info as structured JSON (processes, services, registry, etc.)\
         \n- wmi: WMI/WQL queries for hardware, OS, and system information\
         \n- web_search: Search the web via DuckDuckGo\
         \n- browser: Control Chrome browser (navigate, click, type, screenshot, eval_js, etc.)\
         \n- uia: Windows UI Automation — control any desktop app (click, type, hotkeys, window management)\
         \n- screen_capture: Take screenshots (full screen, window, region) for Vision AI analysis\
         \n- com: Clipboard read/write, open files with default apps, special folder paths\
         \n- office: Automate Word, Excel, Outlook via COM\
         \n- memory_store: Save important information to long-term memory for future conversations\
         \n\nVision AI workflow: When UIA cannot find an element, use screen_capture to take a screenshot, \
         analyze it to find the element's coordinates, then use uia with x/y coordinates to click.\
         \n\nMemory guidelines:\
         \n- When you learn something important about the user (preferences, habits, goals, project details), \
         call memory_store to save it for future reference.\
         \n- Categories: 'preference' (user likes/dislikes), 'fact' (factual info), 'task' (completed tasks), \
         'person' (people info), 'project' (project details), 'general'.\
         \n\nGeneral guidelines:\
         \n- Be concise and action-oriented\
         \n- Use the most appropriate tool for each task\
         \n- For browser tasks, prefer browser tool over shell+curl\
         \n- If browser reports captcha/human verification, pause and ask user to complete it manually\
         \n- For system info, prefer powershell_query or wmi over raw shell commands\
         \n- Always confirm before destructive operations (delete files, send emails, etc.)\
         \n- Respect workspace boundaries for file operations\
         \n- Today's date: {date}{memory}{skills}",
        date = chrono::Utc::now().format("%Y-%m-%d"),
        memory = memory_context,
        skills = if skill_context.is_empty() { String::new() } else { format!("\n\n## Active Skills\n{}", skill_context) }
    )
}

/// Estimate token count for a string.
/// CJK characters ≈ 1 token each; ASCII ≈ 1 token per 4 chars.
pub fn estimate_tokens(text: &str) -> usize {
    let mut cjk_count = 0usize;
    let mut ascii_count = 0usize;
    for ch in text.chars() {
        let cp = ch as u32;
        if (0x4E00..=0x9FFF).contains(&cp)   // CJK Unified
            || (0x3400..=0x4DBF).contains(&cp) // CJK Extension A
            || (0xF900..=0xFAFF).contains(&cp) // CJK Compatibility
            || (0x3000..=0x303F).contains(&cp) // CJK Symbols
            || (0xFF00..=0xFFEF).contains(&cp) // Fullwidth
        {
            cjk_count += 1;
        } else {
            ascii_count += 1;
        }
    }
    cjk_count + (ascii_count / 4).max(1)
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
) {
    // Only extract if there's meaningful assistant content
    let assistant_chars: usize = messages.iter()
        .filter(|m| m.role == "assistant")
        .map(|m| m.content.as_text().chars().count())
        .sum();

    if assistant_chars < 100 {
        return;
    }

    // Build a compact conversation summary for the extraction prompt
    let conv_summary: String = messages.iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .take(10)
        .map(|m| {
            let text = m.content.as_text();
            let truncated: String = text.chars().take(300).collect();
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
    };

    match client.complete(req).await {
        Ok(resp) if !resp.content.is_empty() && resp.content.trim() != "NONE" => {
            let db = db_arc.lock().await;
            for line in resp.content.lines() {
                let line = line.trim();
                if line.is_empty() || line == "NONE" { continue; }

                let (category, content) = if line.starts_with('[') {
                    if let Some(end) = line.find(']') {
                        let cat = &line[1..end];
                        let cont = line[end+1..].trim();
                        (cat, cont)
                    } else {
                        ("general", line)
                    }
                } else {
                    ("general", line)
                };

                let valid_categories = ["preference", "fact", "task", "person", "project", "general"];
                let category = if valid_categories.contains(&category) { category } else { "general" };

                if !content.is_empty() {
                    let _ = db.save_memory(content, category, 0.75, Some(&session_id));
                    tracing::info!("Auto-extracted memory [{category}]: {content}");
                }
            }
        }
        Ok(_) => {} // NONE or empty — nothing to save
        Err(e) => tracing::warn!("Memory auto-extraction failed: {}", e),
    }
}
