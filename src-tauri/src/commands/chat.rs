use crate::agent::loop_::AgentLoop;
use crate::agent::messages::AgentEvent;
use crate::agent::tool::ToolContext;
use crate::llm::{build_client, LlmMessage, MessageContent};
use crate::policy::PolicyGate;
use crate::store::{db::ChatMessage, db::Session, AppState};
use crate::tools;
use serde::Serialize;
use std::sync::{atomic::AtomicBool, Arc};
use tauri::{AppHandle, Emitter, State};

#[derive(Debug, Serialize)]
pub struct SessionList {
    pub sessions: Vec<Session>,
    pub total: usize,
}

#[tauri::command]
pub async fn create_session(
    state: State<'_, AppState>,
    title: Option<String>,
) -> Result<Session, String> {
    let db = state.db.lock().await;
    db.create_session(title.as_deref()).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_sessions(
    state: State<'_, AppState>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<SessionList, String> {
    let db = state.db.lock().await;
    let sessions = db
        .list_sessions(limit.unwrap_or(20), offset.unwrap_or(0))
        .map_err(|e| e.to_string())?;
    let total = sessions.len();
    Ok(SessionList { sessions, total })
}

#[tauri::command]
pub async fn delete_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.delete_session(&session_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_messages(
    state: State<'_, AppState>,
    session_id: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<ChatMessage>, String> {
    let db = state.db.lock().await;
    db.get_messages(&session_id, limit.unwrap_or(100), offset.unwrap_or(0))
        .map_err(|e| e.to_string())
}

/// Send a user message and run the agent loop.
/// Streams AgentEvents to the frontend via Tauri events.
#[tauri::command]
pub async fn chat_send(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    content: String,
) -> Result<(), String> {
    // Load settings
    let (provider, model, api_key, base_url, workspace_root, max_tokens) = {
        let settings = state.settings.lock().await;
        (
            settings.provider.clone(),
            settings.model.clone(),
            settings.active_api_key().to_string(),
            settings.custom_base_url.clone(),
            settings.workspace_root.clone(),
            settings.max_tokens,
        )
    };

    if api_key.is_empty() {
        return Err("API key not configured. Please open Settings to configure your API key.".into());
    }

    // Save user message to DB
    {
        let db = state.db.lock().await;
        db.append_message(&session_id, "user", &content)
            .map_err(|e| e.to_string())?;
        db.update_session_status(&session_id, "running")
            .map_err(|e| e.to_string())?;
    }

    // Load message history for context
    let history = {
        let db = state.db.lock().await;
        db.get_messages(&session_id, 50, 0)
            .map_err(|e| e.to_string())?
    };

    // Convert DB messages to LLM messages
    let llm_messages: Vec<LlmMessage> = history
        .iter()
        .map(|m| LlmMessage {
            role: m.role.clone(),
            content: MessageContent::text(&m.content),
        })
        .collect();

    // Build cancellation token
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut flags = state.cancel_flags.lock().await;
        flags.insert(session_id.clone(), cancel.clone());
    }

    // Build agent components
    let client = build_client(
        &provider,
        &api_key,
        if base_url.is_empty() { None } else { Some(&base_url) },
    );

    let registry = Arc::new(tools::build_registry());

    let policy = Arc::new(PolicyGate::new(&workspace_root));

    let agent = AgentLoop {
        client,
        registry,
        policy,
        system_prompt: build_system_prompt(),
        model,
        max_tokens,
    };

    let ctx = ToolContext {
        session_id: session_id.clone(),
        workspace_root: std::path::PathBuf::from(&workspace_root),
        bypass_permissions: false,
    };

    // Create event channel
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);

    // Spawn event forwarding task
    let app_clone = app.clone();
    let session_id_clone = session_id.clone();
    let forward_handle = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let payload = serde_json::to_value(&event).unwrap_or_default();
            let _ = app_clone.emit(&format!("agent_event_{}", session_id_clone), payload);
        }
    });

    // Run agent loop
    let result = agent.run(llm_messages, event_tx, cancel.clone(), ctx).await;

    // Wait for event forwarding to complete
    let _ = forward_handle.await;

    // Clean up cancel flag
    {
        let mut flags = state.cancel_flags.lock().await;
        flags.remove(&session_id);
    }

    match result {
        Ok(final_messages) => {
            // Save assistant response to DB
            if let Some(last) = final_messages.last() {
                if last.role == "assistant" {
                    let text = last.content.as_text();
                    if !text.is_empty() {
                        let db = state.db.lock().await;
                        let _ = db.append_message(&session_id, "assistant", &text);
                    }
                }
            }
            let db = state.db.lock().await;
            let _ = db.update_session_status(&session_id, "idle");
            Ok(())
        }
        Err(e) => {
            let db = state.db.lock().await;
            let _ = db.update_session_status(&session_id, "idle");
            Err(e.to_string())
        }
    }
}

/// Cancel an in-progress agent run
#[tauri::command]
pub async fn chat_cancel(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
    let flags = state.cancel_flags.lock().await;
    if let Some(flag) = flags.get(&session_id) {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

fn build_system_prompt() -> String {
    format!(
        "You are Pisci, an AI assistant that can help with a wide range of tasks on Windows. \
         You have access to tools for file operations, shell commands, web search, \
         and Windows UI automation. \
         \n\nGuidelines:\
         \n- Be concise and helpful\
         \n- Use tools when needed to complete tasks\
         \n- Always confirm before destructive operations\
         \n- Respect the user's workspace boundaries\
         \n- Today's date: {}",
        chrono::Utc::now().format("%Y-%m-%d")
    )
}
