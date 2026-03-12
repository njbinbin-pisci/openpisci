use crate::store::AppState;
use tauri::State;

#[tauri::command]
pub async fn respond_interactive_ui(
    state: State<'_, AppState>,
    request_id: String,
    values: serde_json::Value,
) -> Result<(), String> {
    let mut map = state.interactive_responses.lock().await;
    if let Some(tx) = map.remove(&request_id) {
        let _ = tx.send(values);
        Ok(())
    } else {
        Err("Interactive UI request not found or expired".into())
    }
}
