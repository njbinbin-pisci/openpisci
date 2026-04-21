//! Board commands — Kanban board for Koi todo management.
//!
//! These Tauri commands remain thin wrappers around the persistence
//! layer for UI-initiated todo mutations. All event emissions go through
//! [`DesktopEventSink::emit_pool`] so desktop, CLI, and kernel sources
//! share a single [`PoolEvent`]-shaped wire format (see
//! `host::DesktopEventSink`'s impl for the per-variant Tauri event name).

use std::sync::Arc;

use pisci_core::host::{PoolEvent, PoolEventSink, TodoChangeAction, TodoSnapshot};
use serde::Deserialize;
use tauri::State;

use crate::host::DesktopEventSink;
use crate::pool::bridge;
use crate::pool::KoiTodo;
use crate::store::AppState;

fn pool_sink(app: &tauri::AppHandle) -> Arc<DesktopEventSink> {
    Arc::new(DesktopEventSink::new(app.clone()))
}

fn emit_todo(app: &tauri::AppHandle, action: TodoChangeAction, todo: &KoiTodo) {
    let pool_id = todo.pool_session_id.clone().unwrap_or_default();
    pool_sink(app).emit_pool(&PoolEvent::TodoChanged {
        pool_id,
        action,
        todo: TodoSnapshot::from(todo),
    });
}

#[tauri::command]
pub async fn list_koi_todos(
    state: State<'_, AppState>,
    owner_id: Option<String>,
) -> Result<Vec<KoiTodo>, String> {
    let db = state.db.lock().await;
    db.list_koi_todos(owner_id.as_deref())
        .map_err(|e| e.to_string())
}

#[derive(Deserialize)]
pub struct CreateKoiTodoInput {
    pub owner_id: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub assigned_by: Option<String>,
    pub pool_session_id: Option<String>,
    pub source_type: Option<String>,
    pub depends_on: Option<String>,
    pub task_timeout_secs: Option<u32>,
}

#[tauri::command]
pub async fn create_koi_todo(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: CreateKoiTodoInput,
) -> Result<KoiTodo, String> {
    let db = state.db.lock().await;
    let todo = db
        .create_koi_todo(
            &input.owner_id,
            &input.title,
            input.description.as_deref().unwrap_or(""),
            input.priority.as_deref().unwrap_or("medium"),
            input.assigned_by.as_deref().unwrap_or("user"),
            input.pool_session_id.as_deref(),
            input.source_type.as_deref().unwrap_or("user"),
            input.depends_on.as_deref(),
            input.task_timeout_secs.unwrap_or(0),
        )
        .map_err(|e| e.to_string())?;
    drop(db);

    emit_todo(&app, TodoChangeAction::Created, &todo);
    Ok(todo)
}

#[derive(Deserialize)]
pub struct UpdateKoiTodoInput {
    pub id: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
}

#[tauri::command]
pub async fn update_koi_todo(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: UpdateKoiTodoInput,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.update_koi_todo(
        &input.id,
        input.title.as_deref(),
        input.description.as_deref(),
        input.status.as_deref(),
        input.priority.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    // Re-read so the emitted snapshot reflects the latest state.
    let todo = db.get_koi_todo(&input.id).map_err(|e| e.to_string())?;
    drop(db);

    if let Some(todo) = todo {
        emit_todo(&app, TodoChangeAction::Updated, &todo);
    }
    Ok(())
}

#[tauri::command]
pub async fn claim_koi_todo(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
    claimed_by: String,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.claim_koi_todo(&id, &claimed_by)
        .map_err(|e| e.to_string())?;
    let todo = db.get_koi_todo(&id).map_err(|e| e.to_string())?;
    drop(db);
    if let Some(todo) = todo {
        emit_todo(&app, TodoChangeAction::Claimed, &todo);
    }
    Ok(())
}

#[tauri::command]
pub async fn complete_koi_todo(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
    result_message_id: Option<i64>,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.complete_koi_todo(&id, result_message_id)
        .map_err(|e| e.to_string())?;
    let todo = db.get_koi_todo(&id).map_err(|e| e.to_string())?;
    drop(db);
    if let Some(todo) = todo {
        emit_todo(&app, TodoChangeAction::Completed, &todo);
    }
    Ok(())
}

#[tauri::command]
pub async fn resume_koi_todo(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    bridge::resume_todo(&app, &state, &id, "user")
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_koi_todo(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    // Read the todo once so the delete-event snapshot is accurate —
    // after `delete_koi_todo` the row is gone and we can only emit an
    // id.
    let snapshot = {
        let db = state.db.lock().await;
        db.get_koi_todo(&id).map_err(|e| e.to_string())?
    };
    {
        let db = state.db.lock().await;
        db.delete_koi_todo(&id).map_err(|e| e.to_string())?;
    }

    if let Some(todo) = snapshot {
        emit_todo(&app, TodoChangeAction::Deleted, &todo);
    }
    Ok(())
}
