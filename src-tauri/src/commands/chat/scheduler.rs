use crate::browser::SharedBrowserManager;
use crate::commands::chat::{persist_task_spine_from_plan_state, render_task_state_section};
use crate::host::DesktopHostTools;
use crate::store::{db::ScheduledTask, AppState, Database, Settings};
use pisci_kernel::agent::harness::HarnessConfig;
use pisci_kernel::agent::messages::AgentEvent;
use pisci_kernel::agent::tool::ToolContext;
use pisci_kernel::llm::{build_client, LlmMessage, MessageContent};
use pisci_kernel::policy::PolicyGate;
use pisci_kernel::project_context::render_project_instruction_context;
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

fn build_scheduler_session_title(task_name: Option<&str>, task_id: &str) -> String {
    let title = task_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            let short_id = task_id.chars().take(8).collect::<String>();
            short_id
        });
    format!("[定时] {}", title)
}

fn render_pisci_personal_prompt(prompt: &str) -> String {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("\n\n## Personal Pisci Prompt\n{}\n", trimmed)
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

/// Phase 4c — build a focused consolidation snapshot for a single
/// session. Used by the event-driven consolidation path
/// ([`trigger_consolidation_for_session`]) so we don't blur the
/// snapshot with unrelated sessions.
fn build_session_consolidation_snapshot(db: &Database, session_id: &str) -> String {
    let session = db.get_session(session_id).ok().flatten();
    let task_states = db.list_recent_task_states(4).unwrap_or_default();
    let memories = db.list_memories_for_owner("pisci").unwrap_or_default();

    let session_line = match session {
        Some(s) => {
            let summary = if s.rolling_summary.trim().is_empty() {
                "no rolling summary".to_string()
            } else {
                trim_preview(&s.rolling_summary.replace('\n', " "), 320)
            };
            format!(
                "- {} [{}] msgs={} status={} summary={}",
                s.title.clone().unwrap_or_else(|| s.id.clone()),
                s.source,
                s.message_count,
                s.status,
                summary
            )
        }
        None => format!("- session {} not found", session_id),
    };

    let task_lines = if task_states.is_empty() {
        "- No persisted task spines".to_string()
    } else {
        task_states
            .iter()
            .filter(|t| t.scope_id == session_id || t.status == "active")
            .take(4)
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
        // Prefer memories with evidence anchored in THIS session
        // (Phase 4d) or, failing that, the most recent 5.
        let mut relevant: Vec<_> = memories
            .iter()
            .filter(|m| {
                m.evidence_session_id.as_deref() == Some(session_id)
                    || m.source_session_id.as_deref() == Some(session_id)
            })
            .collect();
        if relevant.is_empty() {
            relevant = memories.iter().rev().take(5).collect();
        }
        relevant
            .iter()
            .take(6)
            .map(|m| {
                format!(
                    "- [{}/{}] {}",
                    m.category,
                    m.kind,
                    trim_preview(&m.content.replace('\n', " "), 160)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "## Session-focused Consolidation Snapshot\n\
         ### Target Session ({})\n{}\n\n\
         ### Related Task Spines\n{}\n\n\
         ### Related Stored Memories\n{}\n",
        session_id, session_line, task_lines, memory_lines
    )
}

/// Phase 4c — event-triggered consolidation for a single session.
///
/// Public API (used in-process from session finalize hooks and when
/// the L2 compaction has run N times for a session). Runs a
/// **lightweight** consolidation bound to the specified session:
/// instead of the wide-snapshot nightly cron, the prompt only sees
/// this session's rolling summary, task spines, and session-anchored
/// memories.
///
/// The existing cron (`0 4 * * *`) is unchanged — this is additive.
///
/// Returns the underlying task id used for the synchronous-spawn run.
pub async fn trigger_consolidation_for_session(
    state: &AppState,
    session_id: &str,
) -> Result<String, String> {
    if session_id.trim().is_empty() {
        return Err("session_id is required".into());
    }
    let task = ensure_memory_consolidation_task_inner(state, None).await?;

    // Build the session-scoped snapshot and append it to the prompt
    // so the agent focuses on this session's residual information.
    let enriched_prompt = {
        let db = state.db.lock().await;
        let snapshot = build_session_consolidation_snapshot(&db, session_id);
        format!(
            "{}\n\n{}\n\n[triggered_by=session_finalize session_id={}]",
            snapshot, task.task_prompt, session_id
        )
    };

    {
        let db = state.db.lock().await;
        let _ = db.record_task_run(&task.id);
    }

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

    Ok(task.id)
}

/// Tauri command wrapping [`trigger_consolidation_for_session`] so
/// the frontend (or external integrations) can also fire a
/// session-scoped consolidation on demand.
#[tauri::command]
pub async fn trigger_memory_consolidation_for_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<String, String> {
    trigger_consolidation_for_session(&state, &session_id).await
}

pub(crate) async fn register_task_job(state: &AppState, task: &ScheduledTask) {
    unregister_task_job(state, &task.id).await;

    let app_h = state.app_handle.clone();
    let task_id = task.id.clone();
    let task_prompt_clone = task.task_prompt.clone();
    let db_arc = state.db.clone();
    let settings_arc = state.settings.clone();
    let browser = state.browser.clone();
    let cancel_flags = state.cancel_flags.clone();
    let cron = task.cron_expression.clone();
    let sched = state.scheduler.clone();
    let scheduled_job_ids = state.scheduled_job_ids.clone();
    let task_id_log = task.id.clone();

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
        Ok(job_id) => {
            let mut jobs = scheduled_job_ids.lock().await;
            jobs.insert(task.id.clone(), job_id);
            info!(
                "Scheduled task {} registered as job {}",
                task_id_log, job_id
            );
        }
        Err(e) => warn!(
            "Failed to register task {} in scheduler: {}",
            task_id_log, e
        ),
    }
}

pub(crate) async fn unregister_task_job(state: &AppState, task_id: &str) {
    let job_id = {
        let mut jobs = state.scheduled_job_ids.lock().await;
        jobs.remove(task_id)
    };
    if let Some(job_id) = job_id {
        if let Err(err) = state.scheduler.remove_job(job_id).await {
            warn!(
                "Failed to remove scheduled job {} for task {}: {}",
                job_id, task_id, err
            );
        } else {
            info!("Removed scheduled job {} for task {}", job_id, task_id);
        }
    }
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
                    None,
                    Some("active"),
                )
                .map_err(|e| e.to_string())?;
                db.get_task(&task.id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| {
                        "Memory consolidation task disappeared after update".to_string()
                    })?
            }
            None => db
                .create_task(
                    MEMORY_CONSOLIDATION_TASK_NAME,
                    Some(description),
                    cron,
                    &task_prompt,
                    None,
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
    notify_targets: Option<Vec<String>>,
) -> Result<ScheduledTask, String> {
    let parts: Vec<&str> = cron_expression.split_whitespace().collect();
    if parts.len() != 5 {
        return Err(format!(
            "Invalid cron expression '{}': must have 5 parts (minute hour day month weekday)",
            cron_expression
        ));
    }

    let notify_targets_json = match notify_targets {
        Some(tokens) if !tokens.is_empty() => {
            Some(serde_json::to_string(&tokens).map_err(|e| e.to_string())?)
        }
        _ => None,
    };

    let task = {
        let db = state.db.lock().await;
        db.create_task(
            &name,
            description.as_deref(),
            &cron_expression,
            &task_prompt,
            notify_targets_json.as_deref(),
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
    notify_targets: Option<Vec<String>>,
    status: Option<String>,
) -> Result<(), String> {
    let notify_targets_json = match notify_targets {
        Some(tokens) => Some(serde_json::to_string(&tokens).map_err(|e| e.to_string())?),
        None => None,
    };
    {
        let db = state.db.lock().await;
        db.update_task(
            &task_id,
            name.as_deref(),
            cron_expression.as_deref(),
            task_prompt.as_deref(),
            notify_targets_json.as_deref(),
            status.as_deref(),
        )
        .map_err(|e| e.to_string())?;
    }

    let task = {
        let db = state.db.lock().await;
        db.get_task(&task_id).map_err(|e| e.to_string())?
    };

    match task {
        Some(task) if task.status == "active" => register_task_job(&state, &task).await,
        Some(_) | None => unregister_task_job(&state, &task_id).await,
    }

    Ok(())
}

#[tauri::command]
pub async fn delete_task(state: State<'_, AppState>, task_id: String) -> Result<(), String> {
    unregister_task_job(&state, &task_id).await;
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
    let task_name = {
        let db = db.lock().await;
        db.get_task(&task_id).ok().flatten().map(|task| task.name)
    };
    let session_title = build_scheduler_session_title(task_name.as_deref(), &task_id);
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
        project_instruction_budget_chars,
        enable_project_instructions,
        pisci_personal_prompt,
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
            std::sync::Arc::new(pisci_kernel::agent::tool::ToolSettings::from_settings(&s)),
            s.max_iterations,
            s.builtin_tool_enabled.clone(),
            s.allow_outside_workspace,
            s.project_instruction_budget_chars,
            s.enable_project_instructions,
            s.pisci_personal_prompt.clone(),
        )
    };
    let effective_task_prompt = {
        let db_lock = db.lock().await;
        resolve_memory_consolidation_prompt(&task_prompt, &db_lock)
    };
    let task_notify_targets_section = {
        let db_lock = db.lock().await;
        match db_lock.get_scheduled_task_notify_targets(&task_id) {
            Ok(Some(raw)) if !raw.trim().is_empty() => match serde_json::from_str::<Vec<String>>(&raw)
            {
                Ok(tokens) if !tokens.is_empty() => format!(
                    "\n\nConfigured notify targets for this task: {:?}.\nPrefer using app_control(action=\"notify_user\", message=..., targets=<that exact target list>) for the default delivery path.\nIf you need raw IM output instead, use an im_binding target's binding_key with im_send_message.\nOnly fall back to im_channel_binding_lookup(task_id=...) when this configured target list is empty or insufficient.",
                    tokens
                ),
                _ => String::new(),
            },
            _ => String::new(),
        }
    };

    if api_key.is_empty() {
        warn!(
            "Scheduled task {}: API key not configured, skipping",
            task_id
        );
        let db_lock = db.lock().await;
        let _ = db_lock.update_task_run_status(&task_id, "error");
        let _ = app.emit(
            &format!("task_status_{}", task_id),
            serde_json::json!({ "status": "error", "error": "API key not configured" }),
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
    let registry = Arc::new(
        DesktopHostTools {
            browser: Some(browser),
            db: Some(db.clone()),
            settings: Some(settings.clone()),
            app_handle: Some(app.clone()),
            app_data_dir: app_data_dir_s,
            skill_loader: None,
            builtin_tool_enabled: Some(builtin_tool_enabled.clone()),
            user_tools_dir,
            ..DesktopHostTools::default()
        }
        .fill_pool_defaults()
        .build_registry(),
    );
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

    let personal_prompt_section = render_pisci_personal_prompt(&pisci_personal_prompt);
    let project_instruction_section = if enable_project_instructions {
        match render_project_instruction_context(
            std::path::Path::new(&workspace_root),
            project_instruction_budget_chars as usize,
        ) {
            Ok(content) => content,
            Err(error) => {
                warn!(
                    "Scheduled task {} failed to load project instructions: {}",
                    task_id, error
                );
                String::new()
            }
        }
    } else {
        String::new()
    };

    let system_prompt = format!(
        "You are Pisci, a Windows AI Agent running a scheduled task.\n\
         Task ID: {}\n\
         Today's date: {}\n\
         IM routing rule: if this task needs to notify a user through WeChat / WeCom / Feishu / other IM, do NOT guess a binding_key. First use `im_channel_list` to see which channel names are configured and connected. If the desired channel is configured but disconnected, use `im_channel_connect`. Then prefer `im_channel_binding_list(channel=..., task_id=...)` to list candidate tokens for that named channel; use `im_channel_binding_lookup(task_id=...)` or `im_channel_binding_lookup(session_id=...)` only when you already know the exact runtime context you want.\n\
         Example sequence for scheduled tasks:\n\
         1. `im_channel_list()`\n\
         2. if the required channel is disconnected: `im_channel_connect(channel=\"wechat\")`\n\
         3. `im_channel_binding_list(channel=\"wechat\", task_id=\"{0}\")`\n\
         4. `im_send_message(binding_key=\"<resolved binding_key>\", text=\"<short result summary>\")`\n\
         If step 3 returns no candidates, state that there is no bound IM target for that channel instead of pretending the message was delivered.{}{}{}{}",
        task_id,
        chrono::Utc::now().format("%Y-%m-%d"),
        personal_prompt_section,
        project_instruction_section,
        task_notify_targets_section,
        task_state_section
    );
    let scheduler_compaction_settings = {
        let s = settings.lock().await;
        pisci_kernel::agent::harness::config::CompactionSettings::from_settings(&s)
    };
    let agent = HarnessConfig::for_scheduler(
        model,
        vec![],
        registry,
        policy,
        system_prompt,
        max_tokens,
        0,
        None,
        100_000,
        scheduler_compaction_settings,
        db.clone(),
    )
    .into_agent_loop(client, None, None);

    let ctx = ToolContext {
        session_id: format!("sched_{}", task_id),
        workspace_root: std::path::PathBuf::from(&workspace_root),
        bypass_permissions: false,
        settings: tool_settings,
        max_iterations: Some(max_iterations),
        memory_owner_id: "pisci".to_string(),
        pool_session_id: None,
        cancel: cancel.clone(),
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
    let mut final_messages: Vec<LlmMessage> = Vec::new();
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
            Ok((msgs, _input_tokens, _output_tokens)) => {
                final_messages = msgs;
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
                    let _ = db_lock.update_task(&task_id, None, None, None, None, Some("error"));
                    break;
                }
                let backoff = std::time::Duration::from_secs(1 << (attempt - 1));
                tokio::time::sleep(backoff).await;
            }
        }
    }

    // Persist the agent conversation to the DB so the user can inspect
    // what actually happened (e.g. whether im_send_message was called,
    // whether it succeeded, and what the LLM said).
    let scope_id = format!("sched_{}", task_id);
    {
        let db_lock = db.lock().await;
        // Ensure a session exists for this scheduled task run.
        let _ = db_lock.ensure_fixed_session(&scope_id, &session_title, "scheduled_task");
        let _ = db_lock.rename_session(&scope_id, &session_title);
        // Append the full conversation (user prompt + agent replies).
        for msg in &final_messages {
            let text = match &msg.content {
                pisci_kernel::llm::MessageContent::Text(t) => t.clone(),
                pisci_kernel::llm::MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| {
                        if let pisci_kernel::llm::ContentBlock::Text { text } = b {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            if !text.is_empty() {
                let _ = db_lock.append_message(&scope_id, &msg.role, &text);
            }
        }
    }

    // Write final run status
    let scope_id = format!("sched_{}", task_id);

    // Log a summary of what the agent actually produced — this helps diagnose
    // silent failures where the agent ran but did not call im_send_message.
    let last_assistant_text = final_messages
        .iter()
        .rev()
        .filter(|m| m.role == "assistant")
        .find_map(|m| {
            let t = match &m.content {
                pisci_kernel::llm::MessageContent::Text(t) => Some(t.as_str()),
                pisci_kernel::llm::MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| {
                        if let pisci_kernel::llm::ContentBlock::Text { text } = b {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .next(),
            };
            t.filter(|s| !s.trim().is_empty())
        });
    if let Some(summary) = last_assistant_text {
        let preview: String = summary.chars().take(200).collect();
        info!("Scheduled task {} final agent output: {}", task_id, preview);
    } else {
        warn!(
            "Scheduled task {} produced no assistant text output — the agent may have failed silently or only used tools",
            task_id
        );
    }
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
