use crate::store::AppState;
use tauri::State;

#[tauri::command]
pub async fn respond_permission(
    state: State<'_, AppState>,
    request_id: String,
    approved: bool,
) -> Result<(), String> {
    let mut map = state.confirmation_responses.lock().await;
    if let Some(tx) = map.remove(&request_id) {
        let _ = tx.send(approved);
        Ok(())
    } else {
        Err("Permission request not found or expired".into())
    }
}
