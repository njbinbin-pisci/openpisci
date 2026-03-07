use crate::gateway::{
    dingtalk::{DingtalkChannel, DingtalkConfig},
    discord::{DiscordChannel, DiscordConfig},
    feishu::{FeishuChannel, FeishuConfig},
    matrix::{MatrixChannel, MatrixConfig},
    slack::{SlackChannel, SlackConfig},
    telegram::{TelegramChannel, TelegramConfig},
    teams::{TeamsChannel, TeamsConfig},
    webhook::{WebhookChannel, WebhookConfig},
    ChannelInfo,
};
use crate::store::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Serialize, Deserialize)]
pub struct GatewayStatus {
    pub channels: Vec<ChannelInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GatewayDiagnosticItem {
    pub channel: String,
    pub enabled: bool,
    pub configured: bool,
    pub message: String,
}

/// 列出所有已注册的 IM 渠道及其状态
#[tauri::command]
pub async fn list_gateway_channels(state: State<'_, AppState>) -> Result<GatewayStatus, String> {
    let channels = state.gateway.list_channels().await;
    Ok(GatewayStatus { channels })
}

/// 根据当前 Settings 中的 IM 配置，连接启用的渠道。
/// 每次调用前先 shutdown 所有已有渠道，避免重复监听任务。
#[tauri::command]
pub async fn connect_gateway_channels(state: State<'_, AppState>) -> Result<GatewayStatus, String> {
    // Stop any existing listeners before re-registering to prevent duplicate tasks
    let _ = state.gateway.stop_all().await;

    let settings = state.settings.lock().await.clone();

    // 飞书
    if settings.feishu_enabled && !settings.feishu_app_id.is_empty() {
        let config = FeishuConfig {
            app_id: settings.feishu_app_id.clone(),
            app_secret: settings.feishu_app_secret.clone(),
            domain: settings.feishu_domain.clone(),
        };
        let ch = Box::new(FeishuChannel::new(config));
        state.gateway.register_channel(ch).await;
    }

    // 钉钉
    if settings.dingtalk_enabled && !settings.dingtalk_app_key.is_empty() {
        let config = DingtalkConfig {
            app_key: settings.dingtalk_app_key.clone(),
            app_secret: settings.dingtalk_app_secret.clone(),
            robot_code: None,
        };
        let ch = Box::new(DingtalkChannel::new(config));
        state.gateway.register_channel(ch).await;
    }

    // Telegram
    if settings.telegram_enabled && !settings.telegram_bot_token.is_empty() {
        let config = TelegramConfig { bot_token: settings.telegram_bot_token.clone() };
        let ch = Box::new(TelegramChannel::new(config));
        state.gateway.register_channel(ch).await;
    }

    // Slack (incoming webhook, outbound)
    if settings.slack_enabled && !settings.slack_webhook_url.is_empty() {
        let config = SlackConfig {
            webhook_url: settings.slack_webhook_url.clone(),
        };
        let ch = Box::new(SlackChannel::new(config));
        state.gateway.register_channel(ch).await;
    }

    // Discord (webhook, outbound)
    if settings.discord_enabled && !settings.discord_webhook_url.is_empty() {
        let config = DiscordConfig {
            webhook_url: settings.discord_webhook_url.clone(),
        };
        let ch = Box::new(DiscordChannel::new(config));
        state.gateway.register_channel(ch).await;
    }

    // Teams (incoming webhook, outbound)
    if settings.teams_enabled && !settings.teams_webhook_url.is_empty() {
        let config = TeamsConfig {
            webhook_url: settings.teams_webhook_url.clone(),
        };
        let ch = Box::new(TeamsChannel::new(config));
        state.gateway.register_channel(ch).await;
    }

    // Matrix (room send)
    if settings.matrix_enabled
        && !settings.matrix_homeserver.is_empty()
        && !settings.matrix_access_token.is_empty()
        && !settings.matrix_room_id.is_empty()
    {
        let config = MatrixConfig {
            homeserver: settings.matrix_homeserver.clone(),
            access_token: settings.matrix_access_token.clone(),
            room_id: settings.matrix_room_id.clone(),
        };
        let ch = Box::new(MatrixChannel::new(config));
        state.gateway.register_channel(ch).await;
    }

    // Generic outbound webhook
    if settings.webhook_enabled && !settings.webhook_outbound_url.is_empty() {
        let config = WebhookConfig {
            outbound_url: settings.webhook_outbound_url.clone(),
            bearer_token: if settings.webhook_auth_token.is_empty() {
                None
            } else {
                Some(settings.webhook_auth_token.clone())
            },
        };
        let ch = Box::new(WebhookChannel::new(config));
        state.gateway.register_channel(ch).await;
    }

    // 企业微信
    if settings.wecom_enabled && !settings.wecom_corp_id.is_empty() {
        let config = crate::gateway::wecom::WecomConfig {
            corp_id: settings.wecom_corp_id.clone(),
            agent_secret: settings.wecom_agent_secret.clone(),
            agent_id: settings.wecom_agent_id.clone(),
            inbox_file: if settings.wecom_inbox_file.is_empty() {
                None
            } else {
                Some(settings.wecom_inbox_file.clone())
            },
        };
        let ch = Box::new(crate::gateway::wecom::WecomChannel::new(config));
        state.gateway.register_channel(ch).await;
    }

    // 启动所有已注册渠道
    state.gateway.start_all().await.map_err(|e| e.to_string())?;

    let channels = state.gateway.list_channels().await;
    Ok(GatewayStatus { channels })
}

/// 断开所有 IM 渠道
#[tauri::command]
pub async fn disconnect_gateway_channels(state: State<'_, AppState>) -> Result<(), String> {
    state.gateway.stop_all().await.map_err(|e| e.to_string())
}

/// Return per-channel config diagnostics before connect.
#[tauri::command]
pub async fn diagnose_gateway_channels(state: State<'_, AppState>) -> Result<Vec<GatewayDiagnosticItem>, String> {
    let s = state.settings.lock().await.clone();
    let items = vec![
        GatewayDiagnosticItem {
            channel: "telegram".into(),
            enabled: s.telegram_enabled,
            configured: !s.telegram_bot_token.is_empty(),
            message: if s.telegram_bot_token.is_empty() { "missing telegram_bot_token".into() } else { "ok".into() },
        },
        GatewayDiagnosticItem {
            channel: "feishu".into(),
            enabled: s.feishu_enabled,
            configured: !s.feishu_app_id.is_empty() && !s.feishu_app_secret.is_empty(),
            message: if s.feishu_app_id.is_empty() || s.feishu_app_secret.is_empty() { "missing feishu app credentials".into() } else { "ok".into() },
        },
        GatewayDiagnosticItem {
            channel: "dingtalk".into(),
            enabled: s.dingtalk_enabled,
            configured: !s.dingtalk_app_key.is_empty() && !s.dingtalk_app_secret.is_empty(),
            message: if s.dingtalk_app_key.is_empty() || s.dingtalk_app_secret.is_empty() { "missing dingtalk app credentials".into() } else { "ok".into() },
        },
        GatewayDiagnosticItem {
            channel: "wecom".into(),
            enabled: s.wecom_enabled,
            configured: !s.wecom_corp_id.is_empty() && !s.wecom_agent_secret.is_empty() && !s.wecom_agent_id.is_empty(),
            message: if s.wecom_corp_id.is_empty() || s.wecom_agent_secret.is_empty() || s.wecom_agent_id.is_empty() { "missing wecom app credentials".into() } else { "ok".into() },
        },
        GatewayDiagnosticItem {
            channel: "slack".into(),
            enabled: s.slack_enabled,
            configured: !s.slack_webhook_url.is_empty(),
            message: if s.slack_webhook_url.is_empty() { "missing slack_webhook_url".into() } else { "ok".into() },
        },
        GatewayDiagnosticItem {
            channel: "discord".into(),
            enabled: s.discord_enabled,
            configured: !s.discord_webhook_url.is_empty(),
            message: if s.discord_webhook_url.is_empty() { "missing discord_webhook_url".into() } else { "ok".into() },
        },
        GatewayDiagnosticItem {
            channel: "teams".into(),
            enabled: s.teams_enabled,
            configured: !s.teams_webhook_url.is_empty(),
            message: if s.teams_webhook_url.is_empty() { "missing teams_webhook_url".into() } else { "ok".into() },
        },
        GatewayDiagnosticItem {
            channel: "matrix".into(),
            enabled: s.matrix_enabled,
            configured: !s.matrix_homeserver.is_empty() && !s.matrix_access_token.is_empty() && !s.matrix_room_id.is_empty(),
            message: if s.matrix_homeserver.is_empty() || s.matrix_access_token.is_empty() || s.matrix_room_id.is_empty() { "missing matrix configuration".into() } else { "ok".into() },
        },
        GatewayDiagnosticItem {
            channel: "webhook".into(),
            enabled: s.webhook_enabled,
            configured: !s.webhook_outbound_url.is_empty(),
            message: if s.webhook_outbound_url.is_empty() { "missing webhook_outbound_url".into() } else { "ok".into() },
        },
    ];
    Ok(items)
}
