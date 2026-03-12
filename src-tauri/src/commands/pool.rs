/// Chat Pool commands — Agent collaboration chat room.
use crate::koi::{PoolMessage, PoolSession};
use crate::koi::runtime::KoiRuntime;
use crate::store::AppState;
use serde::Deserialize;
use serde_json::json;
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
    app: tauri::AppHandle,
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
    drop(db);

    let event_name = format!("pool_message_{}", input.session_id);
    let _ = state.app_handle.emit(&event_name, &msg);

    // Auto-detect @mention and dispatch to Koi asynchronously
    if input.content.contains('@') && input.sender_id != "system" {
        let app_clone = app.clone();
        let db_arc = state.db.clone();
        let sender = input.sender_id.clone();
        let pool_sid = input.session_id.clone();
        let content = input.content.clone();
        tokio::spawn(async move {
            let runtime = KoiRuntime::from_tauri(app_clone, db_arc);
            match runtime.handle_mention(&sender, &pool_sid, &content).await {
                Ok(results) if !results.is_empty() => {
                    tracing::info!(
                        "Auto @mention dispatch: {} Koi activated",
                        results.len()
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("Auto @mention dispatch failed: {}", e);
                }
            }
        });
    }

    Ok(msg)
}

#[tauri::command]
pub async fn get_pool_org_spec(
    state: State<'_, AppState>,
    id: String,
) -> Result<String, String> {
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
    db.update_pool_org_spec(&id, &org_spec).map_err(|e| e.to_string())
}

/// Dispatch a task to a Koi agent via the KoiRuntime.
/// This is the unified entry point for programmatic task assignment from the UI.
#[tauri::command]
pub async fn dispatch_koi_task(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    koi_id: String,
    task: String,
    pool_session_id: Option<String>,
    priority: Option<String>,
) -> Result<serde_json::Value, String> {
    let runtime = KoiRuntime::from_tauri(app, state.db.clone());
    let result = runtime.assign_and_execute(
        &koi_id,
        &task,
        "user",
        pool_session_id.as_deref(),
        priority.as_deref().unwrap_or("medium"),
    ).await.map_err(|e| e.to_string())?;

    Ok(json!({
        "success": result.success,
        "reply": result.reply,
        "result_message_id": result.result_message_id,
    }))
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
        return Err(format!("No active task found for Koi '{}' in pool '{}'", koi_id, psid));
    }

    // No pool_session_id provided: cancel all active tasks for this Koi across all projects
    let prefix = format!("koi_{}_", koi_id);
    let matching: Vec<_> = flags.keys()
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

/// Handle an @mention in a pool message, dispatching to the mentioned Koi.
#[tauri::command]
pub async fn handle_pool_mention(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    sender_id: String,
    pool_session_id: String,
    content: String,
) -> Result<Option<serde_json::Value>, String> {
    let runtime = KoiRuntime::from_tauri(app, state.db.clone());
    match runtime.handle_mention(&sender_id, &pool_session_id, &content).await {
        Ok(results) if !results.is_empty() => {
            let items: Vec<serde_json::Value> = results.iter().map(|r| json!({
                "success": r.success,
                "reply": r.reply,
                "result_message_id": r.result_message_id,
            })).collect();
            Ok(Some(json!({ "results": items })))
        }
        Ok(_) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
