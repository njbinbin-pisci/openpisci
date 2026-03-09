/// Board commands — Kanban board for Koi todo management.
use crate::koi::KoiTodo;
use crate::store::AppState;
use serde::Deserialize;
use tauri::{Emitter, State};

#[tauri::command]
pub async fn list_koi_todos(
    state: State<'_, AppState>,
    owner_id: Option<String>,
) -> Result<Vec<KoiTodo>, String> {
    let db = state.db.lock().await;
    db.list_koi_todos(owner_id.as_deref()).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
pub struct CreateKoiTodoInput {
    pub owner_id: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub assigned_by: Option<String>,
    pub pool_session_id: Option<String>,
}

#[tauri::command]
pub async fn create_koi_todo(
    state: State<'_, AppState>,
    input: CreateKoiTodoInput,
) -> Result<KoiTodo, String> {
    let db = state.db.lock().await;
    let todo = db.create_koi_todo(
        &input.owner_id,
        &input.title,
        input.description.as_deref().unwrap_or(""),
        input.priority.as_deref().unwrap_or("medium"),
        input.assigned_by.as_deref().unwrap_or("user"),
        input.pool_session_id.as_deref(),
    ).map_err(|e| e.to_string())?;

    let _ = state.app_handle.emit("koi_todo_updated", &todo);
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
    ).map_err(|e| e.to_string())?;

    let _ = state.app_handle.emit("koi_todo_updated", &input.id);
    Ok(())
}

#[tauri::command]
pub async fn delete_koi_todo(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.delete_koi_todo(&id).map_err(|e| e.to_string())?;

    let _ = state.app_handle.emit("koi_todo_updated", &id);
    Ok(())
}
