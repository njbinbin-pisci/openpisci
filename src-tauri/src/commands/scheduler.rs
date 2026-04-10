use crate::agent::loop_::AgentLoop;
use crate::agent::messages::AgentEvent;
use crate::agent::tool::ToolContext;
use crate::browser::SharedBrowserManager;
use crate::commands::chat::{persist_task_spine_from_plan_state, render_task_state_section};
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
const MEMORY_CONSOLIDATION_TASK_NAME: &str = "Memory Consolidation";
const MEMORY_CONSOLIDATION_MARKER: &str = "[template:memory_consolidation]";
const MEMORY_CONSOLIDATION_DEFAULT_CRON: &str = "0 4 * * *";

#[derive(Debug, Serialize)]
pub struct TaskList {
    pub tasks: Vec<ScheduledTask>,
    pub total: usize,
}

fn trim_preview(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_string()
    } else {
        format!("{}...", text.chars().take(limit).collect::<String>())
    }
}

fn build_memory_consolidation_template() -> String {
    format!(
        "{marker}\n\
         You are running OpenPisci's background memory consolidation pass.\n\
         Review the supplied snapshot of recent sessions, task spines, and stored memories.\n\
         Your job is to preserve only high-signal, durable information.\n\n\
         Rules:\n\
         1. Prefer facts, decisions, procedures, user preferences, and durable project context.\n\
         2. Do not restate routine chatter or ephemeral intermediate steps.\n\
         3. Before saving new memory, use memory_store(action=\"search\") to check for duplicates.\n\
         4. Save at most 3-5 high-value memories via memory_store(action=\"save\").\n\
         5. If there are stale pending tasks worth resurfacing, mention them in the final summary.\n\
         6. Keep the final reply concise: what you consolidated, what you skipped, and any recall/reminder worth showing.\n",
        marker = MEMORY_CONSOLIDATION_MARKER
    )
}

fn is_memory_consolidation_prompt(task_prompt: &str) -> bool {
    task_prompt.contains(MEMORY_CONSOLIDATION_MARKER)
}

fn build_memory_consolidation_snapshot(db: &Database) -> String {
    let sessions = db.list_sessions(8, 0).unwrap_or_default();
    let task_states = db.list_recent_task_states(8).unwrap_or_default();
    let memories = db.list_memories_for_owner("pisci").unwrap_or_default();

    let session_lines = if sessions.is_empty() {
        "- No recent sessions".to_string()
    } else {
        sessions
            .iter()
            .take(6)
            .map(|s| {
                let summary = if s.rolling_summary.trim().is_empty() {
                    "no rolling summary".to_string()
                } else {
                    trim_preview(&s.rolling_summary.replace('\n', " "), 180)
                };
                format!(
                    "- {} [{}] msgs={} status={} summary={}",
                    s.title.clone().unwrap_or_else(|| s.id.clone()),
                    s.source,
                    s.message_count,
                    s.status,
                    summary
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let task_lines = if task_states.is_empty() {
        "- No persisted task spines".to_string()
    } else {
        task_states
            .iter()
            .filter(|t| t.status == "active" || t.status == "completed")
            .take(6)
            .map(|t| {
                let spine = t.to_task_spine();
                format!(
                    "- {}:{} status={} goal={} current_step={} pending={} done={}",
                    t.scope_type,
                    t.scope_id,
                    t.status,
                    trim_preview(&spine.goal, 80),
                    trim_preview(&spine.current_step, 80),
                    spine.pending.len(),
                    spine.done.len()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let memory_lines = if memories.is_empty() {
        "- No stored memories".to_string()
    } else {
        memories
            .iter()
            .rev()
            .take(8)
            .map(|m| {
                format!(
                    "- [{}] {}",
                    m.category,
                    trim_preview(&m.content.replace('\n', " "), 160)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "## Consolidation Snapshot\n\
         ### Recent Sessions\n{session_lines}\n\n\
         ### Task Spine Snapshot\n{task_lines}\n\n\
         ### Recent Stored Memories\n{memory_lines}\n",
        session_lines = session_lines,
        task_lines = task_lines,
        memory_lines = memory_lines
    )
}

fn resolve_memory_consolidation_prompt(task_prompt: &str, db: &Database) -> String {
    if !is_memory_consolidation_prompt(task_prompt) {
        return task_prompt.to_string();
    }
    format!(
        "{}\n\n{}",
        build_memory_consolidation_snapshot(db),
        task_prompt
    )
}

async fn register_task_job(state: &AppState, task: &ScheduledTask) {
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
        match sched
            .add_job(&cron, task_id.clone(), move |_uuid, _sched| {
                let app_h = app_h.clone();
                let task_id = task_id.clone();
                let task_prompt = task_prompt_clone.clone();
                let db_arc = db_arc.clone();
                let settings_arc = settings_arc.clone();
                let browser = browser.clone();
                let cancel_flags = cancel_flags.clone();
                Box::pin(async move {
                    execute_task(
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
            .await
        {
            Ok(job_id) => info!(
                "Scheduled task {} registered as job {}",
                task_id_log, job_id
            ),
            Err(e) => warn!(
                "Failed to register task {} in scheduler: {}",
                task_id_log, e
            ),
        }
    });
}

async fn ensure_memory_consolidation_task_inner(
    state: &AppState,
    cron_expression: Option<&str>,
) -> Result<ScheduledTask, String> {
    let cron = cron_expression
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(MEMORY_CONSOLIDATION_DEFAULT_CRON);
    if cron.split_whitespace().count() != 5 {
        return Err(format!(
            "Invalid cron expression '{}': must have 5 parts (minute hour day month weekday)",
            cron
        ));
    }

    let task_prompt = build_memory_consolidation_template();
    let description =
        "Background consolidation of recent sessions, task spines, and durable memories.";

    let task = {
        let db = state.db.lock().await;
        let existing = db
            .list_tasks()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|t| t.name == MEMORY_CONSOLIDATION_TASK_NAME);
        match existing {
            Some(task) => {
                db.update_task(
                    &task.id,
                    Some(MEMORY_CONSOLIDATION_TASK_NAME),
                    Some(cron),
                    Some(&task_prompt),
                    Some("active"),
                )
                .map_err(|e| e.to_string())?;
                db.get_task(&task.id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "Memory consolidation task disappeared after update".to_string())?
            }
            None => db
                .create_task(
                    MEMORY_CONSOLIDATION_TASK_NAME,
                    Some(description),
                    cron,
                    &task_prompt,
                )
                .map_err(|e| e.to_string())?,
        }
    };

    register_task_job(state, &task).await;
    Ok(task)
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
        db.create_task(
            &name,
            description.as_deref(),
            &cron_expression,
            &task_prompt,
        )
        .map_err(|e| e.to_string())?
    };
    register_task_job(&state, &task).await;

    Ok(task)
}

#[tauri::command]
pub async fn ensure_memory_consolidation_task(
    state: State<'_, AppState>,
    cron_expression: Option<String>,
) -> Result<ScheduledTask, String> {
    ensure_memory_consolidation_task_inner(&state, cron_expression.as_deref()).await
}

#[tauri::command]
pub async fn run_memory_consolidation_now(
    state: State<'_, AppState>,
    cron_expression: Option<String>,
) -> Result<String, String> {
    let task = ensure_memory_consolidation_task_inner(&state, cron_expression.as_deref()).await?;
    run_task_now(state, task.id).await
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
pub async fn delete_task(state: State<'_, AppState>, task_id: String) -> Result<(), String> {
    let db = state.db.lock().await;
    db.delete_task(&task_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn run_task_now(state: State<'_, AppState>, task_id: String) -> Result<String, String> {
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
        execute_task(
            app_h,
            task_id_clone,
            task_prompt,
            db_arc,
            settings_arc,
            browser,
            cancel_flags,
        )
        .await;
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
        execute_task(
            app_h,
            task_id_clone,
            enriched_prompt,
            db_arc,
            settings_arc,
            browser,
            cancel_flags,
        )
        .await;
    });

    Ok(format!(
        "Task '{}' triggered by {}",
        task.name, trigger_type
    ))
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

    let (
        provider,
        model,
        api_key,
        base_url,
        workspace_root,
        max_tokens,
        policy_mode,
        tool_rate_limit_per_minute,
        tool_settings,
        max_iterations,
        builtin_tool_enabled,
        allow_outside_workspace,
    ) = {
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
    let effective_task_prompt = {
        let db_lock = db.lock().await;
        resolve_memory_consolidation_prompt(&task_prompt, &db_lock)
    };

    if api_key.is_empty() {
        warn!(
            "Scheduled task {}: API key not configured, skipping",
            task_id
        );
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
        if base_url.is_empty() {
            None
        } else {
            Some(&base_url)
        },
    );
    let user_tools_dir: Option<std::path::PathBuf> =
        app.path().app_data_dir().map(|d| d.join("user-tools")).ok();
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
    let policy = Arc::new(PolicyGate::with_profile_and_flags(
        &workspace_root,
        &policy_mode,
        tool_rate_limit_per_minute,
        allow_outside_workspace,
    ));

    // Inject task state for scheduled tasks (cross-run continuity)
    let task_state_section = {
        let db_lock = db.lock().await;
        let scope_id = format!("sched_{}", task_id);
        match db_lock.load_task_state("scheduled_task", &scope_id) {
            Ok(Some(ts))
                if ts.status == "active" && (!ts.goal.is_empty() || !ts.summary.is_empty()) =>
            {
                render_task_state_section("Previous Task State", "Progress from last run", &ts)
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
        context_window: 0,
        fallback_models: vec![],
        db: Some(db.clone()),
        app_handle: None,
        confirmation_responses: None,
        confirm_flags: crate::agent::loop_::ConfirmFlags {
            confirm_shell: false,
            confirm_file_write: false,
        },
        vision_override: None,
        notification_rx: None,
        auto_compact_input_tokens_threshold: 100_000,
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
        content: MessageContent::text(&effective_task_prompt),
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
    let _ = app.emit(
        &format!("task_status_{}", task_id),
        serde_json::json!({ "status": "running" }),
    );

    let mut attempt = 0usize;
    let run_success;
    loop {
        match agent
            .run(
                messages.clone(),
                event_tx.clone(),
                cancel.clone(),
                ctx.clone(),
            )
            .await
        {
            Ok(_) => {
                run_success = true;
                break;
            }
            Err(e) => {
                attempt += 1;
                warn!(
                    "Scheduled task {} failed (attempt {}/{}): {}",
                    task_id, attempt, TASK_MAX_RETRIES, e
                );
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
    let scope_id = format!("sched_{}", task_id);
    persist_task_spine_from_plan_state(
        &app,
        &db,
        &scope_id,
        "scheduled_task",
        &scope_id,
        &effective_task_prompt,
    )
    .await;

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

    info!(
        "Scheduled task {} completed (success={})",
        task_id, run_success
    );
}
