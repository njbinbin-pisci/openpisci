use super::{Channel, ChannelStatus, InboundMessage, OutboundMessage};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DingtalkConfig {
    pub app_key: String,
    pub app_secret: String,
    /// Robot code for official proactive send APIs. Recommended by DingTalk docs.
    pub robot_code: Option<String>,
}

enum DingtalkRecipient<'a> {
    SessionWebhook(&'a str),
    Group(&'a str),
    User(&'a str),
}

struct TokenCache {
    token: String,
    expires_at: std::time::Instant,
}

pub struct DingtalkChannel {
    config: DingtalkConfig,
    http: Client,
    status: ChannelStatus,
    token_cache: Arc<RwLock<Option<TokenCache>>>,
    shutdown: Arc<AtomicBool>,
}

impl DingtalkChannel {
    pub fn new(config: DingtalkConfig) -> Self {
        let http = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            config,
            http,
            status: ChannelStatus::Disconnected,
            token_cache: Arc::new(RwLock::new(None)),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    fn robot_code(&self) -> &str {
        self.config
            .robot_code
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(self.config.app_key.as_str())
    }

    async fn get_access_token(&self) -> Result<String> {
        {
            let cache = self.token_cache.read().await;
            if let Some(ref tc) = *cache {
                if tc.expires_at > std::time::Instant::now() {
                    return Ok(tc.token.clone());
                }
            }
        }

        let resp = self
            .http
            .post("https://api.dingtalk.com/v1.0/oauth2/accessToken")
            .json(&json!({
                "appKey": self.config.app_key,
                "appSecret": self.config.app_secret,
            }))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Network error reaching DingTalk API: {}", e))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Invalid JSON from DingTalk auth API: {}", e))?;

        let token = body["accessToken"]
            .as_str()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Missing accessToken in DingTalk response: {:?}. Check appKey/appSecret.",
                    body
                )
            })?
            .to_string();
        let expires_in = body["expireIn"].as_u64().unwrap_or(7200);

        let mut cache = self.token_cache.write().await;
        *cache = Some(TokenCache {
            token: token.clone(),
            expires_at: std::time::Instant::now()
                + std::time::Duration::from_secs(expires_in.saturating_sub(300)),
        });

        Ok(token)
    }

    fn parse_recipient<'a>(&self, recipient: &'a str) -> DingtalkRecipient<'a> {
        if let Some(value) = recipient.strip_prefix("sessionWebhook:") {
            DingtalkRecipient::SessionWebhook(value)
        } else if let Some(value) = recipient.strip_prefix("group:") {
            DingtalkRecipient::Group(value)
        } else if let Some(value) = recipient.strip_prefix("user:") {
            DingtalkRecipient::User(value)
        } else {
            DingtalkRecipient::User(recipient)
        }
    }

    async fn send_user_text(&self, user_id: &str, text: &str) -> Result<()> {
        let token = self.get_access_token().await?;
        let resp = self
            .http
            .post("https://api.dingtalk.com/v1.0/robot/oToMessages/batchSend")
            .header("x-acs-dingtalk-access-token", &token)
            .json(&json!({
                "robotCode": self.robot_code(),
                "userIds": [user_id],
                "msgKey": "sampleText",
                "msgParam": serde_json::to_string(&json!({"content": text}))?,
            }))
            .send()
            .await?;
        ensure_dingtalk_send_ok(resp).await
    }

    async fn send_group_text(&self, open_conversation_id: &str, text: &str) -> Result<()> {
        let token = self.get_access_token().await?;
        let resp = self
            .http
            .post("https://api.dingtalk.com/v1.0/robot/groupMessages/send")
            .header("x-acs-dingtalk-access-token", &token)
            .json(&json!({
                "robotCode": self.robot_code(),
                "openConversationId": open_conversation_id,
                "msgKey": "sampleText",
                "msgParam": serde_json::to_string(&json!({"content": text}))?,
            }))
            .send()
            .await?;
        ensure_dingtalk_send_ok(resp).await
    }

    async fn send_via_session_webhook(&self, session_webhook: &str, text: &str) -> Result<()> {
        let resp = self
            .http
            .post(session_webhook)
            .json(&json!({
                "msgtype": "text",
                "text": { "content": text }
            }))
            .send()
            .await?;
        ensure_dingtalk_send_ok(resp).await
    }

    async fn send_message(&self, recipient: &str, text: &str) -> Result<()> {
        match self.parse_recipient(recipient) {
            DingtalkRecipient::SessionWebhook(url) if !url.trim().is_empty() => {
                self.send_via_session_webhook(url, text).await
            }
            DingtalkRecipient::Group(open_conversation_id)
                if !open_conversation_id.trim().is_empty() =>
            {
                self.send_group_text(open_conversation_id, text).await
            }
            DingtalkRecipient::User(user_id) if !user_id.trim().is_empty() => {
                self.send_user_text(user_id, text).await
            }
            _ => Err(anyhow::anyhow!("DingTalk recipient is empty")),
        }
    }

    async fn handle_stream_callback_frame(
        &self,
        frame: &serde_json::Value,
        message_id: &str,
        tx: &mpsc::Sender<InboundMessage>,
    ) -> Result<()> {
        let data_str = frame["data"].as_str().unwrap_or("{}");
        let data = serde_json::from_str::<serde_json::Value>(data_str)
            .map_err(|e| anyhow::anyhow!("Invalid DingTalk callback payload JSON: {}", e))?;

        if data["msgtype"].as_str() != Some("text") {
            return Ok(());
        }

        let sender_user_id = data["senderStaffId"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                data["senderId"]
                    .as_str()
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_default()
            .to_string();
        let sender_nick = data["senderNick"].as_str().map(String::from);
        let conversation_id = data["conversationId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let session_webhook = data["sessionWebhook"].as_str();
        let msg_id = data["msgId"].as_str().unwrap_or(message_id).to_string();
        let text_content = data["text"]["content"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();
        let is_group = data["conversationType"].as_str() == Some("2");
        let group_name = data["conversationTitle"].as_str().map(String::from);
        let create_at = data["createAt"].as_u64().unwrap_or(0);

        if text_content.is_empty() || sender_user_id.is_empty() {
            return Ok(());
        }

        let inbound = InboundMessage {
            id: msg_id,
            channel: "dingtalk".to_string(),
            sender: sender_user_id.clone(),
            sender_name: sender_nick,
            content: text_content,
            reply_target: build_reply_target(
                session_webhook,
                is_group,
                &conversation_id,
                &sender_user_id,
            ),
            conversation_key: Some(if is_group {
                format!("conversation:{}", conversation_id)
            } else {
                format!("user:{}", sender_user_id)
            }),
            is_group,
            group_name,
            timestamp: create_at,
            media: None,
            routing_state: None,
        };

        if tx.send(inbound).await.is_err() {
            return Err(anyhow::anyhow!("DingTalk inbound consumer dropped"));
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for DingtalkChannel {
    fn name(&self) -> &str {
        "dingtalk"
    }

    async fn connect(&mut self) -> Result<()> {
        self.shutdown.store(false, Ordering::Relaxed);
        self.status = ChannelStatus::Connecting;
        match self.get_access_token().await {
            Ok(_) => {
                self.status = ChannelStatus::Connected;
                info!("DingTalk channel connected (official Stream mode)");
                Ok(())
            }
            Err(e) => {
                self.status = ChannelStatus::Error(e.to_string());
                Err(e)
            }
        }
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.shutdown.store(true, Ordering::Relaxed);
        self.status = ChannelStatus::Disconnected;
        info!("DingTalk: disconnect requested, listener will stop");
        Ok(())
    }

    async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        self.send_message(&msg.recipient, &msg.content).await
    }

    /// DingTalk Stream mode: establish a WebSocket long connection to receive robot messages.
    async fn listen(&self, tx: mpsc::Sender<InboundMessage>) -> Result<()> {
        info!("DingTalk listener started (official Stream mode WebSocket)");

        let config = self.config.clone();
        let http = self.http.clone();
        let shutdown = self.shutdown.clone();

        let mut backoff = std::time::Duration::from_secs(1);
        const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(60);

        loop {
            if shutdown.load(Ordering::Relaxed) {
                info!("DingTalk: shutdown flag set, listener exiting");
                return Ok(());
            }

            let (ws_endpoint, ticket) = match get_stream_connection(&http, &config).await {
                Ok(v) => {
                    backoff = std::time::Duration::from_secs(1);
                    v
                }
                Err(e) => {
                    warn!("DingTalk: failed to get stream connection: {}", e);
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    continue;
                }
            };

            let ws_url = format!("{}?ticket={}", ws_endpoint, ticket);
            info!("DingTalk: connecting to Stream WebSocket: {}", ws_endpoint);

            match tokio_tungstenite::connect_async(&ws_url).await {
                Ok((ws_stream, _)) => {
                    info!("DingTalk: Stream WebSocket connected");
                    backoff = std::time::Duration::from_secs(1);

                    use futures::{SinkExt, StreamExt};
                    let (mut ws_sink, mut ws_reader) = futures::StreamExt::split(ws_stream);

                    let poll = std::time::Duration::from_secs(2);
                    let mut ping_elapsed = std::time::Duration::ZERO;
                    const PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

                    loop {
                        match tokio::time::timeout(poll, ws_reader.next()).await {
                            Ok(Some(Ok(msg))) => {
                                let text = match msg {
                                    tokio_tungstenite::tungstenite::Message::Text(t) => t,
                                    tokio_tungstenite::tungstenite::Message::Binary(b) => {
                                        String::from_utf8_lossy(&b).into_owned()
                                    }
                                    tokio_tungstenite::tungstenite::Message::Ping(data) => {
                                        let _ = ws_sink
                                            .send(tokio_tungstenite::tungstenite::Message::Pong(
                                                data,
                                            ))
                                            .await;
                                        continue;
                                    }
                                    tokio_tungstenite::tungstenite::Message::Pong(_) => continue,
                                    tokio_tungstenite::tungstenite::Message::Close(_) => {
                                        warn!("DingTalk: Stream WebSocket closed by server");
                                        break;
                                    }
                                    _ => continue,
                                };

                                if let Ok(frame) = serde_json::from_str::<serde_json::Value>(&text)
                                {
                                    let frame_type = frame["type"].as_str().unwrap_or("");
                                    let topic = frame["headers"]["topic"].as_str().unwrap_or("");
                                    let message_id = frame["headers"]["messageId"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string();

                                    let ack = json!({
                                        "code": 200,
                                        "headers": {
                                            "messageId": message_id,
                                            "contentType": "application/json"
                                        },
                                        "message": "OK",
                                        "data": ""
                                    });
                                    let _ = ws_sink
                                        .send(tokio_tungstenite::tungstenite::Message::Text(
                                            ack.to_string(),
                                        ))
                                        .await;

                                    if frame_type == "CALLBACK"
                                        && topic == "/v1.0/im/bot/messages/get"
                                    {
                                        match self
                                            .handle_stream_callback_frame(&frame, &message_id, &tx)
                                            .await
                                        {
                                            Ok(()) => {}
                                            Err(e)
                                                if e.to_string().contains("consumer dropped") =>
                                            {
                                                return Ok(());
                                            }
                                            Err(e) => {
                                                warn!(
                                                    "DingTalk: failed to process callback frame: {}",
                                                    e
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(Some(Err(e))) => {
                                warn!("DingTalk: Stream WebSocket error: {}", e);
                                break;
                            }
                            Ok(None) => {
                                warn!("DingTalk: Stream WebSocket stream ended");
                                break;
                            }
                            Err(_) => {
                                if shutdown.load(Ordering::Relaxed) {
                                    info!("DingTalk: shutdown requested, closing WebSocket");
                                    let _ = ws_sink
                                        .send(tokio_tungstenite::tungstenite::Message::Close(None))
                                        .await;
                                    return Ok(());
                                }
                                ping_elapsed += poll;
                                if ping_elapsed >= PING_INTERVAL {
                                    ping_elapsed = std::time::Duration::ZERO;
                                    let _ = ws_sink
                                        .send(tokio_tungstenite::tungstenite::Message::Ping(vec![]))
                                        .await;
                                }
                            }
                        }

                        if shutdown.load(Ordering::Relaxed) {
                            info!("DingTalk: shutdown after message, closing WebSocket");
                            let _ = ws_sink
                                .send(tokio_tungstenite::tungstenite::Message::Close(None))
                                .await;
                            return Ok(());
                        }
                    }
                }
                Err(e) => {
                    warn!("DingTalk: Stream WebSocket connect failed: {}", e);
                }
            }

            if shutdown.load(Ordering::Relaxed) {
                info!("DingTalk: shutdown, not reconnecting");
                return Ok(());
            }
            warn!("DingTalk: reconnecting in {:?}", backoff);
            let mut remaining = backoff;
            let chunk = std::time::Duration::from_secs(1);
            while remaining > std::time::Duration::ZERO {
                if shutdown.load(Ordering::Relaxed) {
                    return Ok(());
                }
                tokio::time::sleep(chunk.min(remaining)).await;
                remaining = remaining.saturating_sub(chunk);
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }

    fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        info!("DingTalk: shutdown flag set via request_shutdown()");
    }
}

async fn ensure_dingtalk_send_ok(resp: reqwest::Response) -> Result<()> {
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Invalid JSON from DingTalk send API: {}", e))?;

    if !status.is_success() {
        return Err(anyhow::anyhow!("DingTalk send HTTP {}: {}", status, body));
    }
    if let Some(code) = body["errcode"].as_i64().or(body["code"].as_i64()) {
        if code != 0 {
            return Err(anyhow::anyhow!(
                "DingTalk send error {}: {}",
                code,
                body["errmsg"]
                    .as_str()
                    .or(body["message"].as_str())
                    .unwrap_or("unknown")
            ));
        }
    }
    Ok(())
}

fn build_reply_target(
    session_webhook: Option<&str>,
    is_group: bool,
    conversation_id: &str,
    sender_user_id: &str,
) -> String {
    if let Some(webhook) = session_webhook.filter(|value| !value.trim().is_empty()) {
        format!("sessionWebhook:{}", webhook)
    } else if is_group {
        format!("group:{}", conversation_id)
    } else {
        format!("user:{}", sender_user_id)
    }
}

/// Register a DingTalk Stream connection and return (endpoint, ticket).
async fn get_stream_connection(http: &Client, config: &DingtalkConfig) -> Result<(String, String)> {
    let resp = http
        .post("https://api.dingtalk.com/v1.0/gateway/connections/open")
        .json(&json!({
            "clientId": config.app_key,
            "clientSecret": config.app_secret,
            "subscriptions": [
                {
                    "type": "CALLBACK",
                    "topic": "/v1.0/im/bot/messages/get"
                }
            ],
            "ua": "openpisci/1.0",
            "localIp": ""
        }))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Network error reaching DingTalk Stream API: {}", e))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Invalid JSON from DingTalk Stream API: {}", e))?;

    let endpoint = body["endpoint"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing endpoint in DingTalk Stream response: {:?}", body))?
        .to_string();
    let ticket = body["ticket"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing ticket in DingTalk Stream response: {:?}", body))?
        .to_string();

    Ok((endpoint, ticket))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_session_webhook_reply_target() {
        let target = build_reply_target(
            Some("https://oapi.dingtalk.com/robot/sendBySession?session=abc"),
            true,
            "cid",
            "user123",
        );
        assert_eq!(
            target,
            "sessionWebhook:https://oapi.dingtalk.com/robot/sendBySession?session=abc"
        );
    }

    #[test]
    fn parses_recipient_kinds() {
        let channel = DingtalkChannel::new(DingtalkConfig {
            app_key: "app".into(),
            app_secret: "secret".into(),
            robot_code: Some("robot".into()),
        });

        assert!(matches!(
            channel.parse_recipient("sessionWebhook:https://example.com"),
            DingtalkRecipient::SessionWebhook(_)
        ));
        assert!(matches!(
            channel.parse_recipient("group:cid123"),
            DingtalkRecipient::Group("cid123")
        ));
        assert!(matches!(
            channel.parse_recipient("user:u123"),
            DingtalkRecipient::User("u123")
        ));
    }
}
