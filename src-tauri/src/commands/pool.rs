//! Chat Pool commands — Agent collaboration chat room.
//!
//! State-change events funnel through [`DesktopEventSink::emit_pool`]
//! so desktop, kernel, and CLI hosts share one wire format (see
//! `host::DesktopEventSink` for the per-variant Tauri event name).

// Pool-domain submodules (registered as Tauri commands by `app::bootstrap`).
pub mod board;
pub mod koi;

use std::sync::Arc;

use crate::host::DesktopEventSink;
use crate::pool::bridge;
use crate::pool::{PoolMessage, PoolSession};
use crate::store::AppState;
use pisci_core::host::{PoolEvent, PoolEventSink, PoolMessageSnapshot, PoolSessionSnapshot};
use serde::Deserialize;
use serde_json::json;
use tauri::State;

fn pool_sink(app: &tauri::AppHandle) -> Arc<DesktopEventSink> {
    Arc::new(DesktopEventSink::new(app.clone()))
}

fn emit_message(app: &tauri::AppHandle, msg: &PoolMessage) {
    pool_sink(app).emit_pool(&PoolEvent::MessageAppended {
        pool_id: msg.pool_session_id.clone(),
        message: PoolMessageSnapshot::from(msg),
    });
}

fn emit_pool_status(app: &tauri::AppHandle, session: &PoolSession, archived: bool) {
    let snapshot = PoolSessionSnapshot::from(session);
    let event = match (archived, session.status.as_str()) {
        (true, _) => PoolEvent::PoolArchived {
            pool_id: session.id.clone(),
        },
        (false, "paused") => PoolEvent::PoolPaused { pool: snapshot },
        (false, "active") => PoolEvent::PoolResumed { pool: snapshot },
        (false, _) => PoolEvent::PoolUpdated { pool: snapshot },
    };
    pool_sink(app).emit_pool(&event);
}

pub(crate) fn ensure_pool_can_archive(db: &crate::store::Database, id: &str) -> Result<(), String> {
    let session = db
        .get_pool_session(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Pool '{}' not found", id))?;

    let active_todos: Vec<_> = db
        .list_koi_todos(None)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|todo| {
            todo.pool_session_id.as_deref() == Some(id)
                && !matches!(todo.status.as_str(), "done" | "cancelled")
        })
        .collect();

    if active_todos.is_empty() {
        return Ok(());
    }

    let todo_preview = active_todos
        .iter()
        .take(3)
        .map(|todo| format!("{} [{}]", &todo.id[..8.min(todo.id.len())], todo.status))
        .collect::<Vec<_>>()
        .join(", ");

    Err(format!(
        "Pool '{}' still has {} active todo(s): {}. Finish, block, or cancel them before archiving.",
        session.name,
        active_todos.len(),
        todo_preview
    ))
}

#[tauri::command]
pub async fn list_pool_sessions(state: State<'_, AppState>) -> Result<Vec<PoolSession>, String> {
    let db = state.db.lock().await;
    db.list_pool_sessions().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_pool_session(
    state: State<'_, AppState>,
    name: String,
    task_timeout_secs: Option<u32>,
) -> Result<PoolSession, String> {
    let db = state.db.lock().await;
    db.create_pool_session(&name, task_timeout_secs.unwrap_or(0))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_pool_session(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let db = state.db.lock().await;
    db.delete_pool_session(&id).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
pub struct GetPoolMessagesInput {
    pub session_id: String,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[tauri::command]
pub async fn get_pool_messages(
    state: State<'_, AppState>,
    input: GetPoolMessagesInput,
) -> Result<Vec<PoolMessage>, String> {
    let db = state.db.lock().await;
    db.get_pool_messages(
        &input.session_id,
        input.limit.unwrap_or(100),
        input.offset.unwrap_or(0),
    )
    .map_err(|e| e.to_string())
}

#[derive(Deserialize)]
pub struct SendPoolMessageInput {
    pub session_id: String,
    pub sender_id: String,
    pub content: String,
    pub msg_type: Option<String>,
    pub metadata: Option<String>,
}

#[tauri::command]
pub async fn send_pool_message(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: SendPoolMessageInput,
) -> Result<PoolMessage, String> {
    let db = state.db.lock().await;
    let msg = db
        .insert_pool_message(
            &input.session_id,
            &input.sender_id,
            &input.content,
            input.msg_type.as_deref().unwrap_or("text"),
            input.metadata.as_deref().unwrap_or("{}"),
        )
        .map_err(|e| e.to_string())?;
    drop(db);

    emit_message(&app, &msg);

    // Auto-detect @mention and dispatch to Koi asynchronously via the
    // kernel coordinator. The coordinator reuses the same PoolStore
    // (SQLite DB) and queues each mention as its own subprocess turn.
    if input.content.contains('@') && input.sender_id != "system" {
        let app_clone = app.clone();
        let db_arc = state.db.clone();
        let sender = input.sender_id.clone();
        let pool_sid = input.session_id.clone();
        let content = input.content.clone();
        tokio::spawn(async move {
            if let Err(e) =
                bridge::handle_mention_arc(&app_clone, db_arc, &sender, &pool_sid, &content).await
            {
                tracing::warn!("Auto @mention dispatch failed: {e}");
            }
        });
    }

    Ok(msg)
}

#[tauri::command]
pub async fn get_pool_org_spec(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let db = state.db.lock().await;
    let session = db.get_pool_session(&id).map_err(|e| e.to_string())?;
    Ok(session.map(|s| s.org_spec).unwrap_or_default())
}

#[tauri::command]
pub async fn update_pool_org_spec(
    state: State<'_, AppState>,
    id: String,
    org_spec: String,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.update_pool_org_spec(&id, &org_spec)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_pool_session_config(
    state: State<'_, AppState>,
    id: String,
    task_timeout_secs: Option<u32>,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.update_pool_session_config(&id, task_timeout_secs)
        .map_err(|e| e.to_string())
}

/// Dispatch a task to a Koi agent via the KoiRuntime.
/// This is the unified entry point for programmatic task assignment from the UI.
///
/// When a pool_session_id is provided, posts the task as a @mention message
/// in the pool and wakes the Koi to read and decide autonomously.
/// Without a pool, falls back to direct assign_and_execute (no pool context
/// for the agent to read).
#[tauri::command]
pub async fn dispatch_koi_task(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    koi_id: String,
    task: String,
    pool_session_id: Option<String>,
    priority: Option<String>,
    timeout_secs: Option<u32>,
) -> Result<serde_json::Value, String> {
    let priority = priority.as_deref().unwrap_or("medium");

    if let Some(ref psid) = pool_session_id {
        let koi_name = {
            let db = state.db.lock().await;
            db.resolve_koi_identifier(&koi_id)
                .ok()
                .flatten()
                .map(|k| k.name.clone())
                .unwrap_or_else(|| koi_id.clone())
        };

        let mention_content = if let Some(timeout_secs) = timeout_secs.filter(|v| *v > 0) {
            format!(
                "@{} [Priority: {}] [Execution timeout: {}s] {}",
                koi_name, priority, timeout_secs, task
            )
        } else {
            format!("@{} [Priority: {}] {}", koi_name, priority, task)
        };
        {
            let db = state.db.lock().await;
            let msg = db
                .insert_pool_message(
                    psid,
                    "user",
                    &mention_content,
                    "mention",
                    &json!({ "target_koi": &koi_id, "priority": priority, "timeout_secs": timeout_secs }).to_string(),
                )
                .map_err(|e| e.to_string())?;
            drop(db);
            emit_message(&app, &msg);
        }

        // Fan out via the kernel coordinator. Dispatch is async — the
        // subprocess turn streams events separately; we just confirm
        // the mention was accepted.
        bridge::handle_mention(&app, &state, "user", psid, &mention_content)
            .await
            .map_err(|e| e.to_string())?;

        Ok(json!({
            "success": true,
            "reply": format!("Task posted to pool. @{} has been notified.", koi_name),
            "result_message_id": null,
        }))
    } else {
        // Direct-assign path (no pool) — single subprocess turn via the
        // kernel coordinator's assign_and_execute entry point.
        let result = bridge::assign_and_execute(
            &app,
            &state,
            &koi_id,
            &task,
            "user",
            priority,
            timeout_secs,
        )
        .await
        .map_err(|e| e.to_string())?;

        Ok(json!({
            "success": result.success,
            "reply": result.reply,
            "result_message_id": result.result_message_id,
        }))
    }
}

/// Cancel a running Koi task by setting its cancel flag.
/// `pool_session_id` is required to identify which project's task to cancel,
/// since the same Koi can run concurrently in multiple projects.
/// Pass None / empty string to cancel any active task for this Koi (tries all known keys).
#[tauri::command]
pub async fn cancel_koi_task(
    state: State<'_, AppState>,
    koi_id: String,
    pool_session_id: Option<String>,
) -> Result<(), String> {
    let flags = state.cancel_flags.lock().await;

    if let Some(psid) = pool_session_id.as_deref().filter(|s| !s.is_empty()) {
        // Cancel the specific project's task
        let session_key = format!("koi_{}_{}", koi_id, psid);
        if let Some(flag) = flags.get(&session_key) {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
            tracing::info!("Cancel flag set for Koi session '{}'", session_key);
            return Ok(());
        }
        return Err(format!(
            "No active task found for Koi '{}' in pool '{}'",
            koi_id, psid
        ));
    }

    // No pool_session_id provided: cancel all active tasks for this Koi across all projects
    let prefix = format!("koi_{}_", koi_id);
    let matching: Vec<_> = flags
        .keys()
        .filter(|k| k.starts_with(&prefix))
        .cloned()
        .collect();
    if matching.is_empty() {
        return Err(format!("No active task found for Koi '{}'", koi_id));
    }
    for key in &matching {
        if let Some(flag) = flags.get(key) {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
            tracing::info!("Cancel flag set for Koi session '{}'", key);
        }
    }
    Ok(())
}

/// Pause an active project pool.
/// - Sets pool status to "paused"
/// - Cancels all running Koi tasks in this pool
/// - Resets in_progress todos back to "todo" so they can be resumed later
/// - Posts a system message in the pool chat
#[tauri::command]
pub async fn pause_pool_session(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    // 1. Update pool status
    {
        let db = state.db.lock().await;
        db.update_pool_session_status(&id, "paused")
            .map_err(|e| e.to_string())?;
    }

    // 2. Cancel all running Koi tasks in this pool
    {
        let flags = state.cancel_flags.lock().await;
        // cancel_flags keys for pool tasks are "koi_runtime_{koi_id}_{pool_id}"
        // and for call_koi path "koi_{koi_id}_{pool_id}"
        for (key, flag) in flags.iter() {
            if key.ends_with(&format!("_{}", id)) || key.contains(&id) {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
                tracing::info!("pause_pool_session: cancel flag set for '{}'", key);
            }
        }
    }

    // 3. Reset in_progress todos back to "todo"
    {
        let db = state.db.lock().await;
        let active_todos = db
            .list_active_todos_by_pool(&id)
            .map_err(|e| e.to_string())?;
        for todo in active_todos.iter().filter(|t| t.status == "in_progress") {
            let _ = db.update_koi_todo(&todo.id, None, None, Some("todo"), None);
        }
    }

    // 4. Post system message + refresh snapshot.
    let (session, sys_msg) = {
        let db = state.db.lock().await;
        let sys_msg = db
            .insert_pool_message(
                &id,
                "system",
                "⏸ 项目已被用户暂停。所有进行中的任务已重置为待办。",
                "status_update",
                "{}",
            )
            .ok();
        let session = db
            .get_pool_session(&id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Pool '{}' not found after pause", id))?;
        (session, sys_msg)
    };

    if let Some(msg) = sys_msg.as_ref() {
        emit_message(&app, msg);
    }
    emit_pool_status(&app, &session, false);
    Ok(())
}

/// Resume a paused or archived project pool.
/// - Sets pool status back to "active"
/// - Posts a system message and @pisci to re-engage coordination
#[tauri::command]
pub async fn resume_pool_session(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    // 1. Update pool status
    {
        let db = state.db.lock().await;
        db.update_pool_session_status(&id, "active")
            .map_err(|e| e.to_string())?;
    }

    // 2. Post system message + @pisci to re-engage
    let resume_msg = "▶ 项目已被用户恢复。@pisci 请检查待办任务并继续协调。".to_string();
    let (session, sys_msg) = {
        let db = state.db.lock().await;
        let msg = db
            .insert_pool_message(&id, "system", &resume_msg, "status_update", "{}")
            .map_err(|e| e.to_string())?;
        let session = db
            .get_pool_session(&id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Pool '{}' not found after resume", id))?;
        (session, msg)
    };

    emit_message(&app, &sys_msg);

    // 3. Trigger @pisci mention so Pisci wakes up and resumes coordination
    let app_clone = app.clone();
    let db_arc = state.db.clone();
    let pool_id = id.clone();
    tokio::spawn(async move {
        if let Err(e) =
            bridge::handle_mention_arc(&app_clone, db_arc, "system", &pool_id, &resume_msg).await
        {
            tracing::warn!("resume_pool_session mention dispatch failed: {e}");
        }
    });

    emit_pool_status(&app, &session, false);
    Ok(())
}

/// Archive a project pool (read-only, no new tasks).
/// - Cancels all running Koi tasks
/// - Sets pool status to "archived"
/// - Posts a system message
#[tauri::command]
pub async fn archive_pool_session(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    {
        let db = state.db.lock().await;
        ensure_pool_can_archive(&db, &id)?;
    }

    // 1. Cancel all running tasks
    {
        let flags = state.cancel_flags.lock().await;
        for (key, flag) in flags.iter() {
            if key.ends_with(&format!("_{}", id)) || key.contains(&id) {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    // 2. Update pool status
    {
        let db = state.db.lock().await;
        db.update_pool_session_status(&id, "archived")
            .map_err(|e| e.to_string())?;
    }

    // 3. Post system message + refresh snapshot.
    let (session, sys_msg) = {
        let db = state.db.lock().await;
        let msg = db
            .insert_pool_message(
                &id,
                "system",
                "🗄 项目已归档。项目进入只读状态，Koi 不再接受新任务。",
                "status_update",
                "{}",
            )
            .ok();
        let session = db
            .get_pool_session(&id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Pool '{}' not found after archive", id))?;
        (session, msg)
    };

    if let Some(msg) = sys_msg.as_ref() {
        emit_message(&app, msg);
    }
    emit_pool_status(&app, &session, true);
    Ok(())
}

/// Handle an @mention in a pool message, dispatching to the mentioned
/// Koi via the kernel coordinator. Returns immediately once dispatch is
/// accepted — the actual Koi turns stream back through `pool_event_*`
/// Tauri events as their subprocesses progress.
#[tauri::command]
pub async fn handle_pool_mention(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    sender_id: String,
    pool_session_id: String,
    content: String,
) -> Result<Option<serde_json::Value>, String> {
    match bridge::handle_mention(&app, &state, &sender_id, &pool_session_id, &content).await {
        Ok(()) => Ok(Some(json!({ "dispatched": true }))),
        Err(e) => Err(e.to_string()),
    }
}
