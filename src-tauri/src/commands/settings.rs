use crate::store::settings::SshServerConfig;
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
pub async fn get_default_workspace() -> Result<String, String> {
    Ok(crate::store::settings::default_workspace_path())
}

#[tauri::command]
pub async fn save_settings(state: State<'_, AppState>, updates: Value) -> Result<Settings, String> {
    let mut settings = state.settings.lock().await;

    // LLM provider keys — only overwrite if the incoming value is non-empty.
    // An empty string means "unchanged" (the frontend may not send the decrypted key back).
    if let Some(v) = updates["anthropic_api_key"].as_str() {
        if !v.is_empty() {
            settings.anthropic_api_key = v.to_string();
        }
    }
    if let Some(v) = updates["openai_api_key"].as_str() {
        if !v.is_empty() {
            settings.openai_api_key = v.to_string();
        }
    }
    if let Some(v) = updates["deepseek_api_key"].as_str() {
        if !v.is_empty() {
            settings.deepseek_api_key = v.to_string();
        }
    }
    if let Some(v) = updates["qwen_api_key"].as_str() {
        if !v.is_empty() {
            settings.qwen_api_key = v.to_string();
        }
    }
    if let Some(v) = updates["minimax_api_key"].as_str() {
        if !v.is_empty() {
            settings.minimax_api_key = v.to_string();
        }
    }
    if let Some(v) = updates["zhipu_api_key"].as_str() {
        if !v.is_empty() {
            settings.zhipu_api_key = v.to_string();
        }
    }
    if let Some(v) = updates["kimi_api_key"].as_str() {
        if !v.is_empty() {
            settings.kimi_api_key = v.to_string();
        }
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
        // workspace_root must never be empty — fall back to default if blank
        let resolved = if v.trim().is_empty() {
            crate::store::settings::default_workspace_path()
        } else {
            v.to_string()
        };
        let _ = std::fs::create_dir_all(&resolved);
        settings.workspace_root = resolved;
    }
    if let Some(v) = updates["allow_outside_workspace"].as_bool() {
        settings.allow_outside_workspace = v;
    }
    if let Some(v) = updates["language"].as_str() {
        settings.language = v.to_string();
    }
    if let Some(v) = updates["max_tokens"].as_u64() {
        settings.max_tokens = v as u32;
    }
    if let Some(v) = updates["context_window"].as_u64() {
        settings.context_window = v as u32;
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
    if let Some(map) = updates["builtin_tool_enabled"].as_object() {
        settings.builtin_tool_enabled = map
            .iter()
            .filter_map(|(k, v)| v.as_bool().map(|b| (k.clone(), b)))
            .collect();
    }
    // Feishu
    if let Some(v) = updates["feishu_app_id"].as_str() {
        settings.feishu_app_id = v.to_string();
    }
    if let Some(v) = updates["feishu_app_secret"].as_str() {
        if !v.is_empty() {
            settings.feishu_app_secret = v.to_string();
        }
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
        if !v.is_empty() {
            settings.wecom_agent_secret = v.to_string();
        }
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
        if !v.is_empty() {
            settings.dingtalk_app_secret = v.to_string();
        }
    }
    if let Some(v) = updates["dingtalk_enabled"].as_bool() {
        settings.dingtalk_enabled = v;
    }
    // Telegram
    if let Some(v) = updates["telegram_bot_token"].as_str() {
        if !v.is_empty() {
            settings.telegram_bot_token = v.to_string();
        }
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
        if !v.is_empty() {
            settings.matrix_access_token = v.to_string();
        }
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
        if !v.is_empty() {
            settings.webhook_auth_token = v.to_string();
        }
    }
    if let Some(v) = updates["webhook_enabled"].as_bool() {
        settings.webhook_enabled = v;
    }
    // WeCom relay inbox
    if let Some(v) = updates["wecom_inbox_file"].as_str() {
        settings.wecom_inbox_file = v.to_string();
    }

    // Email (SMTP / IMAP)
    if let Some(v) = updates["smtp_host"].as_str() {
        settings.smtp_host = v.to_string();
    }
    if let Some(v) = updates["smtp_port"].as_u64() {
        settings.smtp_port = v as u16;
    }
    if let Some(v) = updates["smtp_username"].as_str() {
        settings.smtp_username = v.to_string();
    }
    if let Some(v) = updates["smtp_password"].as_str() {
        if !v.is_empty() {
            settings.smtp_password = v.to_string();
        }
    }
    if let Some(v) = updates["imap_host"].as_str() {
        settings.imap_host = v.to_string();
    }
    if let Some(v) = updates["imap_port"].as_u64() {
        settings.imap_port = v as u16;
    }
    if let Some(v) = updates["smtp_from_name"].as_str() {
        settings.smtp_from_name = v.to_string();
    }
    if let Some(v) = updates["email_enabled"].as_bool() {
        settings.email_enabled = v;
    }

    // Agent loop
    if let Some(v) = updates["max_iterations"].as_u64() {
        settings.max_iterations = v as u32;
    }
    if let Some(v) = updates["auto_compact_input_tokens_threshold"].as_u64() {
        settings.auto_compact_input_tokens_threshold = v as u32;
    }
    if let Some(v) = updates["project_instruction_budget_chars"].as_u64() {
        settings.project_instruction_budget_chars = v.max(512) as u32;
    }
    if let Some(v) = updates["enable_project_instructions"].as_bool() {
        settings.enable_project_instructions = v;
    }
    if let Some(v) = updates["llm_read_timeout_secs"].as_u64() {
        settings.llm_read_timeout_secs = v.max(30) as u32; // minimum 30s
    }
    if let Some(v) = updates["koi_timeout_secs"].as_u64() {
        settings.koi_timeout_secs = v.max(60) as u32; // minimum 60s
    }

    // Heartbeat
    if let Some(v) = updates["heartbeat_enabled"].as_bool() {
        settings.heartbeat_enabled = v;
    }
    if let Some(v) = updates["heartbeat_interval_mins"].as_u64() {
        settings.heartbeat_interval_mins = v as u32;
    }
    if let Some(v) = updates["heartbeat_prompt"].as_str() {
        settings.heartbeat_prompt = v.to_string();
    }

    // Vision / multimodal
    if let Some(v) = updates["vision_enabled"].as_bool() {
        settings.vision_enabled = v;
    }

    // SSH servers — full replacement when provided
    if let Some(arr) = updates["ssh_servers"].as_array() {
        let mut servers: Vec<SshServerConfig> = Vec::new();
        for item in arr {
            let id = item["id"].as_str().unwrap_or("").to_string();
            if id.is_empty() {
                continue;
            }
            let existing_password = settings
                .ssh_servers
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.password.clone())
                .unwrap_or_default();
            let existing_key = settings
                .ssh_servers
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.private_key.clone())
                .unwrap_or_default();
            servers.push(SshServerConfig {
                id,
                label: item["label"].as_str().unwrap_or("").to_string(),
                host: item["host"].as_str().unwrap_or("").to_string(),
                port: item["port"].as_u64().unwrap_or(22) as u16,
                username: item["username"].as_str().unwrap_or("").to_string(),
                // Only update password/key if non-empty (frontend may omit to keep existing)
                password: {
                    let v = item["password"].as_str().unwrap_or("");
                    if v.is_empty() {
                        existing_password
                    } else {
                        v.to_string()
                    }
                },
                private_key: {
                    let v = item["private_key"].as_str().unwrap_or("");
                    if v.is_empty() {
                        existing_key
                    } else {
                        v.to_string()
                    }
                },
            });
        }
        settings.ssh_servers = servers;
    }

    // Named LLM providers — full replacement when provided, preserving existing api_key if blank
    if let Some(arr) = updates["llm_providers"].as_array() {
        let mut providers: Vec<crate::store::settings::LlmProviderConfig> = Vec::new();
        for item in arr {
            let id = item["id"].as_str().unwrap_or("").to_string();
            if id.is_empty() {
                continue;
            }
            // Keep the existing api_key if the frontend sends an empty string
            // (frontend never sends back decrypted keys after initial load)
            let existing_api_key = settings
                .llm_providers
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.api_key.clone())
                .unwrap_or_default();
            providers.push(crate::store::settings::LlmProviderConfig {
                id,
                label: item["label"].as_str().unwrap_or("").to_string(),
                provider: item["provider"].as_str().unwrap_or("openai").to_string(),
                model: item["model"].as_str().unwrap_or("").to_string(),
                api_key: {
                    let v = item["api_key"].as_str().unwrap_or("");
                    if v.is_empty() {
                        existing_api_key
                    } else {
                        v.to_string()
                    }
                },
                base_url: item["base_url"].as_str().unwrap_or("").to_string(),
                max_tokens: item["max_tokens"].as_u64().unwrap_or(0) as u32,
            });
        }
        settings.llm_providers = providers;
    }

    // User tool configs
    if let Some(map) = updates["user_tool_configs"].as_object() {
        settings.user_tool_configs = map
            .iter()
            .filter_map(|(k, v)| {
                v.as_object()
                    .map(|obj| (k.clone(), serde_json::Value::Object(obj.clone())))
            })
            .collect();
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
