// lib.rs — Tauri application library entry point.
// main.rs calls run() from here; this allows Tauri mobile targets to work.

mod agent;
mod browser;
mod commands;
mod fish;
mod gateway;
mod llm;
mod memory;
mod policy;
mod scheduler;
mod security;
mod skills;
mod store;
mod tools;

use tauri::Manager;
use tracing::info;
use tracing_subscriber::prelude::*;

pub use store::AppState;

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
                let browser = state.browser.clone();
                let cancel_flags = state.cancel_flags.clone();
                let confirm_resp = state.confirmation_responses.clone();
                let app_h = app_handle.clone();
                let sched = state.scheduler.clone();
                tauri::async_runtime::spawn(async move {
                    if let Some(mut rx) = gateway.take_receiver().await {
                        info!("Gateway inbound consumer started");
                        while let Some(msg) = rx.recv().await {
                            info!("Inbound IM message from {} via {}: {}", msg.sender, msg.channel, &msg.content[..msg.content.len().min(80)]);
                            let channel_name = msg.channel.clone();
                            let reply_target = msg.reply_target.clone();

                            // Deterministic session ID: one persistent session per (channel, sender)
                            let session_id = format!("im_{}_{}", channel_name, msg.sender);
                            let session_title = format!(
                                "{} · {}",
                                channel_name,
                                msg.sender_name.as_deref().unwrap_or(&msg.sender)
                            );
                            let source = format!("im_{}", channel_name);
                            {
                                let db_lock = db.lock().await;
                                // Idempotent: creates on first message, reuses on subsequent ones
                                let _ = db_lock.ensure_im_session(&session_id, &session_title, &source);
                            }

                            let state_ref = store::AppState {
                                db: db.clone(),
                                settings: settings.clone(),
                                browser: browser.clone(),
                                cancel_flags: cancel_flags.clone(),
                                confirmation_responses: confirm_resp.clone(),
                                app_handle: app_h.clone(),
                                scheduler: sched.clone(),
                                gateway: gateway.clone(),
                            };

                            let response = commands::chat::run_agent_headless(&state_ref, &session_id, &msg.content).await;
                            let reply_text = match response {
                                Ok(text) if !text.is_empty() => text,
                                Ok(_) => "（Agent 未返回内容）".to_string(),
                                Err(e) => format!("Agent error: {}", e),
                            };

                            let outbound = gateway::OutboundMessage {
                                channel: channel_name.clone(),
                                recipient: reply_target,
                                content: reply_text,
                                reply_to: Some(msg.id.clone()),
                                media: None,
                            };
                            if let Err(e) = gateway.send(&outbound).await {
                                tracing::warn!("Failed to send IM reply via {}: {}", channel_name, e);
                            }
                        }
                    }
                });
            }

            // Spawn heartbeat runner if enabled
            {
                let settings_arc = state.settings.clone();
                let db_arc = state.db.clone();
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
                            browser: browser_arc.clone(),
                            cancel_flags: cancel_flags_arc.clone(),
                            confirmation_responses: confirm_resp_arc.clone(),
                            app_handle: app_h.clone(),
                            scheduler: sched_arc.clone(),
                            gateway: gateway_arc.clone(),
                        };
                        let _ = commands::chat::run_agent_headless(&state_ref, "heartbeat", &prompt).await;
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
            // Fish (小鱼) sub-Agents
            commands::fish::list_fish,
            commands::fish::activate_fish,
            commands::fish::deactivate_fish,
            commands::fish::get_fish_status,
            commands::fish::fish_chat_send,
            // Window / minimal mode
            commands::window::enter_minimal_mode,
            commands::window::exit_minimal_mode,
            commands::window::set_overlay_position,
            commands::window::set_window_theme_border,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pisci Desktop");
}
