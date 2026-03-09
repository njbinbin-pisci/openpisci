#![recursion_limit = "512"]

// lib.rs — Tauri application library entry point.
// main.rs calls run() from here; this allows Tauri mobile targets to work.

mod agent;
mod browser;
mod commands;
mod fish;
mod gateway;
mod koi;
mod llm;
mod memory;
mod policy;
mod scheduler;
mod security;
mod skills;
mod store;
mod tools;

use tauri::{Emitter, Manager};
use tracing::info;
use tracing_subscriber::prelude::*;

pub use store::AppState;

/// Extract a `SEND_IMAGE:<path>` or `SEND_FILE:<path>` marker from agent reply.
/// Scans all lines (not just the last), removes the marker line, and returns
/// (text_without_marker, Option<file_path>).
fn extract_send_marker(text: &str) -> (String, Option<String>) {
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate().rev() {
        let trimmed = line.trim();
        if let Some(path) = trimmed.strip_prefix("SEND_IMAGE:").or_else(|| trimmed.strip_prefix("SEND_FILE:")) {
            let path = path.trim().to_string();
            if !path.is_empty() {
                // Keep all lines except the marker line itself
                let clean_parts: Vec<&str> = lines.iter().enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, l)| *l)
                    .collect();
                let clean = clean_parts.join("\n").trim().to_string();
                tracing::info!("extract_send_marker: found marker at line {}, path={}", i, path);
                return (clean, Some(path));
            }
        }
    }
    (text.to_string(), None)
}

/// Guess MIME type from file path extension.
fn guess_mime_from_path(path: &str) -> String {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") { "image/png".to_string() }
    else if lower.ends_with(".gif") { "image/gif".to_string() }
    else if lower.ends_with(".webp") { "image/webp".to_string() }
    else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") { "image/jpeg".to_string() }
    else if lower.ends_with(".pdf") { "application/pdf".to_string() }
    else if lower.ends_with(".mp4") { "video/mp4".to_string() }
    else { "application/octet-stream".to_string() }
}

/// Returns the platform log directory: `<data_dir>/pisci/logs`.
/// Falls back to the current directory if the platform path is unavailable.
fn log_dir() -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(base) = dirs::data_local_dir() {
            return base.join("pisci").join("logs");
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("logs")
}

/// Initialise structured logging:
/// - STDERR: human-readable, filtered by RUST_LOG / default "info"
/// - Rolling file: JSON, one file per day, kept up to 7 days (via tracing-appender)
///
/// Returns the `_guard` that must stay alive for the lifetime of the process
/// to ensure the non-blocking writer flushes on drop.
fn init_logging() -> tracing_appender::non_blocking::WorkerGuard {
    let dir = log_dir();
    // Ensure the log directory exists (best-effort)
    let _ = std::fs::create_dir_all(&dir);

    let file_appender = tracing_appender::rolling::daily(&dir, "pisci.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "pisci_desktop_lib=debug,info".into());

    // Layer 1 — pretty stdout/stderr for developer visibility
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_writer(std::io::stderr);

    // Layer 2 — JSON file, one entry per log record
    let file_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(non_blocking)
        .with_current_span(true)
        .with_span_list(true);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();

    guard
}

/// Install a panic hook that writes a crash report to the log directory and
/// re-raises the default panic message so the OS crash dialog still appears.
fn install_crash_reporter() {
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);

    std::panic::set_hook(Box::new(move |info| {
        // Format crash report
        let timestamp = chrono::Utc::now().to_rfc3339();
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".into());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic payload".into());

        let report = serde_json::json!({
            "event": "panic",
            "timestamp": timestamp,
            "location": location,
            "message": payload,
        });

        // Write crash-<timestamp>.json
        let crash_file = dir.join(format!(
            "crash-{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%S")
        ));
        let _ = std::fs::write(&crash_file, report.to_string());

        // Also emit to tracing so it ends up in the rolling log
        tracing::error!(
            location = %location,
            message = %payload,
            "PANIC — crash report written to {}",
            crash_file.display()
        );

        // Print panic info to stderr so the OS crash dialog still appears
        eprintln!("PANIC at {location}: {payload}");
    }));
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _log_guard = init_logging();
    install_crash_reporter();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let app_handle = app.handle().clone();

            // Initialize scheduler (async) then build AppState synchronously
            let state = tauri::async_runtime::block_on(async {
                let scheduler = scheduler::cron::CronScheduler::new().await?;
                scheduler.start().await?;
                store::AppState::new_sync(&app_handle, scheduler)
            })?;

            // Re-register persisted active tasks into the live scheduler
            {
                let db = tauri::async_runtime::block_on(state.db.lock());
                let tasks = db.list_tasks().unwrap_or_default();
                drop(db);
                for task in tasks {
                    if task.status == "active" {
                        let app_h = app_handle.clone();
                        let task_id = task.id.clone();
                        let task_prompt = task.task_prompt.clone();
                        let db_arc = state.db.clone();
                        let settings_arc = state.settings.clone();
                        let browser = state.browser.clone();
                        let cancel_flags = state.cancel_flags.clone();
                        let cron = task.cron_expression.clone();
                        let sched = state.scheduler.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = sched.add_job(&cron, task_id.clone(), move |_uuid, _sched| {
                                let app_h = app_h.clone();
                                let task_id = task_id.clone();
                                let task_prompt = task_prompt.clone();
                                let db_arc = db_arc.clone();
                                let settings_arc = settings_arc.clone();
                                let browser = browser.clone();
                                let cancel_flags = cancel_flags.clone();
                                Box::pin(async move {
                                    commands::scheduler::execute_task(
                                        app_h, task_id, task_prompt,
                                        db_arc, settings_arc, browser, cancel_flags,
                                    ).await;
                                })
                            }).await;
                        });
                    }
                }
            }

            // Spawn IM gateway inbound consumer — routes IM messages to agent and replies
            {
                let gateway = state.gateway.clone();
                let db = state.db.clone();
                let settings = state.settings.clone();
                let plan_state = state.plan_state.clone();
                let browser = state.browser.clone();
                let cancel_flags = state.cancel_flags.clone();
                let confirm_resp = state.confirmation_responses.clone();
                let app_h = app_handle.clone();
                let sched = state.scheduler.clone();
                // Per-session mutex map: prevents two concurrent agent runs for the same IM session.
                // Messages for the same sender are queued and processed one at a time.
                let im_session_locks: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>> =
                    std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
                tauri::async_runtime::spawn(async move {
                    if let Some(mut rx) = gateway.take_receiver().await {
                        info!("Gateway inbound consumer started");
                        while let Some(msg) = rx.recv().await {
                            info!("Inbound IM message from {} via {}: {}", msg.sender, msg.channel, &msg.content[..msg.content.len().min(80)]);

                            if let Err(e) = crate::commands::window::enter_unattended_im_mode(&app_h, &store::AppState {
                                db: db.clone(),
                                settings: settings.clone(),
                                plan_state: plan_state.clone(),
                                browser: browser.clone(),
                                cancel_flags: cancel_flags.clone(),
                                confirmation_responses: confirm_resp.clone(),
                                app_handle: app_h.clone(),
                                scheduler: sched.clone(),
                                gateway: gateway.clone(),
                            }).await {
                                tracing::warn!("Failed to enter unattended IM mode: {}", e);
                            }

                            // Deterministic session ID: one persistent session per (channel, sender)
                            let session_id = format!("im_{}_{}", msg.channel, msg.sender);
                            let session_title = format!(
                                "{} · {}",
                                msg.channel,
                                msg.sender_name.as_deref().unwrap_or(&msg.sender)
                            );
                            let source = format!("im_{}", msg.channel);
                            {
                                let db_lock = db.lock().await;
                                let _ = db_lock.ensure_im_session(&session_id, &session_title, &source);
                                // Write the inbound user message to DB *before* emitting im_session_updated,
                                // so the frontend can immediately load it when it receives the event.
                                // run_agent_headless will skip re-inserting if it detects the message is already there,
                                // but we handle this by pre-inserting here and passing a flag.
                                let _ = db_lock.append_message(&session_id, "user", &msg.content);
                                let _ = db_lock.update_session_status(&session_id, "running");
                            }
                            match app_h.emit("im_session_updated", &session_id) {
                                Ok(()) => info!("Emitted im_session_updated for session={}", session_id),
                                Err(e) => tracing::warn!("Failed to emit im_session_updated: {}", e),
                            }

                            // Get or create per-session lock to serialize agent runs for the same session.
                            // This prevents a new message from spawning a second agent while the first is running.
                            let session_lock = {
                                let mut locks = im_session_locks.lock().await;
                                locks.entry(session_id.clone())
                                    .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
                                    .clone()
                            };

                            // If a previous agent is still running for this session, cancel it so it
                            // stops promptly and the new message can be processed without waiting.
                            {
                                let flags = cancel_flags.lock().await;
                                if let Some(flag) = flags.get(&session_id) {
                                    info!("Cancelling previous agent for session {} due to new inbound message", session_id);
                                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                                }
                            }

                            let state_ref = store::AppState {
                                db: db.clone(),
                                settings: settings.clone(),
                                plan_state: plan_state.clone(),
                                browser: browser.clone(),
                                cancel_flags: cancel_flags.clone(),
                                confirmation_responses: confirm_resp.clone(),
                                app_handle: app_h.clone(),
                                scheduler: sched.clone(),
                                gateway: gateway.clone(),
                            };

                            let gw = gateway.clone();
                            let inbound_media = msg.media.clone();
                            let msg_channel = msg.channel.clone();
                            tokio::spawn(async move {
                                // Acquire the per-session lock — waits for the previous agent to finish
                                // (it will finish quickly since we just set its cancel flag above).
                                let _session_guard = session_lock.lock().await;
                                info!("IM session lock acquired for {}", session_id);

                                let response = commands::chat::run_agent_headless(&state_ref, &session_id, &msg.content, inbound_media, &msg_channel).await;

                                // run_agent_headless emits im_session_done after persist on success.
                                // On error it returns Err without emitting, so we emit here as fallback.
                                if response.is_err() {
                                    info!("run_agent_headless returned error, emitting im_session_done for {}", session_id);
                                    let _ = state_ref.app_handle.emit("im_session_done", &session_id);
                                }

                                let (reply_text, reply_image, reply_image_mime) = match response {
                                    Ok((text, img, mime)) => {
                                        let t = if text.is_empty() && img.is_none() {
                                            "（Agent 未返回内容）".to_string()
                                        } else { text };
                                        (t, img, mime)
                                    }
                                    Err(e) => (format!("Agent error: {}", e), None, None),
                                };

                                // Parse SEND_IMAGE: or SEND_FILE: marker from agent reply
                                let (clean_text, file_path) = extract_send_marker(&reply_text);

                                // Build media: prefer explicit file marker, fall back to reply_image
                                let media = file_path
                                    .and_then(|p| {
                                        match std::fs::read(&p) {
                                            Ok(data) => {
                                                let mime = guess_mime_from_path(&p);
                                                let filename = std::path::Path::new(&p)
                                                    .file_name()
                                                    .map(|n| n.to_string_lossy().into_owned())
                                                    .unwrap_or_else(|| "file".to_string());
                                                info!("extract_send_marker: read {} bytes from '{}', mime={}", data.len(), p, mime);
                                                Some(gateway::MediaAttachment {
                                                    media_type: mime,
                                                    url: None,
                                                    data: Some(data),
                                                    filename: Some(filename),
                                                })
                                            }
                                            Err(e) => {
                                                tracing::warn!("extract_send_marker: failed to read file '{}': {}", p, e);
                                                None
                                            }
                                        }
                                    })
                                    .or_else(|| reply_image.map(|data| gateway::MediaAttachment {
                                        media_type: reply_image_mime.unwrap_or_else(|| "image/jpeg".to_string()),
                                        url: None,
                                        data: Some(data),
                                        filename: Some("image.jpg".to_string()),
                                    }));

                                let outbound = gateway::OutboundMessage {
                                    channel: msg.channel.clone(),
                                    recipient: msg.reply_target.clone(),
                                    content: clean_text,
                                    reply_to: Some(msg.id.clone()),
                                    media,
                                };
                                info!("Sending IM reply via channel={} recipient={} len={}", msg.channel, msg.reply_target, outbound.content.len());
                                match gw.send(&outbound).await {
                                    Ok(()) => info!("IM reply sent successfully via {}", msg.channel),
                                    Err(e) => tracing::warn!("Failed to send IM reply via {}: {}", msg.channel, e),
                                }
                            });
                        }
                    }
                });
            }

            // Spawn heartbeat runner if enabled
            {
                let settings_arc = state.settings.clone();
                let db_arc = state.db.clone();
                let plan_state_arc = state.plan_state.clone();
                let browser_arc = state.browser.clone();
                let cancel_flags_arc = state.cancel_flags.clone();
                let confirm_resp_arc = state.confirmation_responses.clone();
                let app_h = app_handle.clone();
                let sched_arc = state.scheduler.clone();
                let gateway_arc = state.gateway.clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        let (enabled, interval_mins, prompt) = {
                            let s = settings_arc.lock().await;
                            (s.heartbeat_enabled, s.heartbeat_interval_mins, s.heartbeat_prompt.clone())
                        };
                        if !enabled || interval_mins == 0 {
                            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                            continue;
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(interval_mins as u64 * 60)).await;
                        // Re-check after sleep in case settings changed
                        let still_enabled = {
                            let s = settings_arc.lock().await;
                            s.heartbeat_enabled
                        };
                        if !still_enabled { continue; }
                        info!("Heartbeat: triggering agent run");
                        let state_ref = store::AppState {
                            db: db_arc.clone(),
                            settings: settings_arc.clone(),
                            plan_state: plan_state_arc.clone(),
                            browser: browser_arc.clone(),
                            cancel_flags: cancel_flags_arc.clone(),
                            confirmation_responses: confirm_resp_arc.clone(),
                            app_handle: app_h.clone(),
                            scheduler: sched_arc.clone(),
                            gateway: gateway_arc.clone(),
                        };
                        let _ = commands::chat::run_agent_headless(&state_ref, "heartbeat", &prompt, None, "heartbeat").await;
                    }
                });
            }

            // ── Startup skill sync: scan disk → DB so installed skills survive restarts ──
            // This runs before app.manage() so the DB is consistent before any command runs.
            {
                let app_dir = app_handle.path().app_data_dir()
                    .unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
                let skills_dir = app_dir.join("skills");
                let db_arc = state.db.clone();
                tauri::async_runtime::block_on(async {
                    let mut loader = crate::skills::loader::SkillLoader::new(&skills_dir);
                    if let Err(e) = loader.load_all() {
                        tracing::warn!("Startup skill scan failed: {}", e);
                        return;
                    }
                    let db = db_arc.lock().await;
                    for skill in loader.list_skills() {
                        if skill.source == "builtin" { continue; }
                        // Skip skills that failed to parse (name defaults to "unnamed")
                        if skill.name.is_empty() || skill.name == "unnamed" { continue; }
                        let safe_name: String = skill.name.chars()
                            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                            .collect::<String>()
                            .to_lowercase();
                        if let Err(e) = db.upsert_skill(&safe_name, &skill.name, &skill.description, "📦") {
                            tracing::warn!("Startup skill sync: failed to upsert '{}': {}", skill.name, e);
                        } else {
                            tracing::info!("Startup skill sync: registered '{}'", skill.name);
                        }
                    }
                    // Clean up any stale "unnamed" DB entries left from previous bad parses
                    if let Ok(db_skills) = db.list_skills() {
                        for s in db_skills {
                            if s.name == "unnamed" || s.id == "unnamed" {
                                let _ = db.delete_skill(&s.id);
                                tracing::info!("Startup skill sync: removed stale 'unnamed' entry '{}'", s.id);
                            }
                        }
                    }
                });
            }

            app.manage(state);

            // Attach tray menu (Show / Quit) — tray is created from config with id "main"
            if let Some(tray) = app.tray_by_id("main") {
                use tauri::menu::{Menu, MenuItem};
                if let (Ok(show_i), Ok(quit_i)) = (
                    MenuItem::with_id(&app_handle, "tray_show", "显示主界面", true, None::<&str>),
                    MenuItem::with_id(&app_handle, "tray_quit", "退出 OpenPisci", true, None::<&str>),
                ) {
                    if let Ok(menu) = Menu::with_items(&app_handle, &[&show_i, &quit_i]) {
                        if let Err(e) = tray.set_menu(Some(menu)) {
                            tracing::warn!("Tray set_menu: {}", e);
                        }
                    }
                }
                let _ = tray.set_tooltip(Some("OpenPisci"));
            }

            info!("OpenPisci started");
            Ok(())
        })
        .on_menu_event(|app, event| {
            match event.id().as_ref() {
                "tray_show" => {
                    if let Some(main) = app.get_webview_window("main") {
                        let _ = main.show();
                        let _ = main.set_focus();
                    }
                    if let Some(overlay) = app.get_webview_window("overlay") {
                        let _ = overlay.hide();
                    }
                }
                "tray_quit" => {
                    app.exit(0);
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            // Settings
            commands::settings::get_settings,
            commands::settings::save_settings,
            commands::settings::is_configured,
            // Sessions
            commands::chat::create_session,
            commands::chat::list_sessions,
            commands::chat::delete_session,
            commands::chat::rename_session,
            commands::chat::get_messages,
            commands::chat::chat_send,
            commands::chat::chat_cancel,
            commands::chat::get_context_preview,
            // Memory
            commands::memory::list_memories,
            commands::memory::add_memory,
            commands::memory::delete_memory,
            commands::memory::clear_memories,
            // Skills
            commands::skills::list_skills,
            commands::skills::toggle_skill,
            commands::skills::scan_skill_catalog,
            commands::skills::install_skill,
            commands::skills::uninstall_skill,
            commands::skills::clawhub_search,
            commands::skills::clawhub_install,
            commands::skills::check_skill_compat,
            // Scheduler
            commands::scheduler::list_tasks,
            commands::scheduler::create_task,
            commands::scheduler::update_task,
            commands::scheduler::delete_task,
            commands::scheduler::run_task_now,
            commands::scheduler::trigger_task_by_event,
            // System
            commands::system::get_vm_status,
            commands::system::get_runtime_capabilities,
            commands::system::check_runtimes,
            commands::system::set_runtime_path,
            // Audit log
            commands::audit::get_audit_log,
            commands::audit::clear_audit_log,
            // Permission
            commands::permission::respond_permission,
            // Gateway / IM
            commands::gateway::list_gateway_channels,
            commands::gateway::diagnose_gateway_channels,
            commands::gateway::connect_gateway_channels,
            commands::gateway::disconnect_gateway_channels,
            // User Tools
            commands::user_tools::list_user_tools,
            commands::user_tools::install_user_tool,
            commands::user_tools::uninstall_user_tool,
            commands::user_tools::save_user_tool_config,
            commands::user_tools::get_user_tool_config,
            // Built-in Tools & Heartbeat
            commands::tools::list_builtin_tools,
            commands::tools::trigger_heartbeat,
            // Debug / E2E testing
            commands::debug::list_debug_scenarios,
            commands::debug::run_debug_scenario,
            commands::debug::run_all_debug_scenarios,
            commands::debug::run_uia_drag_test,
            commands::debug::get_debug_report,
            commands::debug::get_log_tail,
            // Fish (小鱼) sub-Agents
            commands::fish::get_fish_dir,
            commands::fish::list_fish,
            // Koi (锦鲤) persistent Agents
            commands::koi::list_kois,
            commands::koi::get_koi,
            commands::koi::create_koi,
            commands::koi::update_koi,
            commands::koi::delete_koi,
            commands::koi::get_koi_palette,
            // Chat Pool
            commands::pool::list_pool_sessions,
            commands::pool::create_pool_session,
            commands::pool::delete_pool_session,
            commands::pool::get_pool_messages,
            commands::pool::send_pool_message,
            // Board (Kanban)
            commands::board::list_koi_todos,
            commands::board::create_koi_todo,
            commands::board::update_koi_todo,
            commands::board::delete_koi_todo,
            // MCP servers
            commands::mcp::list_mcp_servers,
            commands::mcp::save_mcp_servers,
            commands::mcp::test_mcp_server,
            // Window / minimal mode
            commands::window::enter_minimal_mode,
            commands::window::exit_minimal_mode,
            commands::window::set_overlay_position,
            commands::window::save_overlay_position,
            commands::window::set_app_theme,
            commands::window::set_window_theme_border,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pisci Desktop");
}
