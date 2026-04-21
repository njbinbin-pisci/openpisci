use crate::{
    commands, gateway,
    headless_cli::{
        disabled_tools_for_mode, tool_profile, DisabledToolInfo, HeadlessCliMode,
        HeadlessCliRequest, HeadlessCliResponse, PoolWaitSummary,
    },
    scheduler, store, tools,
};
use serde_json::json;
use std::sync::{Arc, Mutex as StdMutex};
use tauri::{Emitter, Manager};
use tracing::info;
use tracing_subscriber::prelude::*;

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

type CliResultSink = Arc<StdMutex<Option<Result<HeadlessCliResponse, String>>>>;

fn persist_headless_cli_result(
    output: Option<&str>,
    result: &Result<HeadlessCliResponse, String>,
) -> Result<(), String> {
    match result {
        Ok(response) => {
            let json = serde_json::to_string_pretty(response)
                .map_err(|e| format!("Serialize failed: {}", e))?;
            if let Some(path) = output.map(str::trim).filter(|s| !s.is_empty()) {
                let path = std::path::Path::new(path);
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(parent).map_err(|e| {
                            format!("Failed to create '{}': {}", parent.display(), e)
                        })?;
                    }
                }
                std::fs::write(path, format!("{}\n", json))
                    .map_err(|e| format!("Failed to write '{}': {}", path.display(), e))?;
            } else {
                println!("{}", json);
            }
            Ok(())
        }
        Err(error) => {
            if let Some(path) = output.map(str::trim).filter(|s| !s.is_empty()) {
                let path = std::path::Path::new(path);
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(parent).map_err(|e| {
                            format!("Failed to create '{}': {}", parent.display(), e)
                        })?;
                    }
                }
                let payload = serde_json::json!({
                    "ok": false,
                    "error": error,
                });
                std::fs::write(path, format!("{}\n", payload))
                    .map_err(|e| format!("Failed to write '{}': {}", path.display(), e))?;
            }
            if !error.is_empty() {
                eprintln!("{}", error);
            }
            Err(error.clone())
        }
    }
}

fn default_app_data_dir() -> std::path::PathBuf {
    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("com.pisci.desktop")
}

/// Returns the platform log directory: `<data_dir>/logs`.
/// Falls back to `./logs` only if the platform path is unavailable.
fn log_dir() -> std::path::PathBuf {
    let base = default_app_data_dir();
    if !base.as_os_str().is_empty() {
        return base.join("logs");
    }
    std::path::PathBuf::from(".").join("logs")
}

/// Initialise structured logging:
/// - STDERR: human-readable, filtered by RUST_LOG / default "info"
/// - Rolling file: JSON, one file per day, kept up to 7 days (via tracing-appender)
///
/// Returns the `_guard` that must stay alive for the lifetime of the process
/// to ensure the non-blocking writer flushes on drop.
fn init_logging() -> tracing_appender::non_blocking::WorkerGuard {
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);

    let file_appender = tracing_appender::rolling::daily(&dir, "pisci.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "pisci_desktop_lib=debug,info".into());

    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_writer(std::io::stderr);

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

        let crash_file = dir.join(format!(
            "crash-{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%S")
        ));
        let _ = std::fs::write(&crash_file, report.to_string());

        tracing::error!(
            location = %location,
            message = %payload,
            "PANIC — crash report written to {}",
            crash_file.display()
        );

        eprintln!("PANIC at {location}: {payload}");
    }));
}

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

fn cli_disabled_tools(mode: HeadlessCliMode) -> Vec<DisabledToolInfo> {
    disabled_tools_for_mode(mode)
}

fn cli_extra_system_context(request: &HeadlessCliRequest) -> String {
    let mut lines = vec![
        "## Headless CLI Runtime".to_string(),
        format!("- Mode: {}", request.mode.as_str()),
        format!("- Host OS: {}", std::env::consts::OS),
        "- This is a non-interactive headless CLI session.".to_string(),
    ];
    let disabled = cli_disabled_tools(request.mode);
    if !disabled.is_empty() {
        lines.push("- Disabled tools in this runtime:".to_string());
        for tool in &disabled {
            lines.push(format!("  - {}: {}", tool.name, tool.reason));
        }
    }
    match request.mode {
        HeadlessCliMode::Pisci => {
            lines.push(
                "- Stay single-agent. Do not create or manage collaborative pool work in this run."
                    .to_string(),
            );
        }
        HeadlessCliMode::Pool => {
            lines.push(
                "- You are coordinating a project pool. Use pool_org + pool_chat for visible collaboration."
                    .to_string(),
            );
            if let Some(size) = request.pool_size {
                lines.push(format!(
                    "- Target collaboration scale: at most {} Koi unless the task clearly needs fewer.",
                    size
                ));
            }
            if !request.koi_ids.is_empty() {
                lines.push(format!(
                    "- Prefer coordinating these Koi IDs first: {}.",
                    request.koi_ids.join(", ")
                ));
            }
        }
    }
    if let Some(extra) = request
        .extra_system_context
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        lines.push(String::new());
        lines.push("## Additional Context".to_string());
        lines.push(extra.to_string());
    }
    lines.join("\n")
}

async fn resolve_cli_pool(
    state: &store::AppState,
    request: &HeadlessCliRequest,
) -> Result<crate::koi::PoolSession, String> {
    let db = state.db.lock().await;
    if let Some(requested) = request
        .pool_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let pool = db
            .resolve_pool_session_identifier(requested)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Pool '{}' not found.", requested))?;
        if request.task_timeout_secs.is_some() {
            db.update_pool_session_config(&pool.id, request.task_timeout_secs)
                .map_err(|e| e.to_string())?;
        }
        return Ok(pool);
    }

    let name = request
        .pool_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Headless Pool Run");
    db.create_pool_session_with_dir(
        name,
        request.workspace.as_deref(),
        request.task_timeout_secs.unwrap_or(0),
    )
    .map_err(|e| e.to_string())
}

async fn wait_for_pool_completion(
    state: &store::AppState,
    pool_id: &str,
    timeout_secs: u64,
) -> Result<PoolWaitSummary, String> {
    let start = std::time::Instant::now();
    let idle_grace = std::time::Duration::from_secs(3);
    let timeout = std::time::Duration::from_secs(timeout_secs.max(1));
    let mut zero_since: Option<std::time::Instant> = None;

    loop {
        let (active, done, cancelled, blocked, latest_messages) = {
            let db = state.db.lock().await;
            let todos = db.list_koi_todos(None).map_err(|e| e.to_string())?;
            let pool_todos = todos
                .into_iter()
                .filter(|todo| todo.pool_session_id.as_deref() == Some(pool_id))
                .collect::<Vec<_>>();
            let active = pool_todos
                .iter()
                .filter(|todo| matches!(todo.status.as_str(), "todo" | "in_progress" | "blocked"))
                .count() as u32;
            let done = pool_todos
                .iter()
                .filter(|todo| todo.status == "done")
                .count() as u32;
            let cancelled = pool_todos
                .iter()
                .filter(|todo| todo.status == "cancelled")
                .count() as u32;
            let blocked = pool_todos
                .iter()
                .filter(|todo| todo.status == "blocked")
                .count() as u32;
            let latest_messages = db
                .get_pool_messages(pool_id, 10, 0)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|msg| {
                    format!(
                        "#{} {} ({}): {}",
                        msg.id,
                        msg.sender_id,
                        msg.msg_type,
                        msg.content.chars().take(240).collect::<String>()
                    )
                })
                .collect::<Vec<_>>();
            (active, done, cancelled, blocked, latest_messages)
        };

        if active == 0 {
            match zero_since {
                Some(since) if since.elapsed() >= idle_grace => {
                    return Ok(PoolWaitSummary {
                        completed: true,
                        timed_out: false,
                        active_todos: active,
                        done_todos: done,
                        cancelled_todos: cancelled,
                        blocked_todos: blocked,
                        latest_messages,
                    });
                }
                None => zero_since = Some(std::time::Instant::now()),
                _ => {}
            }
        } else {
            zero_since = None;
        }

        if start.elapsed() >= timeout {
            return Ok(PoolWaitSummary {
                completed: false,
                timed_out: true,
                active_todos: active,
                done_todos: done,
                cancelled_todos: cancelled,
                blocked_todos: blocked,
                latest_messages,
            });
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

async fn run_cli_headless_request(
    state: store::AppState,
    app_handle: tauri::AppHandle,
    request: HeadlessCliRequest,
) -> Result<HeadlessCliResponse, String> {
    let (builtin_tool_overrides, workspace_override) = {
        let settings = state.settings.lock().await;
        (
            tools::apply_runtime_tool_profile(
                &settings.builtin_tool_enabled,
                tool_profile(request.mode),
            ),
            request
                .workspace
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string),
        )
    };

    let extra_context = cli_extra_system_context(&request);
    let disabled_tools = cli_disabled_tools(request.mode);

    let (session_id, scene_kind, pool_id) = match request.mode {
        HeadlessCliMode::Pisci => (
            request
                .session_id
                .clone()
                .unwrap_or_else(|| format!("headless_cli_{}", chrono::Utc::now().timestamp())),
            commands::scene::SceneKind::IMHeadless,
            None,
        ),
        HeadlessCliMode::Pool => {
            let pool = resolve_cli_pool(&state, &request).await?;
            (
                request
                    .session_id
                    .clone()
                    .unwrap_or_else(|| commands::chat::pool_pisci_session_id(&pool.id)),
                commands::scene::SceneKind::PoolCoordinator,
                Some(pool.id),
            )
        }
    };

    let session_title = request
        .session_title
        .clone()
        .unwrap_or_else(|| match request.mode {
            HeadlessCliMode::Pisci => "Headless CLI Task".to_string(),
            HeadlessCliMode::Pool => "Headless Pool Coordinator".to_string(),
        });
    let session_source = Some(match request.mode {
        HeadlessCliMode::Pisci => "headless_cli".to_string(),
        HeadlessCliMode::Pool => commands::chat::SESSION_SOURCE_PISCI_POOL.to_string(),
    });
    let channel = request.channel.clone().unwrap_or_else(|| "cli".to_string());

    let options = commands::chat::HeadlessRunOptions {
        pool_session_id: pool_id.clone(),
        extra_system_context: Some(extra_context),
        session_title: Some(session_title),
        session_source,
        scene_kind: Some(scene_kind),
        workspace_root_override: workspace_override,
        builtin_tool_overrides,
        context_toggles: request.context_toggles.clone(),
    };

    let (response_text, _, _) = commands::chat::run_agent_headless(
        &state,
        &session_id,
        &request.prompt,
        None,
        &channel,
        Some(options),
    )
    .await?;

    let pool_wait = if request.mode == HeadlessCliMode::Pool && request.wait_for_completion {
        Some(
            wait_for_pool_completion(
                &state,
                pool_id.as_deref().unwrap_or_default(),
                request.wait_timeout_secs.unwrap_or(900),
            )
            .await?,
        )
    } else {
        None
    };

    let _ = app_handle.emit(
        "headless_cli_completed",
        json!({ "session_id": session_id, "mode": request.mode.as_str() }),
    );

    Ok(HeadlessCliResponse {
        ok: true,
        mode: request.mode.as_str().to_string(),
        session_id,
        pool_id,
        response_text,
        disabled_tools,
        pool_wait,
    })
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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init());

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
                                        commands::scheduler::execute_task(
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

                            if let Err(e) = crate::commands::window::enter_unattended_im_mode(
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

            let startup_hooks_active = cfg!(debug_assertions)
                && (std::env::var("PISCI_RUN_COLLAB_TRIAL").ok().as_deref() == Some("1")
                    || std::env::var("PISCI_HEADLESS_PROMPT")
                        .ok()
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false));

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
                        let runtime =
                            crate::koi::runtime::KoiRuntime::from_tauri(app_h.clone(), db_arc.clone());
                        let (stale_koi, stale_todo) = runtime.watchdog_recover(0).await;
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
                        match runtime.activate_pending_todos(None).await {
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
                        let runtime =
                            crate::koi::runtime::KoiRuntime::from_tauri(app_h.clone(), db_arc.clone());

                        let (stale_koi, stale_todo) = runtime.watchdog_recover(600).await;
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

                        match runtime.activate_pending_todos(None).await {
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
                                    scene_kind: Some(commands::scene::SceneKind::IMHeadless),
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
                        match crate::commands::collab_trial::run_collaboration_trial_with_state(
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

            #[cfg(debug_assertions)]
            {
                if !startup_hooks_active {
                    tauri::async_runtime::spawn(async move {
                        info!("=== Running Multi-Agent Integration Tests ===");
                        match commands::test_runner::run_multi_agent_tests().await {
                            Ok(suite) => {
                                for r in &suite.results {
                                    if r.passed {
                                        info!("[PASS] {} ({}ms)", r.name, r.duration_ms);
                                    } else {
                                        tracing::error!(
                                            "[FAIL] {} — {} ({}ms)",
                                            r.name,
                                            r.message,
                                            r.duration_ms
                                        );
                                    }
                                }
                                info!("=== {} ===", suite.summary);
                            }
                            Err(e) => tracing::error!("Test runner error: {}", e),
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
            commands::settings::get_settings,
            commands::settings::get_default_workspace,
            commands::settings::save_settings,
            commands::settings::is_configured,
            commands::chat::create_session,
            commands::chat::list_sessions,
            commands::chat::delete_session,
            commands::chat::rename_session,
            commands::chat::get_messages,
            commands::chat::chat_send,
            commands::chat::chat_cancel,
            commands::chat::get_context_preview,
            commands::memory::list_memories,
            commands::memory::list_memories_for_koi,
            commands::memory::add_memory,
            commands::memory::delete_memory,
            commands::memory::clear_memories,
            commands::skills::list_skills,
            commands::skills::toggle_skill,
            commands::skills::scan_skill_catalog,
            commands::skills::sync_skills_from_disk,
            commands::skills::install_skill,
            commands::skills::uninstall_skill,
            commands::skills::clawhub_search,
            commands::skills::clawhub_install,
            commands::skills::check_skill_compat,
            commands::scheduler::list_tasks,
            commands::scheduler::create_task,
            commands::scheduler::update_task,
            commands::scheduler::delete_task,
            commands::scheduler::ensure_memory_consolidation_task,
            commands::scheduler::run_memory_consolidation_now,
            commands::scheduler::trigger_memory_consolidation_for_session,
            commands::scheduler::run_task_now,
            commands::scheduler::trigger_task_by_event,
            commands::system::get_vm_status,
            commands::system::get_runtime_capabilities,
            commands::system::check_runtimes,
            commands::system::set_runtime_path,
            commands::audit::get_audit_log,
            commands::audit::clear_audit_log,
            commands::permission::respond_permission,
            commands::interactive::respond_interactive_ui,
            commands::gateway::list_gateway_channels,
            commands::gateway::diagnose_gateway_channels,
            commands::gateway::connect_gateway_channels,
            commands::gateway::disconnect_gateway_channels,
            commands::gateway::start_wechat_login,
            commands::gateway::poll_wechat_login,
            commands::user_tools::list_user_tools,
            commands::user_tools::install_user_tool,
            commands::user_tools::uninstall_user_tool,
            commands::user_tools::save_user_tool_config,
            commands::user_tools::get_user_tool_config,
            commands::tools::list_builtin_tools,
            commands::tools::trigger_heartbeat,
            commands::debug::list_debug_scenarios,
            commands::debug::run_debug_scenario,
            commands::debug::run_all_debug_scenarios,
            commands::debug::run_uia_drag_test,
            commands::debug::get_debug_report,
            commands::debug::get_log_tail,
            commands::fish::get_fish_dir,
            commands::fish::list_fish,
            commands::koi::list_kois,
            commands::koi::get_koi,
            commands::koi::create_koi,
            commands::koi::update_koi,
            commands::koi::delete_koi,
            commands::koi::get_koi_delete_info,
            commands::koi::set_koi_active,
            commands::koi::get_koi_palette,
            commands::koi::dedup_kois,
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
            commands::board::list_koi_todos,
            commands::board::create_koi_todo,
            commands::board::update_koi_todo,
            commands::board::claim_koi_todo,
            commands::board::complete_koi_todo,
            commands::board::resume_koi_todo,
            commands::board::delete_koi_todo,
            commands::mcp::list_mcp_servers,
            commands::mcp::save_mcp_servers,
            commands::mcp::test_mcp_server,
            commands::window::enter_minimal_mode,
            commands::window::exit_minimal_mode,
            commands::window::set_overlay_position,
            commands::window::save_overlay_position,
            commands::window::set_app_theme,
            commands::window::set_window_theme_border,
            commands::test_runner::run_multi_agent_tests,
            commands::collab_trial::run_collaboration_trial,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pisci Desktop");
}
