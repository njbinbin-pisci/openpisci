/// Board commands — Kanban board for Koi todo management.
use crate::koi::runtime::KoiRuntime;
use crate::koi::KoiTodo;
use crate::store::AppState;
use serde::Deserialize;
use serde_json::json;
use tauri::{Emitter, State};

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
    )
    .map_err(|e| e.to_string())?;

    let _ = state.app_handle.emit("koi_todo_updated", &input.id);
    Ok(())
}

#[tauri::command]
pub async fn claim_koi_todo(
    state: State<'_, AppState>,
    id: String,
    claimed_by: String,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.claim_koi_todo(&id, &claimed_by)
        .map_err(|e| e.to_string())?;
    let _ = state.app_handle.emit(
        "koi_todo_updated",
        json!({ "id": id, "action": "claimed", "claimed_by": claimed_by }),
    );
    Ok(())
}

#[tauri::command]
pub async fn complete_koi_todo(
    state: State<'_, AppState>,
    id: String,
    result_message_id: Option<i64>,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.complete_koi_todo(&id, result_message_id)
        .map_err(|e| e.to_string())?;
    let _ = state.app_handle.emit(
        "koi_todo_updated",
        json!({ "id": id, "action": "completed" }),
    );
    Ok(())
}

#[tauri::command]
pub async fn resume_koi_todo(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let runtime = KoiRuntime::from_tauri(app, state.db.clone());
    runtime.resume_todo(&id, "user").await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_koi_todo(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let db = state.db.lock().await;
    db.delete_koi_todo(&id).map_err(|e| e.to_string())?;

    let _ = state.app_handle.emit("koi_todo_updated", &id);
    Ok(())
}
