use crate::agent::loop_::AgentLoop;
use crate::agent::messages::AgentEvent;
use crate::agent::tool::ToolContext;
use crate::browser::SharedBrowserManager;
use crate::llm::{build_client, LlmMessage, MessageContent};
use crate::policy::PolicyGate;
use crate::store::{db::ScheduledTask, AppState, Database, Settings};
use crate::tools;
use serde::Serialize;
use std::sync::{atomic::AtomicBool, Arc};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;
use tracing::{info, warn};

const TASK_MAX_RETRIES: usize = 3;

#[derive(Debug, Serialize)]
pub struct TaskList {
    pub tasks: Vec<ScheduledTask>,
    pub total: usize,
}

#[tauri::command]
pub async fn list_tasks(state: State<'_, AppState>) -> Result<TaskList, String> {
    let db = state.db.lock().await;
    let tasks = db.list_tasks().map_err(|e| e.to_string())?;
    let total = tasks.len();
    Ok(TaskList { tasks, total })
}

#[tauri::command]
pub async fn create_task(
    state: State<'_, AppState>,
    name: String,
    description: Option<String>,
    cron_expression: String,
    task_prompt: String,
) -> Result<ScheduledTask, String> {
    let parts: Vec<&str> = cron_expression.split_whitespace().collect();
    if parts.len() != 5 {
        return Err(format!(
            "Invalid cron expression '{}': must have 5 parts (minute hour day month weekday)",
            cron_expression
        ));
    }

    let task = {
        let db = state.db.lock().await;
        db.create_task(&name, description.as_deref(), &cron_expression, &task_prompt)
            .map_err(|e| e.to_string())?
    };

    // Register the new task in the live scheduler
    let app_h = state.app_handle.clone();
    let task_id = task.id.clone();
    let task_prompt_clone = task.task_prompt.clone();
    let db_arc = state.db.clone();
    let settings_arc = state.settings.clone();
    let browser = state.browser.clone();
    let cancel_flags = state.cancel_flags.clone();
    let cron = task.cron_expression.clone();
    let sched = state.scheduler.clone();
    let task_id_log = task.id.clone();

    tokio::spawn(async move {
        match sched.add_job(&cron, task_id.clone(), move |_uuid, _sched| {
            let app_h = app_h.clone();
            let task_id = task_id.clone();
            let task_prompt = task_prompt_clone.clone();
            let db_arc = db_arc.clone();
            let settings_arc = settings_arc.clone();
            let browser = browser.clone();
            let cancel_flags = cancel_flags.clone();
            Box::pin(async move {
                execute_task(app_h, task_id, task_prompt, db_arc, settings_arc, browser, cancel_flags).await;
            })
        }).await {
            Ok(job_id) => info!("Scheduled task {} registered as job {}", task_id_log, job_id),
            Err(e) => warn!("Failed to register task {} in scheduler: {}", task_id_log, e),
        }
    });

    Ok(task)
}

#[tauri::command]
pub async fn update_task(
    state: State<'_, AppState>,
    task_id: String,
    name: Option<String>,
    cron_expression: Option<String>,
    task_prompt: Option<String>,
    status: Option<String>,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.update_task(
        &task_id,
        name.as_deref(),
        cron_expression.as_deref(),
        task_prompt.as_deref(),
        status.as_deref(),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_task(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.delete_task(&task_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn run_task_now(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<String, String> {
    let task = {
        let db = state.db.lock().await;
        db.get_task(&task_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Task {} not found", task_id))?
    };

    // Record the run
    {
        let db = state.db.lock().await;
        db.record_task_run(&task_id).map_err(|e| e.to_string())?;
    }

    // Execute the task asynchronously
    let app_h = state.app_handle.clone();
    let task_id_clone = task.id.clone();
    let task_prompt = task.task_prompt.clone();
    let db_arc = state.db.clone();
    let settings_arc = state.settings.clone();
    let browser = state.browser.clone();
    let cancel_flags = state.cancel_flags.clone();

    tokio::spawn(async move {
        execute_task(app_h, task_id_clone, task_prompt, db_arc, settings_arc, browser, cancel_flags).await;
    });

    Ok(format!("Task '{}' triggered manually", task.name))
}

/// Trigger a task from external events (webhook/email relay).
/// The payload is appended to the task prompt as contextual input.
#[tauri::command]
pub async fn trigger_task_by_event(
    state: State<'_, AppState>,
    task_id: String,
    trigger_type: String,
    payload: Option<String>,
) -> Result<String, String> {
    let task = {
        let db = state.db.lock().await;
        db.get_task(&task_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Task {} not found", task_id))?
    };

    let enriched_prompt = if let Some(p) = payload {
        format!(
            "{}\n\n[trigger_type={}]\n[payload]\n{}",
            task.task_prompt, trigger_type, p
        )
    } else {
        format!("{}\n\n[trigger_type={}]", task.task_prompt, trigger_type)
    };

    let app_h = state.app_handle.clone();
    let task_id_clone = task.id.clone();
    let db_arc = state.db.clone();
    let settings_arc = state.settings.clone();
    let browser = state.browser.clone();
    let cancel_flags = state.cancel_flags.clone();
    tokio::spawn(async move {
        execute_task(app_h, task_id_clone, enriched_prompt, db_arc, settings_arc, browser, cancel_flags).await;
    });

    Ok(format!("Task '{}' triggered by {}", task.name, trigger_type))
}

/// Core task execution: runs the task_prompt through the Agent loop.
/// Emits agent events on the "scheduler_task_{task_id}" Tauri event channel.
pub async fn execute_task(
    app: AppHandle,
    task_id: String,
    task_prompt: String,
    db: Arc<Mutex<Database>>,
    settings: Arc<Mutex<Settings>>,
    browser: SharedBrowserManager,
    cancel_flags: Arc<Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>>,
) {
    info!("Executing scheduled task: {}", task_id);
    {
        let db = db.lock().await;
        let _ = db.record_task_run(&task_id);
    }

    let (provider, model, api_key, base_url, workspace_root, max_tokens, policy_mode, tool_rate_limit_per_minute, tool_settings, max_iterations, builtin_tool_enabled, allow_outside_workspace) = {
        let s = settings.lock().await;
        (
            s.provider.clone(),
            s.model.clone(),
            s.active_api_key().to_string(),
            s.custom_base_url.clone(),
            s.workspace_root.clone(),
            s.max_tokens,
            s.policy_mode.clone(),
            s.tool_rate_limit_per_minute,
            std::sync::Arc::new(crate::agent::tool::ToolSettings::from_settings(&s)),
            s.max_iterations,
            s.builtin_tool_enabled.clone(),
            s.allow_outside_workspace,
        )
    };

    if api_key.is_empty() {
        warn!("Scheduled task {}: API key not configured, skipping", task_id);
        return;
    }

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut flags = cancel_flags.lock().await;
        flags.insert(format!("sched_{}", task_id), cancel.clone());
    }

    let client = build_client(
        &provider,
        &api_key,
        if base_url.is_empty() { None } else { Some(&base_url) },
    );
    let user_tools_dir: Option<std::path::PathBuf> = app
        .path()
        .app_data_dir()
        .map(|d| d.join("user-tools"))
        .ok();
    let app_data_dir_s = app.path().app_data_dir().ok();
    let registry = Arc::new(tools::build_registry(
        browser,
        user_tools_dir.as_deref(),
        Some(db.clone()),
        Some(&builtin_tool_enabled),
        Some(app.clone()),
        Some(settings.clone()),
        app_data_dir_s,
        None, // skill_search not used in scheduled task sessions
    ));
    let policy = Arc::new(PolicyGate::with_profile_and_flags(&workspace_root, &policy_mode, tool_rate_limit_per_minute, allow_outside_workspace));

    // Inject task state for scheduled tasks (cross-run continuity)
    let task_state_section = {
        let db_lock = db.lock().await;
        let scope_id = format!("sched_{}", task_id);
        match db_lock.load_task_state("scheduled_task", &scope_id) {
            Ok(Some(ts)) if ts.status == "active" && (!ts.goal.is_empty() || !ts.summary.is_empty()) => {
                let mut ctx = String::from("\n\n## Previous Task State\n");
                if !ts.goal.is_empty() {
                    ctx.push_str(&format!("**Goal:** {}\n", ts.goal));
                }
                if !ts.summary.is_empty() {
                    ctx.push_str(&format!("**Progress from last run:** {}\n", ts.summary));
                }
                if ts.state_json != "{}" && !ts.state_json.is_empty() {
                    ctx.push_str(&format!("**Details:** {}\n", ts.state_json));
                }
                ctx
            }
            _ => String::new(),
        }
    };

    let agent = AgentLoop {
        client,
        registry,
        policy,
        system_prompt: format!(
            "You are Pisci, a Windows AI Agent running a scheduled task.\n\
             Task ID: {}\n\
             Today's date: {}{}",
            task_id,
            chrono::Utc::now().format("%Y-%m-%d"),
            task_state_section
        ),
        model,
        max_tokens,
        db: Some(db.clone()),
        app_handle: None,
        confirmation_responses: None,
        confirm_flags: crate::agent::loop_::ConfirmFlags {
            confirm_shell: false,
            confirm_file_write: false,
        },
        vision_override: None,
        notification_rx: None,
    };

    let ctx = ToolContext {
        session_id: format!("sched_{}", task_id),
        workspace_root: std::path::PathBuf::from(&workspace_root),
        bypass_permissions: false,
        settings: tool_settings,
        max_iterations: Some(max_iterations),
        memory_owner_id: "pisci".to_string(),
        pool_session_id: None,
    };

    let messages = vec![LlmMessage {
        role: "user".into(),
        content: MessageContent::text(&task_prompt),
    }];

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);

    let app_clone = app.clone();
    let task_id_clone = task_id.clone();
    let forward_handle = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let payload = serde_json::to_value(&event).unwrap_or_default();
            let _ = app_clone.emit(&format!("scheduler_task_{}", task_id_clone), payload);
        }
    });

    // Mark as running
    {
        let db_lock = db.lock().await;
        let _ = db_lock.update_task_run_status(&task_id, "running");
    }
    let _ = app.emit(&format!("task_status_{}", task_id), serde_json::json!({ "status": "running" }));

    let mut attempt = 0usize;
    let run_success;
    loop {
        match agent.run(messages.clone(), event_tx.clone(), cancel.clone(), ctx.clone()).await {
            Ok(_) => {
                run_success = true;
                break;
            }
            Err(e) => {
                attempt += 1;
                warn!("Scheduled task {} failed (attempt {}/{}): {}", task_id, attempt, TASK_MAX_RETRIES, e);
                if attempt >= TASK_MAX_RETRIES {
                    run_success = false;
                    let db_lock = db.lock().await;
                    let _ = db_lock.update_task(&task_id, None, None, None, Some("error"));
                    break;
                }
                let backoff = std::time::Duration::from_secs(1 << (attempt - 1));
                tokio::time::sleep(backoff).await;
            }
        }
    }

    // Write final run status
    {
        let db_lock = db.lock().await;
        let final_status = if run_success { "success" } else { "failed" };
        let _ = db_lock.update_task_run_status(&task_id, final_status);
        let _ = app.emit(
            &format!("task_status_{}", task_id),
            serde_json::json!({ "status": final_status }),
        );
    }

    let _ = forward_handle.await;

    {
        let mut flags = cancel_flags.lock().await;
        flags.remove(&format!("sched_{}", task_id));
    }

    info!("Scheduled task {} completed (success={})", task_id, run_success);
}
