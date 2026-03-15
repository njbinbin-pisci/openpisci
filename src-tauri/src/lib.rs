#![recursion_limit = "512"]

// lib.rs — Tauri application library entry point.
// main.rs calls run() from here; this allows Tauri mobile targets to work.

pub mod agent;
mod browser;
mod commands;
mod fish;
mod gateway;
pub mod koi;
mod llm;
mod memory;
mod pisci;
mod policy;
mod scheduler;
mod security;
mod skills;
pub mod store;
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
        if let Some(path) = trimmed
            .strip_prefix("SEND_IMAGE:")
            .or_else(|| trimmed.strip_prefix("SEND_FILE:"))
        {
            let path = path.trim().to_string();
            if !path.is_empty() {
                // Keep all lines except the marker line itself
                let clean_parts: Vec<&str> = lines
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, l)| *l)
                    .collect();
                let clean = clean_parts.join("\n").trim().to_string();
                tracing::info!(
                    "extract_send_marker: found marker at line {}, path={}",
                    i,
                    path
                );
                return (clean, Some(path));
            }
        }
    }
    (text.to_string(), None)
}

/// Guess MIME type from file path extension.
fn guess_mime_from_path(path: &str) -> String {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        "image/png".to_string()
    } else if lower.ends_with(".gif") {
        "image/gif".to_string()
    } else if lower.ends_with(".webp") {
        "image/webp".to_string()
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg".to_string()
    } else if lower.ends_with(".pdf") {
        "application/pdf".to_string()
    } else if lower.ends_with(".mp4") {
        "video/mp4".to_string()
    } else {
        "application/octet-stream".to_string()
    }
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

/// Open a local file or directory with the system default application.
/// On Windows, directories are opened with `explorer.exe` to guarantee
/// Explorer opens (ShellExecute "open" verb is unreliable for directories).
/// Files are opened with the `start` command (equivalent to double-clicking).
#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let p = std::path::Path::new(&path);
        if p.is_dir() {
            std::process::Command::new("explorer")
                .arg(&path)
                .spawn()
                .map_err(|e| format!("Failed to open directory in Explorer: {e}"))?;
        } else {
            // Use cmd /c start "" "<path>" to open files with default app
            std::process::Command::new("cmd")
                .args(["/c", "start", "", &path])
                .spawn()
                .map_err(|e| format!("Failed to open file: {e}"))?;
        }
        return Ok(());
    }
    #[cfg(not(target_os = "windows"))]
    {
        let cmd = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(cmd)
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open path: {e}"))?;
        Ok(())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _log_guard = init_logging();
    install_crash_reporter();

    // Read allow_multiple_instances before building the Tauri app so we can
    // conditionally register the single-instance plugin.
    let allow_multiple = {
        let config_path = store::settings::Settings::default_config_path();
        store::settings::Settings::load(&config_path)
            .map(|s| s.allow_multiple_instances)
            .unwrap_or(false)
    };

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init());

    // Register single-instance guard unless the user explicitly allows multiple windows.
    if !allow_multiple {
        builder = builder.plugin(
            tauri_plugin_single_instance::Builder::new()
                .callback(|app, _args, _cwd| {
                    // A second instance was launched — focus the existing main window.
                    if let Some(win) = app.get_webview_window("main") {
                        let _ = win.show();
                        let _ = win.unminimize();
                        let _ = win.set_focus();
                    }
                })
                .build(),
        );
    }

    builder
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
                let interactive_resp = state.interactive_responses.clone();
                let app_h = app_handle.clone();
                let sched = state.scheduler.clone();
                let pisci_heartbeat_cursor = state.pisci_heartbeat_cursor.clone();
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
                                interactive_responses: interactive_resp.clone(),
                                app_handle: app_h.clone(),
                                scheduler: sched.clone(),
                                gateway: gateway.clone(),
                                pisci_heartbeat_cursor: pisci_heartbeat_cursor.clone(),
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
                                interactive_responses: interactive_resp.clone(),
                                app_handle: app_h.clone(),
                                scheduler: sched.clone(),
                                gateway: gateway.clone(),
                                pisci_heartbeat_cursor: pisci_heartbeat_cursor.clone(),
                            };

                            let gw = gateway.clone();
                            let inbound_media = msg.media.clone();
                            let msg_channel = msg.channel.clone();
                            tokio::spawn(async move {
                                // Acquire the per-session lock — waits for the previous agent to finish
                                // (it will finish quickly since we just set its cancel flag above).
                                let _session_guard = session_lock.lock().await;
                                info!("IM session lock acquired for {}", session_id);

                                let response = commands::chat::run_agent_headless(
                                    &state_ref,
                                    &session_id,
                                    &msg.content,
                                    inbound_media,
                                    &msg_channel,
                                    None,
                                ).await;

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
                let interactive_resp_arc = state.interactive_responses.clone();
                let app_h = app_handle.clone();
                let sched_arc = state.scheduler.clone();
                let gateway_arc = state.gateway.clone();
                let pisci_heartbeat_cursor_arc = state.pisci_heartbeat_cursor.clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        let (enabled, interval_mins, prompt) = {
                            let s = settings_arc.lock().await;
                            let raw_prompt = s.heartbeat_prompt.clone();
                            let prompt = if raw_prompt.trim().is_empty() {
                                crate::store::settings::default_heartbeat_prompt()
                            } else {
                                raw_prompt
                            };
                            (s.heartbeat_enabled, s.heartbeat_interval_mins, prompt)
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
                            interactive_responses: interactive_resp_arc.clone(),
                            app_handle: app_h.clone(),
                            scheduler: sched_arc.clone(),
                            gateway: gateway_arc.clone(),
                            pisci_heartbeat_cursor: pisci_heartbeat_cursor_arc.clone(),
                        };
                        let _ = crate::pisci::heartbeat::dispatch_heartbeat(&state_ref, &prompt, "heartbeat").await;
                    }
                });
            }

            // Spawn Koi patrol loop — periodic watchdog recovery + pending todo activation
            {
                let db_arc = state.db.clone();
                let app_h = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    // On startup, immediately recover stale state and re-activate pending todos
                    // so crashed/restarted Koi resume quickly instead of waiting for the first patrol interval.
                    {
                        let runtime = crate::koi::runtime::KoiRuntime::from_tauri(
                            app_h.clone(), db_arc.clone(),
                        );
                        let (stale_koi, stale_todo) = runtime.watchdog_recover(0).await;
                        let stale_sessions = {
                            let db = db_arc.lock().await;
                            db.recover_stale_running_sessions(0).unwrap_or(0)
                        };
                        if stale_koi > 0 || stale_todo > 0 || stale_sessions > 0 {
                            tracing::info!(
                                "Koi patrol startup: recovered {} stale Koi, {} stale todos, {} stale sessions",
                                stale_koi, stale_todo, stale_sessions
                            );
                        }
                        match runtime.activate_pending_todos(None).await {
                            Ok(activated) if activated > 0 => {
                                tracing::info!("Koi patrol startup: activated {} pending todos", activated);
                            }
                            Err(e) => {
                                tracing::warn!("Koi patrol startup: pending todo activation error: {}", e);
                            }
                            _ => {}
                        }
                    }

                    // Wait before the next periodic patrol pass
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    loop {
                        let runtime = crate::koi::runtime::KoiRuntime::from_tauri(
                            app_h.clone(), db_arc.clone(),
                        );

                        // 1. Watchdog: recover stale busy Koi and in_progress todos (threshold: 10 min)
                        let (stale_koi, stale_todo) = runtime.watchdog_recover(600).await;
                        let stale_sessions = {
                            let db = db_arc.lock().await;
                            db.recover_stale_running_sessions(600).unwrap_or(0)
                        };
                        if stale_koi > 0 || stale_todo > 0 || stale_sessions > 0 {
                            tracing::info!(
                                "Koi patrol: recovered {} stale Koi, {} stale todos, {} stale sessions",
                                stale_koi, stale_todo, stale_sessions
                            );
                        }

                        // 2. Activate pending (unclaimed) todos for idle Koi
                        match runtime.activate_pending_todos(None).await {
                            Ok(activated) if activated > 0 => {
                                tracing::info!("Koi patrol: activated {} pending todos", activated);
                            }
                            Err(e) => {
                                tracing::warn!("Koi patrol: pending todo activation error: {}", e);
                            }
                            _ => {}
                        }

                        // Patrol every 2 minutes
                        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
                    }
                });
            }

            // ── Startup skill cleanup: remove stale DB placeholders only ──
            // Database is the source of truth for installed/enabled skills. We do not
            // auto-import arbitrary skill folders from disk on startup.
            {
                let db_arc = state.db.clone();
                tauri::async_runtime::block_on(async {
                    let db = db_arc.lock().await;
                    // Clean up any stale "unnamed" DB entries left from previous bad parses
                    if let Ok(db_skills) = db.list_skills() {
                        for s in db_skills {
                            if s.name == "unnamed" || s.id == "unnamed" {
                                let _ = db.delete_skill(&s.id);
                                tracing::info!("Startup skill cleanup: removed stale 'unnamed' entry '{}'", s.id);
                            }
                        }
                    }
                });
            }

            // ── Startup Koi dedup: remove duplicate Koi created by repeated trial runs ──
            {
                let db_arc = state.db.clone();
                tauri::async_runtime::block_on(async {
                    let db = db_arc.lock().await;
                    match db.dedup_kois() {
                        Ok(0) => {}
                        Ok(n) => info!("Startup dedup: removed {} duplicate Koi entries", n),
                        Err(e) => tracing::warn!("Startup dedup failed: {}", e),
                    }
                });
            }

            // ── First-run starter Koi seed ──────────────────────────────────────
            {
                let db_arc = state.db.clone();
                let settings_arc = state.settings.clone();
                tauri::async_runtime::block_on(async {
                    let should_seed = {
                        let settings = settings_arc.lock().await;
                        !settings.starter_kois_initialized
                    };

                    if should_seed {
                        let created = {
                            let db = db_arc.lock().await;
                            db.ensure_starter_kois()
                        };

                        match created {
                            Ok(created) => {
                                let mut settings = settings_arc.lock().await;
                                settings.starter_kois_initialized = true;
                                if let Err(e) = settings.save() {
                                    tracing::warn!("Startup Koi seed: failed to persist init flag: {}", e);
                                }

                                if created.is_empty() {
                                    info!("Startup Koi seed: skipped starter Koi creation because Koi already exist");
                                } else {
                                    let names = created.iter()
                                        .map(|k| k.name.clone())
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    info!("Startup Koi seed: created starter Koi [{}]", names);
                                }
                            }
                            Err(e) => tracing::warn!("Startup Koi seed failed: {}", e),
                        }
                    }
                });
            }

            // ── Startup recovery: fix stale state from previous crash/restart ──
            {
                let db_arc = state.db.clone();
                tauri::async_runtime::block_on(async {
                    let db = db_arc.lock().await;
                    let _ = db.recover_stale_koi_status();
                    let _ = db.recover_stale_todos();
                    let _ = db.recover_stale_running_sessions(0);
                });
            }

            let startup_trial_state = store::AppState {
                db: state.db.clone(),
                settings: state.settings.clone(),
                plan_state: state.plan_state.clone(),
                browser: state.browser.clone(),
                cancel_flags: state.cancel_flags.clone(),
                confirmation_responses: state.confirmation_responses.clone(),
                interactive_responses: state.interactive_responses.clone(),
                app_handle: app_handle.clone(),
                scheduler: state.scheduler.clone(),
                gateway: state.gateway.clone(),
                pisci_heartbeat_cursor: state.pisci_heartbeat_cursor.clone(),
            };

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

            // Optional developer hook: auto-run a real collaboration trial on startup.
            if std::env::var("PISCI_RUN_COLLAB_TRIAL").ok().as_deref() == Some("1") {
                let state_ref = startup_trial_state;
                let app_for_trial = app_handle.clone();
                let exit_after_trial = std::env::var("PISCI_EXIT_AFTER_COLLAB_TRIAL").ok().as_deref() == Some("1");
                tauri::async_runtime::spawn(async move {
                    tracing::info!("Startup hook: running real collaboration trial");
                    match crate::commands::collab_trial::run_collaboration_trial_with_state(app_for_trial.clone(), &state_ref).await {
                        Ok(status) => {
                            tracing::info!(
                                "Startup hook: collaboration trial completed, completed={}, pool_id={}, steps={}",
                                status.completed,
                                status.pool_id,
                                status.steps.len()
                            );
                        }
                        Err(e) => tracing::error!("Startup hook: collaboration trial failed: {}", e),
                    }
                    if exit_after_trial {
                        tracing::info!("Startup hook: exiting after collaboration trial");
                        app_for_trial.exit(0);
                    }
                });
            }

            // Auto-run multi-agent tests in dev mode
            #[cfg(debug_assertions)]
            {
                tauri::async_runtime::spawn(async move {
                    // Always run pipeline tests (fast, no LLM)
                    info!("=== Running Multi-Agent Integration Tests ===");
                    match commands::test_runner::run_multi_agent_tests().await {
                        Ok(suite) => {
                            for r in &suite.results {
                                if r.passed {
                                    info!("[PASS] {} ({}ms)", r.name, r.duration_ms);
                                } else {
                                    tracing::error!("[FAIL] {} — {} ({}ms)", r.name, r.message, r.duration_ms);
                                }
                            }
                            info!("=== {} ===", suite.summary);
                        }
                        Err(e) => tracing::error!("Test runner error: {}", e),
                    }
                });
            }

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
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            open_path,
            // Settings
            commands::settings::get_settings,
            commands::settings::get_default_workspace,
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
            commands::memory::list_memories_for_koi,
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
            // Interactive UI (chat_ui tool responses)
            commands::interactive::respond_interactive_ui,
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
            commands::koi::get_koi_delete_info,
            commands::koi::set_koi_active,
            commands::koi::get_koi_palette,
            commands::koi::dedup_kois,
            // Chat Pool
            commands::pool::list_pool_sessions,
            commands::pool::create_pool_session,
            commands::pool::delete_pool_session,
            commands::pool::get_pool_messages,
            commands::pool::send_pool_message,
            commands::pool::get_pool_org_spec,
            commands::pool::update_pool_org_spec,
            commands::pool::dispatch_koi_task,
            commands::pool::cancel_koi_task,
            commands::pool::handle_pool_mention,
            commands::pool::pause_pool_session,
            commands::pool::resume_pool_session,
            commands::pool::archive_pool_session,
            // Board (Kanban)
            commands::board::list_koi_todos,
            commands::board::create_koi_todo,
            commands::board::update_koi_todo,
            commands::board::claim_koi_todo,
            commands::board::complete_koi_todo,
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
            // Test runner
            commands::test_runner::run_multi_agent_tests,
            commands::collab_trial::run_collaboration_trial,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pisci Desktop");
}
