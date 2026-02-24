use crate::store::{AppState, Settings};
use serde_json::Value;
use tauri::State;

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    let settings = state.settings.lock().await;
    Ok(settings.clone())
}

#[tauri::command]
pub async fn save_settings(
    state: State<'_, AppState>,
    updates: Value,
) -> Result<Settings, String> {
    let mut settings = state.settings.lock().await;

    if let Some(v) = updates["anthropic_api_key"].as_str() {
        settings.anthropic_api_key = v.to_string();
    }
    if let Some(v) = updates["openai_api_key"].as_str() {
        settings.openai_api_key = v.to_string();
    }
    if let Some(v) = updates["provider"].as_str() {
        settings.provider = v.to_string();
    }
    if let Some(v) = updates["model"].as_str() {
        settings.model = v.to_string();
    }
    if let Some(v) = updates["custom_base_url"].as_str() {
        settings.custom_base_url = v.to_string();
    }
    if let Some(v) = updates["workspace_root"].as_str() {
        settings.workspace_root = v.to_string();
        // Ensure workspace directory exists
        let _ = std::fs::create_dir_all(v);
    }
    if let Some(v) = updates["language"].as_str() {
        settings.language = v.to_string();
    }
    if let Some(v) = updates["max_tokens"].as_u64() {
        settings.max_tokens = v as u32;
    }
    if let Some(v) = updates["confirm_shell_commands"].as_bool() {
        settings.confirm_shell_commands = v;
    }
    if let Some(v) = updates["confirm_file_writes"].as_bool() {
        settings.confirm_file_writes = v;
    }

    settings.save().map_err(|e| e.to_string())?;
    Ok(settings.clone())
}

#[tauri::command]
pub async fn is_configured(state: State<'_, AppState>) -> Result<bool, String> {
    let settings = state.settings.lock().await;
    Ok(settings.is_configured())
}
