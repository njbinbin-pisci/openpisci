//! Tauri app bootstrap.
//!
//! This is the Desktop host's main entry point. It glues everything
//! together at process start:
//! - initialise logging + crash reporter
//! - decide whether to allow multiple instances
//! - build the Tauri `Builder`, install plugins, register state
//! - spawn the long-running background loops (IM inbound, heartbeat,
//!   Koi patrol, startup skill sync, startup Koi seed, stale-state
//!   recovery)
//! - register all `tauri::command` entry points via `generate_handler!`
//!
//! Extracted from the old monolithic `desktop_app.rs`; no behaviour
//! changes vs. that file — only the helper functions were moved out to
//! [`super::logging`], [`super::markers`] and [`super::headless`].

use crate::{commands, gateway, store};
use pisci_kernel::scheduler;
use serde_json::Value;
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tracing::info;
use uuid::Uuid;

use super::logging::{init_logging, install_crash_reporter};
use super::markers::{extract_send_marker, guess_mime_from_path};

fn build_im_session_title(msg: &gateway::InboundMessage) -> String {
    let label = if msg.is_group {
        msg.group_name
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .or(msg.sender_name.as_deref())
            .unwrap_or(&msg.sender)
    } else {
        msg.sender_name.as_deref().unwrap_or(&msg.sender)
    };
    format!("{} · {}", msg.channel, label)
}

fn build_new_im_session_id(msg: &gateway::InboundMessage) -> String {
    format!("im_{}_{}", msg.channel, Uuid::new_v4())
}

async fn resolve_or_create_im_binding(
    db: &Arc<tokio::sync::Mutex<store::Database>>,
    msg: &gateway::InboundMessage,
) -> Result<store::db::ImSessionBinding, String> {
    let source = format!("im_{}", msg.channel);
    let title = build_im_session_title(msg);
    let binding_key = msg.binding_key();
    let external_conversation_key = msg.effective_conversation_key();
    let routing_state_json = msg
        .routing_state
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| e.to_string())?;

    let db_lock = db.lock().await;
    let session_id = if let Some(existing) = db_lock
        .get_im_session_binding(&binding_key)
        .map_err(|e| e.to_string())?
    {
        existing.session_id
    } else {
        build_new_im_session_id(msg)
    };

    let _ = db_lock
        .ensure_im_session(&session_id, &title, &source)
        .map_err(|e| e.to_string())?;
    let _ = db_lock.rename_session(&session_id, &title);

    db_lock
        .upsert_im_session_binding(&store::db::ImSessionBindingUpsert {
            binding_key,
            channel: msg.channel.clone(),
            external_conversation_key,
            session_id,
            peer_id: msg.sender.clone(),
            peer_name: msg.sender_name.clone(),
            is_group: msg.is_group,
            group_name: msg.group_name.clone(),
            latest_reply_target: msg.reply_target.clone(),
            routing_state_json,
        })
        .map_err(|e| e.to_string())
}

async fn resolve_im_outbound_route(
    db: &Arc<tokio::sync::Mutex<store::Database>>,
    session_id: &str,
    channel: &str,
    fallback_recipient: &str,
    fallback_routing_state: Option<Value>,
) -> (String, Option<Value>) {
    let db_lock = db.lock().await;
    match db_lock.get_im_session_binding_by_session(session_id, channel) {
        Ok(Some(binding)) => {
            let recipient = if binding.latest_reply_target.trim().is_empty() {
                fallback_recipient.to_string()
            } else {
                binding.latest_reply_target
            };
            let routing_state = binding
                .routing_state_json
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok())
                .or(fallback_routing_state);
            (recipient, routing_state)
        }
        _ => (fallback_recipient.to_string(), fallback_routing_state),
    }
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
            std::process::Command::new("cmd")
                .args(["/c", "start", "", &path])
                .spawn()
                .map_err(|e| format!("Failed to open file: {e}"))?;
        }
        Ok(())
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
    run_impl();
}

fn run_impl() {
    let _log_guard = init_logging();
    install_crash_reporter();
    let updater_enabled = std::env::var("PISCI_ENABLE_UPDATER").ok().as_deref() == Some("1");

    if !updater_enabled {
        tracing::warn!(
            "Updater plugin disabled at startup. Set PISCI_ENABLE_UPDATER=1 only after updater pubkey/endpoints are fully configured."
        );
    }

    let config_path = store::settings::Settings::default_config_path();
    let allow_multiple = store::settings::Settings::load(&config_path)
        .map(|s| s.allow_multiple_instances)
        .unwrap_or(false);

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_process::init());

    if updater_enabled {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }

    if !allow_multiple {
        builder = builder.plugin(
            tauri_plugin_single_instance::Builder::new()
                .callback(|app, _args, _cwd| {
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
        .setup(move |app| {
            let app_handle = app.handle().clone();

            let state = tauri::async_runtime::block_on(async {
                let scheduler = scheduler::cron::CronScheduler::new().await?;
                scheduler.start().await?;
                store::AppState::new_sync(&app_handle, scheduler)
            })?;
            let managed_state = store::AppState {
                db: state.db.clone(),
                settings: state.settings.clone(),
                plan_state: state.plan_state.clone(),
                browser: state.browser.clone(),
                cancel_flags: state.cancel_flags.clone(),
                confirmation_responses: state.confirmation_responses.clone(),
                interactive_responses: state.interactive_responses.clone(),
                app_handle: state.app_handle.clone(),
                scheduler: state.scheduler.clone(),
                gateway: state.gateway.clone(),
                pisci_heartbeat_cursor: state.pisci_heartbeat_cursor.clone(),
            };
            app.manage(managed_state);

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
                            let _ = sched
                                .add_job(&cron, task_id.clone(), move |_uuid, _sched| {
                                    let app_h = app_h.clone();
                                    let task_id = task_id.clone();
                                    let task_prompt = task_prompt.clone();
                                    let db_arc = db_arc.clone();
                                    let settings_arc = settings_arc.clone();
                                    let browser = browser.clone();
                                    let cancel_flags = cancel_flags.clone();
                                    Box::pin(async move {
                                        commands::chat::scheduler::execute_task(
                                            app_h,
                                            task_id,
                                            task_prompt,
                                            db_arc,
                                            settings_arc,
                                            browser,
                                            cancel_flags,
                                        )
                                        .await;
                                    })
                                })
                                .await;
                        });
                    }
                }
            }

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
                let im_session_locks: std::sync::Arc<
                    tokio::sync::Mutex<
                        std::collections::HashMap<
                            String,
                            std::sync::Arc<tokio::sync::Mutex<()>>,
                        >,
                    >,
                > = std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
                tauri::async_runtime::spawn(async move {
                    if let Some(mut rx) = gateway.take_receiver().await {
                        info!("Gateway inbound consumer started");
                        while let Some(msg) = rx.recv().await {
                            info!(
                                "Inbound IM message from {} via {}: {}",
                                msg.sender,
                                msg.channel,
                                &msg.content[..msg.content.len().min(80)]
                            );

                            if let Err(e) = crate::commands::platform::window::enter_unattended_im_mode(
                                &app_h,
                                &store::AppState {
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
                                },
                            )
                            .await
                            {
                                tracing::warn!("Failed to enter unattended IM mode: {}", e);
                            }

                            let binding = match resolve_or_create_im_binding(&db, &msg).await {
                                Ok(binding) => binding,
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to resolve IM session binding for channel={} sender={}: {}",
                                        msg.channel,
                                        msg.sender,
                                        e
                                    );
                                    continue;
                                }
                            };
                            let session_id = binding.session_id.clone();
                            {
                                let db_lock = db.lock().await;
                                let _ = db_lock.append_message(&session_id, "user", &msg.content);
                                let _ = db_lock.update_session_status(&session_id, "running");
                            }
                            match app_h.emit("im_session_updated", &session_id) {
                                Ok(()) => info!("Emitted im_session_updated for session={}", session_id),
                                Err(e) => tracing::warn!("Failed to emit im_session_updated: {}", e),
                            }

                            let session_lock = {
                                let mut locks = im_session_locks.lock().await;
                                locks
                                    .entry(session_id.clone())
                                    .or_insert_with(|| {
                                        std::sync::Arc::new(tokio::sync::Mutex::new(()))
                                    })
                                    .clone()
                            };

                            {
                                let flags = cancel_flags.lock().await;
                                if let Some(flag) = flags.get(&session_id) {
                                    info!(
                                        "Cancelling previous agent for session {} due to new inbound message",
                                        session_id
                                    );
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
                                let _session_guard = session_lock.lock().await;
                                info!("IM session lock acquired for {}", session_id);

                                let response = commands::chat::run_agent_headless(
                                    &state_ref,
                                    &session_id,
                                    &msg.content,
                                    inbound_media,
                                    &msg_channel,
                                    None,
                                )
                                .await;

                                if response.is_err() {
                                    info!(
                                        "run_agent_headless returned error, emitting im_session_done for {}",
                                        session_id
                                    );
                                    let _ = state_ref.app_handle.emit("im_session_done", &session_id);
                                }

                                let (reply_text, reply_image, reply_image_mime) = match response {
                                    Ok((text, img, mime)) => {
                                        let t = if text.is_empty() && img.is_none() {
                                            "（Agent 未返回内容）".to_string()
                                        } else {
                                            text
                                        };
                                        (t, img, mime)
                                    }
                                    Err(e) => (format!("Agent error: {}", e), None, None),
                                };

                                let (clean_text, file_path) = extract_send_marker(&reply_text);

                                let media = file_path
                                    .and_then(|p| match std::fs::read(&p) {
                                        Ok(data) => {
                                            let mime = guess_mime_from_path(&p);
                                            let filename = std::path::Path::new(&p)
                                                .file_name()
                                                .map(|n| n.to_string_lossy().into_owned())
                                                .unwrap_or_else(|| "file".to_string());
                                            info!(
                                                "extract_send_marker: read {} bytes from '{}', mime={}",
                                                data.len(),
                                                p,
                                                mime
                                            );
                                            Some(gateway::MediaAttachment {
                                                media_type: mime,
                                                url: None,
                                                data: Some(data),
                                                filename: Some(filename),
                                            })
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "extract_send_marker: failed to read file '{}': {}",
                                                p,
                                                e
                                            );
                                            None
                                        }
                                    })
                                    .or_else(|| {
                                        reply_image.map(|data| gateway::MediaAttachment {
                                            media_type: reply_image_mime
                                                .unwrap_or_else(|| "image/jpeg".to_string()),
                                            url: None,
                                            data: Some(data),
                                            filename: Some("image.jpg".to_string()),
                                        })
                                    });

                                let (recipient, routing_state) = resolve_im_outbound_route(
                                    &state_ref.db,
                                    &session_id,
                                    &msg.channel,
                                    &msg.reply_target,
                                    msg.routing_state.clone(),
                                )
                                .await;

                                let outbound = gateway::OutboundMessage {
                                    channel: msg.channel.clone(),
                                    recipient: recipient.clone(),
                                    content: clean_text,
                                    reply_to: Some(msg.id.clone()),
                                    media,
                                    routing_state,
                                };
                                info!(
                                    "Sending IM reply via channel={} recipient={} len={}",
                                    msg.channel,
                                    recipient,
                                    outbound.content.len()
                                );
                                match gw.send(&outbound).await {
                                    Ok(()) => info!("IM reply sent successfully via {}", msg.channel),
                                    Err(e) => {
                                        tracing::warn!(
                                            "Failed to send IM reply via {}: {}",
                                            msg.channel,
                                            e
                                        )
                                    }
                                }
                            });
                        }
                    }
                });
            }

            let startup_heartbeat_disabled =
                std::env::var("PISCI_DISABLE_STARTUP_HEARTBEAT").ok().as_deref() == Some("1");

            if !startup_heartbeat_disabled {
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
                        tokio::time::sleep(std::time::Duration::from_secs(
                            interval_mins as u64 * 60,
                        ))
                        .await;
                        let still_enabled = {
                            let s = settings_arc.lock().await;
                            s.heartbeat_enabled
                        };
                        if !still_enabled {
                            continue;
                        }
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
                        let _ = crate::pisci::heartbeat::dispatch_heartbeat(
                            &state_ref,
                            &prompt,
                            "heartbeat",
                        )
                        .await;
                    }
                });
            }

            {
                let db_arc = state.db.clone();
                let app_h = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    {
                        let (stale_koi, stale_todo) =
                            crate::pool::bridge::watchdog_recover(db_arc.clone(), 0).await;
                        let stale_sessions = {
                            let db = db_arc.lock().await;
                            db.recover_stale_running_sessions(0).unwrap_or(0)
                        };
                        if stale_koi > 0 || stale_todo > 0 || stale_sessions > 0 {
                            tracing::info!(
                                "Koi patrol startup: recovered {} stale Koi, {} stale todos, {} stale sessions",
                                stale_koi,
                                stale_todo,
                                stale_sessions
                            );
                        }
                        match crate::pool::bridge::activate_pending_todos_arc(
                            &app_h,
                            db_arc.clone(),
                            None,
                        )
                        .await
                        {
                            Ok(activated) if activated > 0 => {
                                tracing::info!(
                                    "Koi patrol startup: activated {} pending todos",
                                    activated
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Koi patrol startup: pending todo activation error: {}",
                                    e
                                );
                            }
                            _ => {}
                        }
                    }

                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    loop {
                        let (stale_koi, stale_todo) =
                            crate::pool::bridge::watchdog_recover(db_arc.clone(), 600).await;
                        let stale_sessions = {
                            let db = db_arc.lock().await;
                            db.recover_stale_running_sessions(600).unwrap_or(0)
                        };
                        if stale_koi > 0 || stale_todo > 0 || stale_sessions > 0 {
                            tracing::info!(
                                "Koi patrol: recovered {} stale Koi, {} stale todos, {} stale sessions",
                                stale_koi,
                                stale_todo,
                                stale_sessions
                            );
                        }

                        match crate::pool::bridge::activate_pending_todos_arc(
                            &app_h,
                            db_arc.clone(),
                            None,
                        )
                        .await
                        {
                            Ok(activated) if activated > 0 => {
                                tracing::info!("Koi patrol: activated {} pending todos", activated);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Koi patrol: pending todo activation error: {}",
                                    e
                                );
                            }
                            _ => {}
                        }

                        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
                    }
                });
            }

            {
                let db_arc = state.db.clone();
                let app_handle_clone = app_handle.clone();
                tauri::async_runtime::block_on(async {
                    let app_dir = app_handle_clone
                        .path()
                        .app_data_dir()
                        .unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
                    let skills_dir = app_dir.join("skills");

                    let mut loader = crate::skills::loader::SkillLoader::new(&skills_dir);
                    if let Err(e) = loader.load_all() {
                        tracing::warn!(
                            "Startup skill sync: failed to load skills from disk: {}",
                            e
                        );
                    }

                    let db = db_arc.lock().await;
                    for skill in loader.list_skills() {
                        if skill.name.is_empty() || skill.name == "unnamed" {
                            continue;
                        }
                        let safe_id: String = skill
                            .name
                            .chars()
                            .map(|c| {
                                if c.is_alphanumeric() || c == '-' || c == '_' {
                                    c
                                } else {
                                    '_'
                                }
                            })
                            .collect::<String>()
                            .to_lowercase();
                        if let Err(e) =
                            db.upsert_skill(&safe_id, &skill.name, &skill.description, "📦")
                        {
                            tracing::warn!(
                                "Startup skill sync: failed to upsert '{}': {}",
                                skill.name,
                                e
                            );
                        } else {
                            tracing::debug!("Startup skill sync: upserted '{}'", skill.name);
                        }
                    }

                    if let Ok(db_skills) = db.list_skills() {
                        for s in db_skills {
                            if s.name == "unnamed" || s.id == "unnamed" {
                                let _ = db.delete_skill(&s.id);
                                tracing::info!(
                                    "Startup skill sync: removed stale 'unnamed' entry '{}'",
                                    s.id
                                );
                            }
                        }
                    }
                });
            }

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
                                    tracing::warn!(
                                        "Startup Koi seed: failed to persist init flag: {}",
                                        e
                                    );
                                }

                                if created.is_empty() {
                                    info!(
                                        "Startup Koi seed: skipped starter Koi creation because Koi already exist"
                                    );
                                } else {
                                    let names = created
                                        .iter()
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

            {
                let db_arc = state.db.clone();
                tauri::async_runtime::block_on(async {
                    let db = db_arc.lock().await;
                    let _ = db.recover_stale_koi_status();
                    let _ = db.recover_stale_todos();
                    let _ = db.recover_stale_running_sessions(0);
                });
            }

            {
                if let Some(tray) = app.tray_by_id("main") {
                    use tauri::menu::{Menu, MenuItem};
                    if let (Ok(show_i), Ok(quit_i)) = (
                        MenuItem::with_id(&app_handle, "tray_show", "显示主界面", true, None::<&str>),
                        MenuItem::with_id(
                            &app_handle,
                            "tray_quit",
                            "退出 OpenPisci",
                            true,
                            None::<&str>,
                        ),
                    ) {
                        if let Ok(menu) = Menu::with_items(&app_handle, &[&show_i, &quit_i]) {
                            if let Err(e) = tray.set_menu(Some(menu)) {
                                tracing::warn!("Tray set_menu: {}", e);
                            }
                        }
                    }
                    let _ = tray.set_tooltip(Some("OpenPisci"));
                }
            }

            info!("OpenPisci started");

            #[cfg(debug_assertions)]
            {
                if std::env::var("PISCI_HIDE_WINDOWS_ON_STARTUP").ok().as_deref() == Some("1")
                    || std::env::var("PISCI_RUN_COLLAB_TRIAL").ok().as_deref() == Some("1")
                {
                    if let Some(main) = app.get_webview_window("main") {
                        let _ = main.hide();
                    }
                }

                let startup_headless_state = store::AppState {
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

                if let Ok(prompt) = std::env::var("PISCI_HEADLESS_PROMPT") {
                    if !prompt.trim().is_empty() {
                        let state_ref = startup_headless_state;
                        let app_for_headless = app_handle.clone();
                        let exit_after =
                            std::env::var("PISCI_EXIT_AFTER_HEADLESS_PROMPT").ok().as_deref()
                                == Some("1");
                        let session_id = std::env::var("PISCI_HEADLESS_SESSION_ID")
                            .unwrap_or_else(|_| "startup_headless".to_string());
                        let session_title = std::env::var("PISCI_HEADLESS_SESSION_TITLE")
                            .unwrap_or_else(|_| "Startup Headless Task".to_string());
                        let channel = std::env::var("PISCI_HEADLESS_CHANNEL")
                            .unwrap_or_else(|_| "startup".to_string());
                        let extra_system_context =
                            std::env::var("PISCI_HEADLESS_EXTRA_SYSTEM_CONTEXT").ok();
                        tauri::async_runtime::spawn(async move {
                            tracing::info!(
                                "Startup hook: running headless Pisci task session_id={}",
                                session_id
                            );
                            match commands::chat::run_agent_headless(
                                &state_ref,
                                &session_id,
                                &prompt,
                                None,
                                &channel,
                                Some(commands::chat::HeadlessRunOptions {
                                    pool_session_id: None,
                                    extra_system_context,
                                    session_title: Some(session_title),
                                    session_source: Some("startup_hook".to_string()),
                                    scene_kind: Some(commands::config::scene::SceneKind::IMHeadless),
                                    ..commands::chat::HeadlessRunOptions::default()
                                }),
                            )
                            .await
                            {
                                Ok((text, _, _)) => tracing::info!(
                                    "Startup hook: headless Pisci task completed, chars={}, preview={}",
                                    text.chars().count(),
                                    text.chars().take(400).collect::<String>()
                                ),
                                Err(e) => {
                                    tracing::error!("Startup hook: headless Pisci task failed: {}", e)
                                }
                            }
                            if exit_after {
                                tracing::info!("Startup hook: exiting after headless Pisci task");
                                app_for_headless.exit(0);
                            }
                        });
                    }
                }

                if std::env::var("PISCI_RUN_COLLAB_TRIAL").ok().as_deref() == Some("1") {
                    let state_ref = startup_trial_state;
                    let app_for_trial = app_handle.clone();
                    let exit_after_trial =
                        std::env::var("PISCI_EXIT_AFTER_COLLAB_TRIAL").ok().as_deref()
                            == Some("1");
                    tauri::async_runtime::spawn(async move {
                        tracing::info!("Startup hook: running real collaboration trial");
                        match crate::commands::chat::collab_trial::run_collaboration_trial_with_state(
                            app_for_trial.clone(),
                            &state_ref,
                        )
                        .await
                        {
                            Ok(status) => {
                                tracing::info!(
                                    "Startup hook: collaboration trial completed, completed={}, pool_id={}, steps={}",
                                    status.completed,
                                    status.pool_id,
                                    status.steps.len()
                                );
                            }
                            Err(e) => {
                                tracing::error!("Startup hook: collaboration trial failed: {}", e)
                            }
                        }
                        if exit_after_trial {
                            tracing::info!("Startup hook: exiting after collaboration trial");
                            app_for_trial.exit(0);
                        }
                    });
                }
            }

            Ok(())
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
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
            // config/
            commands::config::settings::get_settings,
            commands::config::settings::get_default_workspace,
            commands::config::settings::save_settings,
            commands::config::settings::is_configured,
            commands::config::memory::list_memories,
            commands::config::memory::list_memories_for_koi,
            commands::config::memory::add_memory,
            commands::config::memory::delete_memory,
            commands::config::memory::clear_memories,
            commands::config::skills::list_skills,
            commands::config::skills::toggle_skill,
            commands::config::skills::scan_skill_catalog,
            commands::config::skills::sync_skills_from_disk,
            commands::config::skills::install_skill,
            commands::config::skills::uninstall_skill,
            commands::config::skills::clawhub_search,
            commands::config::skills::clawhub_install,
            commands::config::skills::check_skill_compat,
            commands::config::audit::get_audit_log,
            commands::config::audit::clear_audit_log,
            commands::config::user_tools::list_user_tools,
            commands::config::user_tools::install_user_tool,
            commands::config::user_tools::uninstall_user_tool,
            commands::config::user_tools::save_user_tool_config,
            commands::config::user_tools::get_user_tool_config,
            commands::config::tools::list_builtin_tools,
            commands::config::tools::trigger_heartbeat,
            commands::config::mcp::list_mcp_servers,
            commands::config::mcp::save_mcp_servers,
            commands::config::mcp::test_mcp_server,
            commands::config::enterprise_capability::list_enterprise_capability_templates,
            commands::config::enterprise_capability::get_enterprise_capability_status,
            commands::config::enterprise_capability::enable_enterprise_capability,
            commands::config::enterprise_capability::test_enterprise_capability,
            // chat/
            commands::chat::create_session,
            commands::chat::list_sessions,
            commands::chat::delete_session,
            commands::chat::rename_session,
            commands::chat::get_messages,
            commands::chat::chat_send,
            commands::chat::chat_cancel,
            commands::chat::get_context_preview,
            commands::chat::scheduler::list_tasks,
            commands::chat::scheduler::create_task,
            commands::chat::scheduler::update_task,
            commands::chat::scheduler::delete_task,
            commands::chat::scheduler::ensure_memory_consolidation_task,
            commands::chat::scheduler::run_memory_consolidation_now,
            commands::chat::scheduler::trigger_memory_consolidation_for_session,
            commands::chat::scheduler::run_task_now,
            commands::chat::scheduler::trigger_task_by_event,
            commands::chat::gateway::list_gateway_channels,
            commands::chat::gateway::diagnose_gateway_channels,
            commands::chat::gateway::connect_gateway_channels,
            commands::chat::gateway::disconnect_gateway_channels,
            commands::chat::gateway::start_wechat_login,
            commands::chat::gateway::poll_wechat_login,
            commands::chat::debug::list_debug_scenarios,
            commands::chat::debug::run_debug_scenario,
            commands::chat::debug::run_all_debug_scenarios,
            commands::chat::debug::run_uia_drag_test,
            commands::chat::debug::get_debug_report,
            commands::chat::debug::get_log_tail,
            commands::chat::fish::get_fish_dir,
            commands::chat::fish::list_fish,
            commands::chat::collab_trial::run_collaboration_trial,
            // pool/
            commands::pool::list_pool_sessions,
            commands::pool::create_pool_session,
            commands::pool::delete_pool_session,
            commands::pool::get_pool_messages,
            commands::pool::send_pool_message,
            commands::pool::get_pool_org_spec,
            commands::pool::update_pool_org_spec,
            commands::pool::update_pool_session_config,
            commands::pool::dispatch_koi_task,
            commands::pool::cancel_koi_task,
            commands::pool::handle_pool_mention,
            commands::pool::pause_pool_session,
            commands::pool::resume_pool_session,
            commands::pool::archive_pool_session,
            commands::pool::koi::list_kois,
            commands::pool::koi::get_koi,
            commands::pool::koi::create_koi,
            commands::pool::koi::update_koi,
            commands::pool::koi::delete_koi,
            commands::pool::koi::get_koi_delete_info,
            commands::pool::koi::set_koi_active,
            commands::pool::koi::get_koi_palette,
            commands::pool::koi::dedup_kois,
            commands::pool::board::list_koi_todos,
            commands::pool::board::create_koi_todo,
            commands::pool::board::update_koi_todo,
            commands::pool::board::claim_koi_todo,
            commands::pool::board::complete_koi_todo,
            commands::pool::board::resume_koi_todo,
            commands::pool::board::delete_koi_todo,
            // platform/
            commands::platform::system::get_vm_status,
            commands::platform::system::get_runtime_capabilities,
            commands::platform::system::check_runtimes,
            commands::platform::system::set_runtime_path,
            commands::platform::permission::respond_permission,
            commands::platform::interactive::respond_interactive_ui,
            commands::platform::window::enter_minimal_mode,
            commands::platform::window::exit_minimal_mode,
            commands::platform::window::set_overlay_position,
            commands::platform::window::save_overlay_position,
            commands::platform::window::set_app_theme,
            commands::platform::window::set_window_theme_border,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pisci Desktop");
}

#[cfg(test)]
mod tests {
    use super::{resolve_im_outbound_route, resolve_or_create_im_binding};
    use crate::{gateway, store};
    use serde_json::json;
    use std::sync::Arc;

    fn inbound_message() -> gateway::InboundMessage {
        gateway::InboundMessage {
            id: "msg-1".to_string(),
            channel: "wechat".to_string(),
            sender: "wx-user-1".to_string(),
            sender_name: Some("Alice".to_string()),
            content: "hello".to_string(),
            reply_target: "wx-user-1|ctx-1".to_string(),
            conversation_key: Some("dm:wx-user-1".to_string()),
            is_group: false,
            group_name: None,
            timestamp: 1,
            media: None,
            routing_state: Some(json!({
                "context_token": "ctx-1",
                "session_id": "",
                "from_user_id": "wx-user-1",
            })),
        }
    }

    #[tokio::test]
    async fn resolve_or_create_im_binding_reuses_existing_session_and_updates_route() {
        let db = Arc::new(tokio::sync::Mutex::new(
            store::Database::open_in_memory().expect("in-memory db"),
        ));

        let first = resolve_or_create_im_binding(&db, &inbound_message())
            .await
            .expect("first binding");

        let mut followup = inbound_message();
        followup.id = "msg-2".to_string();
        followup.reply_target = "wx-user-1|ctx-2".to_string();
        followup.routing_state = Some(json!({
            "context_token": "ctx-2",
            "session_id": "",
            "from_user_id": "wx-user-1",
        }));

        let second = resolve_or_create_im_binding(&db, &followup)
            .await
            .expect("second binding");

        assert_eq!(second.session_id, first.session_id);
        assert_eq!(second.latest_reply_target, "wx-user-1|ctx-2");
        assert_eq!(
            second
                .routing_state_json
                .as_deref()
                .expect("routing_state_json"),
            r#"{"context_token":"ctx-2","from_user_id":"wx-user-1","session_id":""}"#
        );
    }

    #[tokio::test]
    async fn resolve_im_outbound_route_prefers_persisted_binding_state() {
        let db = Arc::new(tokio::sync::Mutex::new(
            store::Database::open_in_memory().expect("in-memory db"),
        ));
        let msg = inbound_message();
        let binding = resolve_or_create_im_binding(&db, &msg)
            .await
            .expect("binding");

        let (recipient, routing_state) = resolve_im_outbound_route(
            &db,
            &binding.session_id,
            &binding.channel,
            "fallback-user",
            Some(json!({ "context_token": "fallback" })),
        )
        .await;

        assert_eq!(recipient, "wx-user-1|ctx-1");
        assert_eq!(
            routing_state.expect("routing state")["context_token"],
            "ctx-1"
        );
    }
}
