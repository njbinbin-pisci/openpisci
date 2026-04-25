use crate::gateway::{
    dingtalk::{DingtalkChannel, DingtalkConfig},
    discord::{DiscordChannel, DiscordConfig},
    feishu::{FeishuChannel, FeishuConfig},
    matrix::{MatrixChannel, MatrixConfig},
    slack::{SlackChannel, SlackConfig},
    teams::{TeamsChannel, TeamsConfig},
    telegram::{TelegramChannel, TelegramConfig},
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
            robot_code: if settings.dingtalk_robot_code.is_empty() {
                None
            } else {
                Some(settings.dingtalk_robot_code.clone())
            },
        };
        let ch = Box::new(DingtalkChannel::new(config));
        state.gateway.register_channel(ch).await;
    }

    // Telegram
    if settings.telegram_enabled && !settings.telegram_bot_token.is_empty() {
        let config = TelegramConfig {
            bot_token: settings.telegram_bot_token.clone(),
        };
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

    // WeChat (iLink Bot HTTP server)
    if settings.wechat_enabled {
        let config = crate::gateway::wechat::WechatConfig {
            gateway_token: settings.wechat_gateway_token.clone(),
            port: settings.wechat_gateway_port,
            bot_token: settings.wechat_bot_token.clone(),
            base_url: settings.wechat_base_url.clone(),
        };
        let ch = Box::new(crate::gateway::wechat::WechatChannel::new(config));
        state.gateway.register_channel(ch).await;
    }

    // 企业微信
    if settings.wecom_enabled
        && !settings.wecom_bot_id.is_empty()
        && !settings.wecom_bot_secret.is_empty()
    {
        let config = crate::gateway::wecom::WecomConfig {
            bot_id: settings.wecom_bot_id.clone(),
            bot_secret: settings.wecom_bot_secret.clone(),
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

const ILINK_DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const ILINK_BOT_TYPE: &str = "3";

/// Fetch a WeChat QR code from the Tencent iLink API and return the QR image
/// URL plus the opaque `qrcode` token needed for status polling.
///
/// No external CLI required — this calls the same public HTTP endpoint that
/// the `@tencent-weixin/openclaw-weixin` plugin uses internally.
#[tauri::command]
pub async fn start_wechat_login(state: State<'_, AppState>) -> Result<WechatLoginStatus, String> {
    let client = reqwest::Client::new();
    let url = format!(
        "{}/ilink/bot/get_bot_qrcode?bot_type={}",
        ILINK_DEFAULT_BASE_URL, ILINK_BOT_TYPE
    );

    tracing::info!("Fetching WeChat QR code from iLink API");

    let resp = client
        .get(&url)
        .header("iLink-App-ClientVersion", "1")
        .send()
        .await
        .map_err(|e| format!("Network error fetching QR code: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!(
            "iLink API returned HTTP {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        ));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse QR code response: {}", e))?;

    let qrcode_token = body["qrcode"]
        .as_str()
        .ok_or("iLink API response missing 'qrcode' field")?
        .to_string();

    // qrcode_img_content is a weixin:// deep-link URL — we convert it to a
    // QR image using the qrcode crate so the frontend can render it directly.
    let qrcode_url = body["qrcode_img_content"]
        .as_str()
        .unwrap_or(&qrcode_token)
        .to_string();

    tracing::info!(
        "WeChat QR code obtained, token length={}",
        qrcode_token.len()
    );

    // Generate a PNG QR code image as base64 data URL for the frontend.
    let qr_data_url = generate_qr_data_url(&qrcode_url)?;

    // Persist the qrcode token in AppState so poll_wechat_login can use it.
    {
        let mut settings = state.settings.lock().await;
        // Temporarily store the qrcode token in wechat_bot_token field
        // (will be overwritten with the real bot_token on confirmed login).
        settings.wechat_bot_token = format!("qr:{}", qrcode_token);
        // Don't save to disk yet — this is ephemeral state.
    }

    Ok(WechatLoginStatus {
        qr_data_url: Some(qr_data_url),
        qrcode_token: Some(qrcode_token),
        message: "scan_qr".to_string(),
        connected: false,
        bot_id: None,
    })
}

/// Poll the iLink API for the QR code scan status.
///
/// The frontend calls this repeatedly after `start_wechat_login` returns.
/// Returns immediately with the current status; the frontend is responsible
/// for the polling interval (recommended: 2 s).
#[tauri::command]
pub async fn poll_wechat_login(
    qrcode_token: String,
    state: State<'_, AppState>,
) -> Result<WechatLoginStatus, String> {
    let client = reqwest::Client::new();
    let url = format!(
        "{}/ilink/bot/get_qrcode_status?qrcode={}",
        ILINK_DEFAULT_BASE_URL,
        urlencoding::encode(&qrcode_token)
    );

    let resp = client
        .get(&url)
        .header("iLink-App-ClientVersion", "1")
        .timeout(std::time::Duration::from_secs(38))
        .send()
        .await
        .map_err(|e| format!("Network error polling QR status: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("iLink status API returned HTTP {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse status response: {}", e))?;

    let status = body["status"].as_str().unwrap_or("wait");

    match status {
        "confirmed" => {
            let bot_token = body["bot_token"].as_str().unwrap_or("").to_string();
            let bot_id = body["ilink_bot_id"].as_str().unwrap_or("").to_string();
            let base_url = body["baseurl"]
                .as_str()
                .unwrap_or(ILINK_DEFAULT_BASE_URL)
                .to_string();

            tracing::info!("WeChat QR login confirmed, bot_id={}", bot_id);

            // Persist credentials to settings.
            {
                let mut settings = state.settings.lock().await;
                settings.wechat_bot_token = bot_token.clone();
                settings.wechat_base_url = base_url;
                settings.wechat_bot_id = bot_id.clone();
                settings.wechat_enabled = true;
                if let Err(e) = settings.save() {
                    tracing::warn!("Failed to save WeChat credentials: {}", e);
                }
            }

            Ok(WechatLoginStatus {
                qr_data_url: None,
                qrcode_token: None,
                message: "connected".to_string(),
                connected: true,
                bot_id: Some(bot_id),
            })
        }
        "expired" => Ok(WechatLoginStatus {
            qr_data_url: None,
            qrcode_token: None,
            message: "expired".to_string(),
            connected: false,
            bot_id: None,
        }),
        "scaned" => Ok(WechatLoginStatus {
            qr_data_url: None,
            qrcode_token: None,
            message: "scaned".to_string(),
            connected: false,
            bot_id: None,
        }),
        _ => Ok(WechatLoginStatus {
            qr_data_url: None,
            qrcode_token: None,
            message: "wait".to_string(),
            connected: false,
            bot_id: None,
        }),
    }
}

/// Generate a `data:image/png;base64,...` QR code from a URL string.
fn generate_qr_data_url(content: &str) -> Result<String, String> {
    use qrcode::render::svg;
    use qrcode::QrCode;

    let code = QrCode::new(content.as_bytes())
        .map_err(|e| format!("Failed to generate QR code: {}", e))?;

    let svg_str = code.render::<svg::Color>().min_dimensions(200, 200).build();

    // Encode SVG as base64 data URL
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(svg_str.as_bytes());
    Ok(format!("data:image/svg+xml;base64,{}", b64))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WechatLoginStatus {
    pub qr_data_url: Option<String>,
    pub qrcode_token: Option<String>,
    pub message: String,
    pub connected: bool,
    pub bot_id: Option<String>,
}

/// Return per-channel config diagnostics before connect.
#[tauri::command]
pub async fn diagnose_gateway_channels(
    state: State<'_, AppState>,
) -> Result<Vec<GatewayDiagnosticItem>, String> {
    let s = state.settings.lock().await.clone();
    let items = vec![
        GatewayDiagnosticItem {
            channel: "telegram".into(),
            enabled: s.telegram_enabled,
            configured: !s.telegram_bot_token.is_empty(),
            message: if s.telegram_bot_token.is_empty() {
                "missing telegram_bot_token".into()
            } else {
                "ok".into()
            },
        },
        GatewayDiagnosticItem {
            channel: "feishu".into(),
            enabled: s.feishu_enabled,
            configured: !s.feishu_app_id.is_empty() && !s.feishu_app_secret.is_empty(),
            message: if s.feishu_app_id.is_empty() || s.feishu_app_secret.is_empty() {
                "missing feishu app credentials".into()
            } else {
                "ok".into()
            },
        },
        GatewayDiagnosticItem {
            channel: "dingtalk".into(),
            enabled: s.dingtalk_enabled,
            configured: !s.dingtalk_app_key.is_empty() && !s.dingtalk_app_secret.is_empty(),
            message: if s.dingtalk_app_key.is_empty() || s.dingtalk_app_secret.is_empty() {
                "missing dingtalk app credentials".into()
            } else if s.dingtalk_robot_code.is_empty() {
                "stream receive ready; add robot_code to enable official proactive send APIs".into()
            } else {
                "ok".into()
            },
        },
        GatewayDiagnosticItem {
            channel: "wecom".into(),
            enabled: s.wecom_enabled,
            configured: !s.wecom_bot_id.is_empty() && !s.wecom_bot_secret.is_empty(),
            message: if s.wecom_bot_id.is_empty() || s.wecom_bot_secret.is_empty() {
                "missing wecom bot_id / bot_secret".into()
            } else {
                "wecom long-connection ready".into()
            },
        },
        GatewayDiagnosticItem {
            channel: "slack".into(),
            enabled: s.slack_enabled,
            configured: !s.slack_webhook_url.is_empty(),
            message: if s.slack_webhook_url.is_empty() {
                "missing slack_webhook_url".into()
            } else {
                "ok".into()
            },
        },
        GatewayDiagnosticItem {
            channel: "discord".into(),
            enabled: s.discord_enabled,
            configured: !s.discord_webhook_url.is_empty(),
            message: if s.discord_webhook_url.is_empty() {
                "missing discord_webhook_url".into()
            } else {
                "ok".into()
            },
        },
        GatewayDiagnosticItem {
            channel: "teams".into(),
            enabled: s.teams_enabled,
            configured: !s.teams_webhook_url.is_empty(),
            message: if s.teams_webhook_url.is_empty() {
                "missing teams_webhook_url".into()
            } else {
                "ok".into()
            },
        },
        GatewayDiagnosticItem {
            channel: "matrix".into(),
            enabled: s.matrix_enabled,
            configured: !s.matrix_homeserver.is_empty()
                && !s.matrix_access_token.is_empty()
                && !s.matrix_room_id.is_empty(),
            message: if s.matrix_homeserver.is_empty()
                || s.matrix_access_token.is_empty()
                || s.matrix_room_id.is_empty()
            {
                "missing matrix configuration".into()
            } else {
                "ok".into()
            },
        },
        GatewayDiagnosticItem {
            channel: "webhook".into(),
            enabled: s.webhook_enabled,
            configured: !s.webhook_outbound_url.is_empty(),
            message: if s.webhook_outbound_url.is_empty() {
                "missing webhook_outbound_url".into()
            } else {
                "ok".into()
            },
        },
        GatewayDiagnosticItem {
            channel: "wechat".into(),
            enabled: s.wechat_enabled,
            configured: true,
            message: format!("OpenClaw-compat gateway on port {}", s.wechat_gateway_port),
        },
    ];
    Ok(items)
}
