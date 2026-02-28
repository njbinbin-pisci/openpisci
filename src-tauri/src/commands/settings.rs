use crate::store::{AppState, Settings};
use serde_json::Value;
use tauri::State;
use tracing::info;

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

    // LLM provider keys
    if let Some(v) = updates["anthropic_api_key"].as_str() {
        settings.anthropic_api_key = v.to_string();
    }
    if let Some(v) = updates["openai_api_key"].as_str() {
        settings.openai_api_key = v.to_string();
    }
    if let Some(v) = updates["deepseek_api_key"].as_str() {
        settings.deepseek_api_key = v.to_string();
    }
    if let Some(v) = updates["qwen_api_key"].as_str() {
        settings.qwen_api_key = v.to_string();
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
    if let Some(v) = updates["browser_headless"].as_bool() {
        settings.browser_headless = v;
    }
    if let Some(v) = updates["policy_mode"].as_str() {
        settings.policy_mode = v.to_string();
    }
    if let Some(v) = updates["tool_rate_limit_per_minute"].as_u64() {
        settings.tool_rate_limit_per_minute = v as u32;
    }
    // Feishu
    if let Some(v) = updates["feishu_app_id"].as_str() {
        settings.feishu_app_id = v.to_string();
    }
    if let Some(v) = updates["feishu_app_secret"].as_str() {
        settings.feishu_app_secret = v.to_string();
    }
    if let Some(v) = updates["feishu_domain"].as_str() {
        settings.feishu_domain = v.to_string();
    }
    if let Some(v) = updates["feishu_enabled"].as_bool() {
        settings.feishu_enabled = v;
    }
    // WeCom
    if let Some(v) = updates["wecom_corp_id"].as_str() {
        settings.wecom_corp_id = v.to_string();
    }
    if let Some(v) = updates["wecom_agent_secret"].as_str() {
        settings.wecom_agent_secret = v.to_string();
    }
    if let Some(v) = updates["wecom_agent_id"].as_str() {
        settings.wecom_agent_id = v.to_string();
    }
    if let Some(v) = updates["wecom_enabled"].as_bool() {
        settings.wecom_enabled = v;
    }
    // DingTalk
    if let Some(v) = updates["dingtalk_app_key"].as_str() {
        settings.dingtalk_app_key = v.to_string();
    }
    if let Some(v) = updates["dingtalk_app_secret"].as_str() {
        settings.dingtalk_app_secret = v.to_string();
    }
    if let Some(v) = updates["dingtalk_enabled"].as_bool() {
        settings.dingtalk_enabled = v;
    }
    // Telegram
    if let Some(v) = updates["telegram_bot_token"].as_str() {
        settings.telegram_bot_token = v.to_string();
    }
    if let Some(v) = updates["telegram_enabled"].as_bool() {
        settings.telegram_enabled = v;
    }
    // Slack
    if let Some(v) = updates["slack_webhook_url"].as_str() {
        settings.slack_webhook_url = v.to_string();
    }
    if let Some(v) = updates["slack_enabled"].as_bool() {
        settings.slack_enabled = v;
    }
    // Discord
    if let Some(v) = updates["discord_webhook_url"].as_str() {
        settings.discord_webhook_url = v.to_string();
    }
    if let Some(v) = updates["discord_enabled"].as_bool() {
        settings.discord_enabled = v;
    }
    // Teams
    if let Some(v) = updates["teams_webhook_url"].as_str() {
        settings.teams_webhook_url = v.to_string();
    }
    if let Some(v) = updates["teams_enabled"].as_bool() {
        settings.teams_enabled = v;
    }
    // Matrix
    if let Some(v) = updates["matrix_homeserver"].as_str() {
        settings.matrix_homeserver = v.to_string();
    }
    if let Some(v) = updates["matrix_access_token"].as_str() {
        settings.matrix_access_token = v.to_string();
    }
    if let Some(v) = updates["matrix_room_id"].as_str() {
        settings.matrix_room_id = v.to_string();
    }
    if let Some(v) = updates["matrix_enabled"].as_bool() {
        settings.matrix_enabled = v;
    }
    // Generic webhook
    if let Some(v) = updates["webhook_outbound_url"].as_str() {
        settings.webhook_outbound_url = v.to_string();
    }
    if let Some(v) = updates["webhook_auth_token"].as_str() {
        settings.webhook_auth_token = v.to_string();
    }
    if let Some(v) = updates["webhook_enabled"].as_bool() {
        settings.webhook_enabled = v;
    }
    // WeCom relay inbox
    if let Some(v) = updates["wecom_inbox_file"].as_str() {
        settings.wecom_inbox_file = v.to_string();
    }

    let headless = settings.browser_headless;
    settings.save().map_err(|e| e.to_string())?;
    let saved = settings.clone();
    drop(settings); // release lock before touching browser

    // Sync headless mode to browser manager (takes effect on next browser launch)
    {
        let mut mgr = state.browser.lock().await;
        let current = mgr.headless();
        if current != headless {
            info!("Browser headless mode changed: {} -> {}", current, headless);
            if mgr.is_running() {
                mgr.close().await;
            }
            mgr.set_headless(headless);
        }
    }

    Ok(saved)
}

#[tauri::command]
pub async fn is_configured(state: State<'_, AppState>) -> Result<bool, String> {
    let settings = state.settings.lock().await;
    Ok(settings.is_configured())
}
