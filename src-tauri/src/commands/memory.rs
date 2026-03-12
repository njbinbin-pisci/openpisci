use crate::store::{db::Memory, AppState};
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct MemoryList {
    pub memories: Vec<Memory>,
    pub total: usize,
}

#[tauri::command]
pub async fn list_memories(state: State<'_, AppState>) -> Result<MemoryList, String> {
    let db = state.db.lock().await;
    let memories = db.list_memories().map_err(|e| e.to_string())?;
    let total = memories.len();
    Ok(MemoryList { memories, total })
}

#[tauri::command]
pub async fn add_memory(
    state: State<'_, AppState>,
    content: String,
    category: Option<String>,
    confidence: Option<f64>,
) -> Result<Memory, String> {
    let db = state.db.lock().await;
    db.save_memory(
        &content,
        category.as_deref().unwrap_or("general"),
        confidence.unwrap_or(0.8),
        None,
        "pisci",
        "private",
        "pisci",
        None,  // cross-project memory (no project tag)
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_memory(
    state: State<'_, AppState>,
    memory_id: String,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.delete_memory(&memory_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn clear_memories(state: State<'_, AppState>) -> Result<(), String> {
    let db = state.db.lock().await;
    db.clear_memories().map_err(|e| e.to_string())
}
