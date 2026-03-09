/// Chat Pool commands — Agent collaboration chat room.
use crate::koi::{PoolMessage, PoolSession};
use crate::store::AppState;
use serde::Deserialize;
use tauri::{Emitter, State};

#[tauri::command]
pub async fn list_pool_sessions(state: State<'_, AppState>) -> Result<Vec<PoolSession>, String> {
    let db = state.db.lock().await;
    db.list_pool_sessions().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_pool_session(state: State<'_, AppState>, name: String) -> Result<PoolSession, String> {
    let db = state.db.lock().await;
    db.create_pool_session(&name).map_err(|e| e.to_string())
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
    ).map_err(|e| e.to_string())
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
    state: State<'_, AppState>,
    input: SendPoolMessageInput,
) -> Result<PoolMessage, String> {
    let db = state.db.lock().await;
    let msg = db.insert_pool_message(
        &input.session_id,
        &input.sender_id,
        &input.content,
        input.msg_type.as_deref().unwrap_or("text"),
        input.metadata.as_deref().unwrap_or("{}"),
    ).map_err(|e| e.to_string())?;

    let event_name = format!("pool_message_{}", input.session_id);
    let _ = state.app_handle.emit(&event_name, &msg);

    Ok(msg)
}
