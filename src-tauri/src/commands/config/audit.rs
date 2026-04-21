use crate::store::{db::AuditEntry, AppState};
use tauri::State;

#[tauri::command]
pub async fn get_audit_log(
    state: State<'_, AppState>,
    session_id: Option<String>,
    tool_name: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<AuditEntry>, String> {
    let db = state.db.lock().await;
    db.get_audit_log(
        session_id.as_deref(),
        tool_name.as_deref(),
        limit.unwrap_or(50),
        offset.unwrap_or(0),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn clear_audit_log(
    state: State<'_, AppState>,
    session_id: Option<String>,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.clear_audit_log(session_id.as_deref())
        .map_err(|e| e.to_string())
}
