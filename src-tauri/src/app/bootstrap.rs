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
//! - if invoked from `openpisci-headless`, dispatch the request to
//!   [`super::headless::run_cli_headless_request`] and exit
//!
//! Extracted from the old monolithic `desktop_app.rs`; no behaviour
//! changes vs. that file — only the helper functions were moved out to
//! [`super::logging`], [`super::markers`] and [`super::headless`].

use crate::{
    commands, gateway,
    headless_cli::{HeadlessCliRequest, HeadlessCliResponse},
    store,
};
use pisci_kernel::scheduler;
use std::sync::{Arc, Mutex as StdMutex};
use tauri::{Emitter, Manager};
use tracing::info;

use super::headless::{persist_headless_cli_result, run_cli_headless_request, CliResultSink};
use super::logging::{init_logging, install_crash_reporter};
use super::markers::{extract_send_marker, guess_mime_from_path};

fn clone_state(state: &store::AppState, app_handle: &tauri::AppHandle) -> store::AppState {
    store::AppState {
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
    run_impl(None, None);
}

pub(crate) fn run_headless_cli(request: HeadlessCliRequest) -> Result<HeadlessCliResponse, String> {
    let sink: CliResultSink = Arc::new(StdMutex::new(None));
    run_impl(Some(request), Some(sink.clone()));
    let result = sink
        .lock()
        .map_err(|_| "Failed to read headless CLI result.".to_string())?
        .take()
        .unwrap_or_else(|| Err("Headless CLI exited without producing a result.".to_string()));
    result
}

fn run_impl(
    headless_cli_request: Option<HeadlessCliRequest>,
    cli_result_sink: Option<CliResultSink>,
) {
    let _log_guard = init_logging();
    install_crash_reporter();
    let updater_enabled = std::env::var("PISCI_ENABLE_UPDATER").ok().as_deref() == Some("1");

    if !updater_enabled {
        tracing::warn!(
            "Updater plugin disabled at startup. Set PISCI_ENABLE_UPDATER=1 only after updater pubkey/endpoints are fully configured."
        );
    }

    let allow_multiple = {
        if headless_cli_request.is_some() {
            true
        } else {
            let config_path = store::settings::Settings::default_config_path();
            store::settings::Settings::load(&config_path)
                .map(|s| s.allow_multiple_instances)
                .unwrap_or(false)
        }
    };

    let setup_cli_request = headless_cli_request.clone();
    let setup_cli_sink = cli_result_sink.clone();

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
            let cli_request = setup_cli_request.clone();
            let cli_sink = setup_cli_sink.clone();
            let app_handle = app.handle().clone();

            let state = tauri::async_runtime::block_on(async {
                let scheduler = scheduler::cron::CronScheduler::new().await?;
                scheduler.start().await?;
                if let Some(app_dir) = cli_request
                    .as_ref()
                    .and_then(HeadlessCliRequest::app_data_dir_override)
                {
                    store::AppState::new_sync_with_app_dir(&app_handle, scheduler, app_dir)
                } else {
                    store::AppState::new_sync(&app_handle, scheduler)
                }
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

            if cli_request.is_none() {
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

                                let outbound = gateway::OutboundMessage {
                                    channel: msg.channel.clone(),
                                    recipient: msg.reply_target.clone(),
                                    content: clean_text,
                                    reply_to: Some(msg.id.clone()),
                                    media,
                                };
                                info!(
                                    "Sending IM reply via channel={} recipient={} len={}",
                                    msg.channel,
                                    msg.reply_target,
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

            if cli_request.is_none() && !startup_heartbeat_disabled {
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

            if cli_request.is_none() {
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

            if cli_request.is_none() {
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

            if let Some(request) = cli_request {
                if let Some(main) = app.get_webview_window("main") {
                    let _ = main.hide();
                }
                if let Some(overlay) = app.get_webview_window("overlay") {
                    let _ = overlay.hide();
                }
                let state_ref = clone_state(&state, &app_handle);
                let app_for_cli = app_handle.clone();
                let cli_output = request.output.clone();
                tauri::async_runtime::spawn(async move {
                    let result =
                        run_cli_headless_request(state_ref, app_for_cli.clone(), request).await;
                    let _ = persist_headless_cli_result(cli_output.as_deref(), &result);
                    let exit_code = if result.is_ok() { 0 } else { 1 };
                    if let Some(sink) = cli_sink {
                        if let Ok(mut slot) = sink.lock() {
                            *slot = Some(result);
                        }
                    }
                    app_for_cli.exit(exit_code);
                });
                return Ok(());
            }

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
