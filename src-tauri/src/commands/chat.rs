use crate::agent::loop_::{AgentLoop, ConfirmFlags};
use crate::agent::messages::AgentEvent;
use crate::agent::tool::ToolContext;
use crate::llm::{build_client, ContentBlock, LlmMessage, MessageContent};
use crate::policy::PolicyGate;
use crate::store::{db::ChatMessage, db::Session, AppState};
use crate::tools;
use serde::{Deserialize, Serialize};
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
    let lim = limit.unwrap_or(100);
    let off = offset.unwrap_or(0);
    if off == 0 {
        // Default: return the latest `limit` messages in chronological order.
        // This ensures the frontend always sees the newest messages regardless of how many
        // tool_calls/tool_results have accumulated in the session history.
        db.get_messages_latest(&session_id, lim)
            .map_err(|e| e.to_string())
    } else {
        // Pagination: caller wants older messages (load-more-history)
        db.get_messages(&session_id, lim, off)
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
) -> Result<(), String> {
    tracing::info!("chat_send called: session={} content_len={} has_attachment={}", session_id, content.len(), attachment.is_some());

    // Load settings
    let (provider, model, api_key, base_url, workspace_root, max_tokens, context_window, confirm_shell, confirm_file_write, policy_mode, tool_rate_limit_per_minute, tool_settings, max_iterations, builtin_tool_enabled, allow_outside_workspace, vision_enabled) = {
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
        )
    };

    tracing::info!("chat_send: provider={} model={} api_key_empty={}", provider, model, api_key.is_empty());

    if api_key.is_empty() {
        tracing::warn!("chat_send: API key not configured");
        return Err("API key not configured. Please open Settings to configure your API key.".into());
    }

    // Prompt injection detection on user input
    {
        let gate = PolicyGate::with_profile_and_flags(&workspace_root, &policy_mode, tool_rate_limit_per_minute, allow_outside_workspace);
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

    // Resolve attachment: convert FrontendAttachment → MediaAttachment
    // For non-vision models or non-image files, we append the path to the message text.
    // For vision models + image data, we pass through as MediaAttachment for inline injection.
    let vision_capable = vision_enabled || model_supports_vision(&provider, &model);
    let (effective_content, media_attachment): (String, Option<crate::gateway::MediaAttachment>) =
        if let Some(att) = attachment {
            if att.media_type.starts_with("image/") {
                if vision_capable {
                    // Vision model: pass raw bytes for inline base64 injection
                    let data = att.data.as_deref()
                        .and_then(|b64| base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64).ok());
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
                            "image/png" => "png", "image/gif" => "gif", "image/webp" => "webp", _ => "jpg",
                        };
                        let default_fname = format!("attachment.{}", ext);
                        let fname = att.filename.as_deref().unwrap_or(&default_fname);
                        let tmp = std::env::temp_dir().join(fname);
                        if let Ok(bytes) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64) {
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
    {
        let db = state.db.lock().await;
        db.append_message(&session_id, "user", &effective_content)
            .map_err(|e| e.to_string())?;
        db.update_session_status(&session_id, "running")
            .map_err(|e| e.to_string())?;
    }

    // Load message history and build context with layered compression.
    let budget = compute_context_budget(context_window, max_tokens);

    let mut llm_messages = {
        let db = state.db.lock().await;
        let history = db.get_messages_latest(&session_id, 2000)
            .map_err(|e| e.to_string())?;
        build_context_messages(&history, budget)
    };

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
    let app_data_dir = app.path().app_data_dir().ok();

    // Load skills: build lightweight directory for system prompt + shared loader for skill_search tool
    let (skill_context, skill_loader_arc) = {
        let app_dir = app.path().app_data_dir().unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
        let skills_dir = app_dir.join("skills");
        let mut loader = crate::skills::loader::SkillLoader::new(skills_dir);
        if let Err(e) = loader.load_all() {
            tracing::warn!("Failed to load skills: {}", e);
        }
        let enabled_names: Vec<String> = loader.list_skills().iter().map(|s| s.name.clone()).collect();
        let dir = loader.generate_skill_directory(&enabled_names);
        let arc = Arc::new(tokio::sync::Mutex::new(loader));
        (dir, arc)
    };

    let registry = Arc::new(tools::build_registry(
        state.browser.clone(),
        user_tools_dir.as_deref(),
        Some(state.db.clone()),
        Some(&builtin_tool_enabled),
        Some(app.clone()),
        Some(state.settings.clone()),
        app_data_dir,
        Some(skill_loader_arc),
    ));

    let policy = Arc::new(PolicyGate::with_profile_and_flags(&workspace_root, &policy_mode, tool_rate_limit_per_minute, allow_outside_workspace));

    // Inject relevant memories into the system prompt
    let memory_context = {
        let db = state.db.lock().await;
        let keywords: Vec<&str> = effective_content.split_whitespace().take(10).collect();
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

    // Inject task state into the system prompt if one exists for this session
    let task_state_context = {
        let db = state.db.lock().await;
        match db.load_task_state("session", &session_id) {
            Ok(Some(ts)) if ts.status == "active" && (!ts.goal.is_empty() || !ts.summary.is_empty()) => {
                let mut ctx = String::from("\n\n## Active Task State\n");
                if !ts.goal.is_empty() {
                    ctx.push_str(&format!("**Goal:** {}\n", ts.goal));
                }
                if !ts.summary.is_empty() {
                    ctx.push_str(&format!("**Progress:** {}\n", ts.summary));
                }
                if ts.state_json != "{}" && !ts.state_json.is_empty() {
                    ctx.push_str(&format!("**Details:** {}\n", ts.state_json));
                }
                ctx
            }
            _ => String::new(),
        }
    };

    let injection_budget = compute_injection_budget(context_window);
    let full_memory_context = budget_truncate(
        &format!("{}{}", memory_context, task_state_context),
        injection_budget,
    );

    let model_for_host = model.clone();
    let agent = AgentLoop {
        client,
        registry,
        policy,
        system_prompt: build_system_prompt(&full_memory_context, &skill_context),
        model: model.clone(),
        max_tokens,
        db: Some(state.db.clone()),
        app_handle: Some(state.app_handle.clone()),
        confirmation_responses: Some(state.confirmation_responses.clone()),
        confirm_flags: crate::agent::loop_::ConfirmFlags {
            confirm_shell,
            confirm_file_write,
        },
        vision_override: Some(vision_capable),
    };

    let tool_settings_for_fish = tool_settings.clone();
    let ctx = ToolContext {
        session_id: session_id.clone(),
        workspace_root: std::path::PathBuf::from(&workspace_root),
        bypass_permissions: false,
        settings: tool_settings,
        max_iterations: Some(max_iterations),
    };

    // Check if the request should be decomposed by HostAgent
    let sub_tasks = if crate::agent::host::HostAgent::should_decompose(&effective_content) {
        let host_client = build_client(
            &provider, &api_key,
            if base_url.is_empty() { None } else { Some(&base_url) },
        );
        let host_agent = crate::agent::host::HostAgent::new(host_client, model_for_host.clone(), max_tokens);
        match host_agent.decompose_task(&effective_content).await {
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
    let workspace_root_clone = workspace_root.clone();
    let policy_mode_clone = policy_mode.clone();
    let tool_rate_limit_clone = tool_rate_limit_per_minute;
    let allow_outside_ws_clone = allow_outside_workspace;
    let builtin_tool_enabled_clone = builtin_tool_enabled.clone();
    let state_browser = state.browser.clone();
    let max_tokens_val = max_tokens;

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
        let context_len = llm_messages.len();
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

            // Load app_data_dir once for Fish registry lookups
            let app_data_dir_for_fish = app_clone.path().app_data_dir().ok();

            for task in &tasks {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) { break; }

                // Try to route this sub-task to a Fish agent based on app_hint
                let fish_id_opt = task.app_hint.as_deref()
                    .and_then(crate::agent::host::HostAgent::route_to_fish);

                if let Some(fish_id) = fish_id_opt {
                    let registry = crate::fish::FishRegistry::load(app_data_dir_for_fish.as_deref());
                    if let Some(fish_def) = registry.get(fish_id) {
                        let fish_def = fish_def.clone();

                        let _ = event_tx.send(AgentEvent::TextDelta {
                            delta: format!("🐠 路由到「{}」: {}\n", fish_def.name, task.description),
                        }).await;

                        // Stateless Fish call: build a fresh AgentLoop with only the task
                        let fish_msgs = vec![LlmMessage {
                            role: "user".into(),
                            content: MessageContent::text(&task.description),
                        }];

                        let fish_cancel = Arc::new(AtomicBool::new(false));
                        let fish_client = build_client(
                            &provider_clone, &api_key_clone,
                            if base_url_clone.is_empty() { None } else { Some(&base_url_clone) },
                        );
                        let fish_user_tools_dir = app_clone.path().app_data_dir().map(|d| d.join("user-tools")).ok();
                        let fish_registry_tools = Arc::new(tools::build_registry(
                            state_browser.clone(),
                            fish_user_tools_dir.as_deref(),
                            Some(db_arc.clone()),
                            Some(&builtin_tool_enabled_clone),
                            None, None, None, None,
                        ));
                        let fish_policy = Arc::new(crate::policy::PolicyGate::with_profile_and_flags(
                            &workspace_root_clone, &policy_mode_clone,
                            tool_rate_limit_clone, allow_outside_ws_clone,
                        ));
                        let fish_agent = AgentLoop {
                            client: fish_client,
                            registry: fish_registry_tools,
                            policy: fish_policy,
                            system_prompt: fish_def.agent.system_prompt.clone(),
                            model: model_clone.clone(),
                            max_tokens: max_tokens_val,
                            db: None,
                            app_handle: Some(app_clone.clone()),
                            confirmation_responses: None,
                            confirm_flags: ConfirmFlags { confirm_shell: false, confirm_file_write: false },
                            vision_override: Some(vision_capable),
                        };
                        let fish_ctx = ToolContext {
                            session_id: format!("fish_ephemeral_{}", fish_id),
                            workspace_root: std::path::PathBuf::from(&workspace_root_clone),
                            bypass_permissions: false,
                            settings: tool_settings_for_fish.clone(),
                            max_iterations: Some(fish_def.agent.max_iterations),
                        };

                        let (fish_tx, mut fish_rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);
                        let fish_id_fwd = fish_id.to_string();
                        let fish_name_fwd = fish_def.name.clone();
                        let parent_sid = session_id_clone.clone();
                        let app_fwd2 = app_clone.clone();
                        let fish_fwd = tokio::spawn(async move {
                            let mut it: u32 = 0;
                            while let Some(ev) = fish_rx.recv().await {
                                let prog = match &ev {
                                    AgentEvent::TextSegmentStart { iteration } => {
                                        it = *iteration;
                                        Some(AgentEvent::FishProgress { fish_id: fish_id_fwd.clone(), fish_name: fish_name_fwd.clone(), iteration: *iteration, tool_name: None, status: "thinking".into() })
                                    }
                                    AgentEvent::ToolStart { name, .. } => Some(AgentEvent::FishProgress { fish_id: fish_id_fwd.clone(), fish_name: fish_name_fwd.clone(), iteration: it, tool_name: Some(name.clone()), status: "tool_call".into() }),
                                    AgentEvent::ToolEnd { name, .. } => Some(AgentEvent::FishProgress { fish_id: fish_id_fwd.clone(), fish_name: fish_name_fwd.clone(), iteration: it, tool_name: Some(name.clone()), status: "tool_done".into() }),
                                    AgentEvent::Done { .. } => Some(AgentEvent::FishProgress { fish_id: fish_id_fwd.clone(), fish_name: fish_name_fwd.clone(), iteration: it, tool_name: None, status: "done".into() }),
                                    _ => None,
                                };
                                if let Some(p) = prog {
                                    let payload = serde_json::to_value(&p).unwrap_or_default();
                                    let _ = app_fwd2.emit(&format!("agent_event_{}", parent_sid), payload);
                                }
                            }
                        });

                        let fish_result = fish_agent.run(fish_msgs, fish_tx, fish_cancel, fish_ctx).await;
                        let _ = fish_fwd.await;

                        match fish_result {
                            Ok((final_fish_msgs, _, _)) => {
                                let reply = final_fish_msgs.iter().rev()
                                    .find(|m| m.role == "assistant")
                                    .map(|m| m.content.as_text())
                                    .unwrap_or_default();
                                let fish_result_msg = LlmMessage {
                                    role: "assistant".into(),
                                    content: MessageContent::text(format!(
                                        "[{}完成子任务: {}]\n\n结果:\n{}",
                                        fish_def.name, task.description, reply
                                    )),
                                };
                                all_messages.push(fish_result_msg);
                                let _ = event_tx.send(AgentEvent::TextDelta {
                                    delta: format!("✓ 「{}」已完成\n", fish_def.name),
                                }).await;
                            }
                            Err(e) => {
                                tracing::warn!("Fish '{}' failed for sub-task: {}", fish_id, e);
                                let _ = event_tx.send(AgentEvent::TextDelta {
                                    delta: format!("⚠ 「{}」执行失败，由主 Agent 接管: {}\n", fish_def.name, e),
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
                                    Err(e2) => { last_err = Some(e2); break; }
                                }
                                let _ = fwd.await;
                            }
                        }
                        continue;
                    }
                }

                // No Fish match — execute with main Agent
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
                // Persist only the NEW messages produced by the agent (not the context we fed in).
                {
                    let db = db_arc.lock().await;
                    persist_agent_turn(&db, &session_id_clone, final_messages, context_len);
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
/// Returns true if the given provider+model supports vision (image input).
pub fn model_supports_vision(provider: &str, model: &str) -> bool {
    let m = model.to_lowercase();
    let p = provider.to_lowercase();
    // OpenAI vision models
    if p == "openai" || p.contains("openai") {
        return m.contains("gpt-4o") || m.contains("gpt-4-vision") || m.contains("gpt-4-turbo") || m.contains("o1");
    }
    // Anthropic Claude 3+
    if p == "anthropic" || p.contains("claude") || m.contains("claude") {
        return m.contains("claude-3") || m.contains("claude-opus") || m.contains("claude-sonnet") || m.contains("claude-haiku");
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
pub async fn run_agent_headless(
    state: &AppState,
    session_id: &str,
    user_message: &str,
    inbound_media: Option<crate::gateway::MediaAttachment>,
    channel: &str,
) -> Result<(String, Option<Vec<u8>>, Option<String>), String> {
    let (provider, model, api_key, base_url, workspace_root, max_tokens, context_window, policy_mode, tool_rate_limit_per_minute, tool_settings, max_iterations, builtin_tool_enabled, allow_outside_workspace, vision_setting) = {
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
        )
    };
    if api_key.is_empty() {
        return Err("API key not configured".into());
    }

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
                let filename = media.filename.as_deref()
                    .unwrap_or(&default_filename);
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
        // Check if this user message was already pre-inserted by lib.rs (to ensure it's visible
        // in the frontend before the agent starts). Skip duplicate insertion if so.
        let already_inserted = db.get_messages_latest(session_id, 1)
            .ok()
            .and_then(|msgs| msgs.into_iter().last())
            .map(|m| m.role == "user" && m.content == effective_user_message)
            .unwrap_or(false);
        if already_inserted {
            tracing::info!("run_agent_headless: user message already pre-inserted for {}, skipping", session_id);
        } else {
            tracing::info!("run_agent_headless: inserting user message for {}", session_id);
            let _ = db.append_message(session_id, "user", &effective_user_message);
        }
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
    let policy = Arc::new(PolicyGate::with_profile_and_flags(&workspace_root, &policy_mode, tool_rate_limit_per_minute, allow_outside_workspace));

    let agent = AgentLoop {
        client, registry, policy,
        system_prompt: build_im_system_prompt(channel, vision_capable),
        model, max_tokens,
        db: Some(state.db.clone()),
        app_handle: None,
        confirmation_responses: None,
        confirm_flags: crate::agent::loop_::ConfirmFlags {
            confirm_shell: false,
            confirm_file_write: false,
        },
        vision_override: Some(vision_capable),
    };
    let ctx = ToolContext {
        session_id: session_id.to_string(),
        workspace_root: std::path::PathBuf::from(&workspace_root),
        bypass_permissions: false,
        settings: tool_settings,
        max_iterations: Some(max_iterations),
    };
    // Load full conversation history for context.
    // After building LLM messages, sanitize any orphaned tool_use blocks (tool calls without
    // a matching tool_result) that can occur when a previous agent run was cancelled mid-turn.
    // Orphaned tool_use blocks cause API errors and confuse the LLM into re-executing old tasks.
    let mut llm_messages = {
        let db = state.db.lock().await;
        let history = db.get_messages_latest(session_id, 2000).map_err(|e| e.to_string())?;
        tracing::info!("run_agent_headless: loaded {} history messages for {}", history.len(), session_id);
        let msgs = build_context_messages(&history, compute_context_budget(context_window, max_tokens));
        let sanitized = sanitize_tool_use_result_pairing(msgs);
        tracing::info!("run_agent_headless: context has {} LLM messages after sanitize for {}", sanitized.len(), session_id);
        sanitized
    };

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

    let context_len = llm_messages.len();

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

    let (final_msgs, _, _) = agent.run(llm_messages, event_tx, cancel.clone(), ctx).await
        .map_err(|e| e.to_string())?;
    let _ = forward_handle.await;

    // Clean up cancel flag
    {
        let mut flags = state.cancel_flags.lock().await;
        flags.remove(session_id);
    }

    // Extract the last assistant message: text + optional image
    let (response_text, image_data, image_mime) = final_msgs.iter().rev()
        .find(|m| m.role == "assistant")
        .map(|m| {
            let text = m.content.as_text();
            let img: Option<(Vec<u8>, String)> = match &m.content {
                crate::llm::MessageContent::Blocks(blocks) => {
                    blocks.iter().find_map(|b| {
                        if let crate::llm::ContentBlock::Image { source } = b {
                            if source.source_type == "base64" {
                                use base64::Engine;
                                let bytes = base64::engine::general_purpose::STANDARD
                                    .decode(&source.data).ok();
                                bytes.map(|b| (b, source.media_type.clone()))
                            } else { None }
                        } else { None }
                    })
                }
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
        tracing::info!("run_agent_headless: persisting agent turn for {}", session_id);
        let db = state.db.lock().await;
        persist_agent_turn(&db, session_id, &final_msgs, context_len);
        let _ = db.update_session_status(session_id, "idle");
        tracing::info!("run_agent_headless: persist done, status=idle for {}", session_id);
    }

    // Emit Done event for tool-steps panel
    let done_payload = serde_json::to_value(&AgentEvent::Done {
        total_input_tokens: 0,
        total_output_tokens: 0,
    }).unwrap_or_default();
    let _ = state.app_handle.emit(&format!("agent_event_{}", session_id), done_payload.clone());
    let _ = state.app_handle.emit("agent_broadcast", done_payload);

    // NOW emit im_session_done — DB is already written, frontend reload will see new messages.
    tracing::info!("run_agent_headless: emitting im_session_done for {}", session_id);
    let _ = state.app_handle.emit("im_session_done", session_id);

    // im_session_done is emitted by _done_guard on drop (RAII above), covering both success and error paths.

    Ok((response_text, image_data, image_mime))
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
    format!(
        r#"You are Pisci, a powerful Windows AI Agent. You run on the user's local Windows machine and can control the entire desktop environment.
Today's date: {date}

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

## Windows System Exploration Pattern

When asked about software installed on this machine, ALWAYS follow this order:
1. List top-level dirs: `file_list(path="C:\\", recursive=false)` or `file_list(path="C:\\Program Files")`
2. Search for files: `file_search(glob, "**/*.exe", path="C:\\Tribon")`
3. Search registry for COM: `shell cmd` → `reg query HKLM\SOFTWARE\Classes /f "AppName" /s`
4. Check WOW6432Node for 32-bit software: `powershell_query(get_registry, arch=x86, path=HKLM:\SOFTWARE\WOW6432Node\...)`
5. Try instantiating COM objects: `com_invoke(create, prog_id=..., arch=x86)`

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

## Key Rules

- **Working directory**: shell tool defaults to `C:\` — use absolute paths always
- **32-bit software**: Most legacy industrial/CAD/engineering software (Tribon, AutoCAD, etc.) is 32-bit. Their COM objects are in WOW6432Node. Always use `arch: "x86"` for these.
- **Non-zero exit codes**: Read the stdout/stderr output — a non-zero exit code does NOT always mean failure
- **File not found**: Before giving up, try: (1) `file_list` the parent directory, (2) `file_search(glob)` for the filename, (3) check if software is installed
- **Permission denied / Access Denied**: Use `shell` with `elevated: true` — Windows UAC dialog, user approves, command runs as Administrator
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
                 When you receive a task, **first call `skill_search`** to find relevant skills. \
                 If a matching skill is found, follow its instructions. \
                 If no skill matches, proceed with your built-in capabilities.\n\n\
                 Installed skills (use skill_search to load full instructions):\n{}",
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

// ---------------------------------------------------------------------------
// Context management helpers
// ---------------------------------------------------------------------------

/// Compute the token budget for `build_context_messages` from settings.
///
/// `context_window` is the user-configured input context limit (0 = auto).
/// `max_tokens` is the max *output* tokens (used only for auto-fallback).
///
/// Budget = (context_window * 0.85) - system_prompt_overhead
/// The 0.85 factor leaves headroom for the system prompt and the new user message.
/// If `context_window` is 0, we fall back to a conservative estimate based on `max_tokens`.
pub fn compute_context_budget(context_window: u32, max_tokens: u32) -> usize {
    const SYSTEM_OVERHEAD: usize = 2_000;

    let window = if context_window > 0 {
        context_window as usize
    } else {
        // Auto: derive a conservative estimate from max_tokens (legacy behaviour).
        // max_tokens is the OUTPUT limit, not the context window, so this is just a
        // rough fallback for users who haven't set context_window yet.
        match max_tokens {
            t if t >= 8192 => 100_000,
            t if t >= 4096 => 60_000,
            _ => 30_000,
        }
    };

    ((window as f64 * 0.85) as usize).saturating_sub(SYSTEM_OVERHEAD)
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
/// `context_len` is the number of messages that were passed INTO agent.run() as context
/// (already stored in the DB). Only messages at index >= context_len are new and need saving.
pub fn persist_agent_turn(
    db: &crate::store::Database,
    session_id: &str,
    final_messages: &[LlmMessage],
    context_len: usize,
) {
    let final_messages = &final_messages[context_len.min(final_messages.len())..];
    // Determine the turn index from the current message count.
    // We use the count BEFORE writing so all new rows share the same index.
    let turn_index = db.get_messages_latest(session_id, 2000)
        .map(|msgs| {
            // Count distinct turn_index values already stored + 1
            let max_turn = msgs.iter()
                .filter_map(|m| m.turn_index)
                .max()
                .unwrap_or(0);
            max_turn + 1
        })
        .unwrap_or(1);

    // Walk through the messages produced by the agent loop.
    // The first N-1 messages are intermediate (tool calls + results).
    // The last assistant message is the final answer.
    let mut i = 0;
    while i < final_messages.len() {
        let msg = &final_messages[i];

        match &msg.content {
            MessageContent::Blocks(blocks) => {
                let tool_uses: Vec<&ContentBlock> = blocks.iter()
                    .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                    .collect();
                let tool_results: Vec<&ContentBlock> = blocks.iter()
                    .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
                    .collect();

                if !tool_uses.is_empty() {
                    // Assistant message with tool calls
                    let text = msg.content.as_text();
                    let calls_json = serde_json::to_string(&tool_uses).unwrap_or_default();
                    let _ = db.append_message_full(
                        session_id,
                        "assistant",
                        &text,
                        Some(&calls_json),
                        None,
                        Some(turn_index),
                    );
                } else if !tool_results.is_empty() {
                    // User message carrying tool results
                    let results_json = serde_json::to_string(&tool_results).unwrap_or_default();
                    let _ = db.append_message_full(
                        session_id,
                        "user",
                        "",
                        None,
                        Some(&results_json),
                        Some(turn_index),
                    );
                } else {
                    // Blocks with only text (or image) — treat as plain assistant message
                    let text = msg.content.as_text();
                    if !text.is_empty() {
                        let _ = db.append_message_full(
                            session_id,
                            &msg.role,
                            &text,
                            None,
                            None,
                            Some(turn_index),
                        );
                    }
                }
            }
            MessageContent::Text(text) => {
                if !text.is_empty() {
                    let logged = if text.len() > 10_000 {
                        tracing::info!(
                            "Saving large assistant message ({} chars) for session={}",
                            text.len(), session_id
                        );
                        text.as_str()
                    } else {
                        text.as_str()
                    };
                    let _ = db.append_message_full(
                        session_id,
                        &msg.role,
                        logged,
                        None,
                        None,
                        Some(turn_index),
                    );
                }
            }
        }
        i += 1;
    }

    tracing::info!(
        "Persisted agent turn {} for session={} ({} messages)",
        turn_index, session_id, final_messages.len()
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
    let tail_str: String = content.chars().rev().take(tail).collect::<String>()
        .chars().rev().collect();
    let trimmed_chars = total.saturating_sub(head + tail);
    format!("{}\n...[trimmed: {} chars]...\n{}", head_str, trimmed_chars, tail_str)
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
                    let name = call.get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool");
                    let input = call.get("input").cloned().unwrap_or(serde_json::Value::Null);
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
                    let content = result.get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let is_error = result.get("is_error")
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

    let final_answer = turn.agent_msgs.iter().rev()
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
        let deduped: Vec<_> = key_artifacts.iter().collect::<std::collections::HashSet<_>>()
            .into_iter().collect();
        format!(" [artifacts: {}]", deduped.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "))
    };

    let user_summary = format!(
        "[历史第{}轮] {}",
        turn.index,
        turn.user_msg.content.chars().take(150).collect::<String>(),
    );
    let assistant_summary = format!(
        "[历史第{}轮回复]{}{} {}",
        turn.index,
        tools_part,
        artifacts_part,
        answer_snippet,
    );

    (user_summary, assistant_summary)
}

/// Extract key identifiers (file paths, URLs, queries) from tool input for summary.
fn extract_key_artifact(tool_name: &str, input: &serde_json::Value) -> Option<String> {
    match tool_name {
        "file_read" | "file_write" | "file_edit" => {
            input["path"].as_str().map(|p| {
                let short: String = p.chars().rev().take(60).collect::<String>().chars().rev().collect();
                if short.len() < p.len() { format!("...{}", short) } else { short }
            })
        }
        "shell" | "powershell_query" => {
            input["command"].as_str()
                .or_else(|| input["query"].as_str())
                .map(|c| {
                    let s: String = c.chars().take(50).collect();
                    format!("cmd:{}", s)
                })
        }
        "web_search" => input["query"].as_str().map(|q| format!("search:{}", q.chars().take(40).collect::<String>())),
        "browser" => input["url"].as_str().map(|u| u.chars().take(60).collect()),
        _ => None,
    }
}

/// Build LLM context messages from stored history using layered compression.
///
/// Strategy (from newest to oldest):
/// - Last `CTX_FULL_TURNS` turns: full ContentBlock reconstruction (tool calls + results)
/// - Middle turns (up to `CTX_COMPACT_AFTER`): tool results trimmed to head+tail
/// - Older turns: entire turn collapsed to a single summary message
/// - Token budget exceeded: stop adding older turns
pub fn build_context_messages(history: &[ChatMessage], budget: usize) -> Vec<LlmMessage> {
    if history.is_empty() {
        return Vec::new();
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
    // Collect each turn's messages as a separate group so we can prepend older turns
    // without reversing the internal message order within each turn.
    let mut turn_groups: Vec<Vec<LlmMessage>> = Vec::new();
    let mut token_est: usize = 0;

    // Process turns from newest to oldest; we prepend each group later.
    for (rev_idx, turn) in turns.iter().rev().enumerate() {
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
            if token_est + turn_tokens > budget && !turn_groups.is_empty() { break; }
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
                    let text_for_tokens = trimmed_blocks.iter()
                        .filter_map(|b| if let ContentBlock::ToolResult { content, .. } = b { Some(content.as_str()) } else { None })
                        .collect::<Vec<_>>().join(" ");
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
            if token_est + turn_tokens > budget && !turn_groups.is_empty() { break; }
            turn_groups.push(turn_msgs);
            token_est += turn_tokens;
        } else {
            // ── Compact: entire turn → user + assistant summary pair ───────
            let (user_summary, assistant_summary) = summarize_turn(turn);
            let t = estimate_tokens(&user_summary) + estimate_tokens(&assistant_summary);
            if token_est + t > budget && !turn_groups.is_empty() { break; }
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

    // Post-process: remove trailing orphaned tool_call messages (interrupted mid-turn).
    result = sanitize_tool_call_pairs(result);

    // Strip orphaned ToolUse blocks inside assistant messages that lack a matching
    // tool_result in the next message. Previously only applied in the headless path.
    result = sanitize_tool_use_result_pairing(result);

    tracing::debug!(
        "build_context_messages: {} turns → {} LlmMessages, ~{} tokens (budget={})",
        total_turns, result.len(), token_est, budget
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
            blocks.iter().filter_map(|b| {
                if let ContentBlock::ToolUse { id, .. } = b { Some(id.clone()) } else { None }
            }).collect()
        } else {
            vec![]
        };

        if tool_use_ids.is_empty() {
            // Not a tool_call message. Check if it's a tool_result (orphaned result after we
            // already removed its tool_call). If so, keep removing. Otherwise stop.
            let is_tool_result = if let MessageContent::Blocks(blocks) = &m.content {
                blocks.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. }))
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
                let has_result = blocks.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. }));
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
            n - drop_from, drop_from, n
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
        blocks.push(ContentBlock::Text { text: msg.content.clone() });
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
    blocks.into_iter().map(|b| {
        if let ContentBlock::ToolResult { tool_use_id, content, is_error } = b {
            ContentBlock::ToolResult {
                tool_use_id,
                content: trim_tool_result(&content, CTX_TRIM_HEAD, CTX_TRIM_TAIL),
                is_error,
            }
        } else {
            b
        }
    }).collect()
}

/// Extract a representative text string from blocks for token estimation.
fn blocks_to_token_text(blocks: &[ContentBlock]) -> String {
    blocks.iter().map(|b| match b {
        ContentBlock::Text { text } => text.as_str(),
        ContentBlock::ToolResult { content, .. } => content.as_str(),
        ContentBlock::ToolUse { name, .. } => name.as_str(),
        ContentBlock::Image { .. } => "[image]",
    }).collect::<Vec<_>>().join(" ")
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
                crate::llm::MessageContent::Blocks(blocks) => {
                    blocks.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }))
                }
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
        let next_is_tool_result = msgs.get(i + 1).map(|next| {
            next.role == "user" && match &next.content {
                crate::llm::MessageContent::Blocks(blocks) => {
                    blocks.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. }))
                }
                _ => false,
            }
        }).unwrap_or(false);

        if !next_is_tool_result {
            // Strip ToolUse blocks from this assistant message
            tracing::warn!("sanitize_tool_use_result_pairing: stripping orphaned ToolUse at index {}", i);
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
) {
    // Only extract if there's meaningful assistant content
    let assistant_chars: usize = messages.iter()
        .filter(|m| m.role == "assistant")
        .map(|m| m.content.as_text().chars().count())
        .sum();

    if assistant_chars < 100 {
        return;
    }

    // Build a compact conversation summary for the extraction prompt.
    // Take the LAST messages (most recent) rather than the first, since recent
    // context is far more likely to contain extractable memories.
    let relevant_msgs: Vec<_> = messages.iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .collect();
    let start = relevant_msgs.len().saturating_sub(12);
    let conv_summary: String = relevant_msgs[start..].iter()
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
    let (model, max_tokens, context_window) = {
        let settings = state.settings.lock().await;
        (
            settings.model.clone(),
            settings.max_tokens,
            settings.context_window,
        )
    };

    // Build context messages from history — this is the exact payload sent to the LLM
    let budget = compute_context_budget(context_window, max_tokens);
    let llm_messages = {
        let db = state.db.lock().await;
        let history = db.get_messages_latest(&session_id, 2000)
            .map_err(|e| e.to_string())?;
        build_context_messages(&history, budget)
    };

    // Convert LlmMessages to preview-friendly structs with structured blocks
    let messages: Vec<ContextPreviewMessage> = llm_messages.iter().map(|m| {
        let blocks: Vec<ContextPreviewBlock> = match &m.content {
            crate::llm::MessageContent::Text(t) => {
                if t.is_empty() {
                    vec![]
                } else {
                    vec![ContextPreviewBlock::Text { text: t.clone() }]
                }
            }
            crate::llm::MessageContent::Blocks(raw_blocks) => {
                raw_blocks.iter().map(|b| match b {
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
                    crate::llm::ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                        const PREVIEW_LIMIT: usize = 4000;
                        let truncated = content.len() > PREVIEW_LIMIT;
                        let display = if truncated {
                            let head: String = content.chars().take(PREVIEW_LIMIT * 3 / 4).collect();
                            let tail_start = content.char_indices()
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
                    crate::llm::ContentBlock::Image { .. } => {
                        ContextPreviewBlock::Image { note: "[image attachment]".to_string() }
                    }
                }).collect()
            }
        };
        // Estimate tokens from text representation
        let token_text: String = blocks.iter().map(|b| match b {
            ContextPreviewBlock::Text { text } => text.clone(),
            ContextPreviewBlock::ToolUse { name, input, .. } => format!("{} {}", name, input),
            ContextPreviewBlock::ToolResult { content, .. } => content.clone(),
            ContextPreviewBlock::Image { .. } => String::new(),
        }).collect::<Vec<_>>().join(" ");
        let tokens = estimate_tokens(&token_text);
        ContextPreviewMessage { role: m.role.clone(), blocks, tokens }
    }).collect();

    let messages_tokens: usize = messages.iter().map(|m| m.tokens).sum();

    Ok(ContextPreview {
        messages,
        messages_tokens,
        total_tokens: messages_tokens,
        model,
        context_budget: budget,
    })
}
