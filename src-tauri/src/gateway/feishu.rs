use super::{Channel, ChannelStatus, InboundMessage, OutboundMessage};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuConfig {
    pub app_id: String,
    pub app_secret: String,
    #[serde(default = "default_domain")]
    pub domain: String,
}

fn default_domain() -> String { "feishu".to_string() }

impl FeishuConfig {
    fn base_url(&self) -> &str {
        if self.domain == "lark" {
            "https://open.larksuite.com"
        } else {
            "https://open.feishu.cn"
        }
    }
}

struct TokenCache {
    token: String,
    expires_at: std::time::Instant,
}

pub struct FeishuChannel {
    config: FeishuConfig,
    http: Client,
    status: ChannelStatus,
    token_cache: Arc<RwLock<Option<TokenCache>>>,
    seen_messages: Arc<RwLock<HashMap<String, std::time::Instant>>>,
}

impl FeishuChannel {
    pub fn new(config: FeishuConfig) -> Self {
        Self {
            config,
            http: Client::new(),
            status: ChannelStatus::Disconnected,
            token_cache: Arc::new(RwLock::new(None)),
            seen_messages: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn get_tenant_access_token(&self) -> Result<String> {
        {
            let cache = self.token_cache.read().await;
            if let Some(ref tc) = *cache {
                if tc.expires_at > std::time::Instant::now() {
                    return Ok(tc.token.clone());
                }
            }
        }

        let url = format!(
            "{}/open-apis/auth/v3/tenant_access_token/internal",
            self.config.base_url()
        );
        let resp = self.http
            .post(&url)
            .json(&json!({
                "app_id": self.config.app_id,
                "app_secret": self.config.app_secret,
            }))
            .send()
            .await?;

        let body: serde_json::Value = resp.json().await?;
        let token = body["tenant_access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing tenant_access_token in Feishu response"))?
            .to_string();
        let expires_in = body["expire"].as_u64().unwrap_or(7200);

        let mut cache = self.token_cache.write().await;
        *cache = Some(TokenCache {
            token: token.clone(),
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(expires_in.saturating_sub(300)),
        });

        Ok(token)
    }

    async fn is_duplicate(&self, message_id: &str) -> bool {
        let mut seen = self.seen_messages.write().await;
        let now = std::time::Instant::now();
        seen.retain(|_, t| now.duration_since(*t).as_secs() < 300);
        if seen.contains_key(message_id) {
            return true;
        }
        seen.insert(message_id.to_string(), now);
        false
    }

    async fn send_text(&self, chat_id: &str, text: &str, reply_to: Option<&str>) -> Result<()> {
        let token = self.get_tenant_access_token().await?;
        let url = format!(
            "{}/open-apis/im/v1/messages",
            self.config.base_url()
        );
        let mut body = json!({
            "receive_id": chat_id,
            "msg_type": "text",
            "content": serde_json::to_string(&json!({"text": text}))?,
        });
        if let Some(_reply_id) = reply_to {
            body["reply_in_thread"] = json!(true);
        }
        self.http
            .post(&url)
            .query(&[("receive_id_type", "chat_id")])
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await?;
        Ok(())
    }
}

#[async_trait]
impl Channel for FeishuChannel {
    fn name(&self) -> &str { "feishu" }

    async fn connect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Connecting;
        match self.get_tenant_access_token().await {
            Ok(_) => {
                self.status = ChannelStatus::Connected;
                info!("Feishu channel connected (domain: {})", self.config.domain);
                Ok(())
            }
            Err(e) => {
                self.status = ChannelStatus::Error(e.to_string());
                Err(e)
            }
        }
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Disconnected;
        Ok(())
    }

    async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        self.send_text(&msg.recipient, &msg.content, msg.reply_to.as_deref()).await
    }

    async fn listen(&self, tx: mpsc::Sender<InboundMessage>) -> Result<()> {
        // Long-poll: periodically fetch unread messages via Feishu API.
        // This avoids needing a webhook server on the desktop.
        info!("Feishu listener started (polling mode)");
        let mut last_page_token = String::new();
        loop {
            match self.get_tenant_access_token().await {
                Ok(token) => {
                    let url = format!(
                        "{}/open-apis/im/v1/messages?receive_id_type=chat_id&page_size=20",
                        self.config.base_url()
                    );
                    let mut req_url = url.clone();
                    if !last_page_token.is_empty() {
                        req_url = format!("{}&page_token={}", url, last_page_token);
                    }
                    if let Ok(resp) = self.http.get(&req_url)
                        .header("Authorization", format!("Bearer {}", token))
                        .send().await
                    {
                        if let Ok(body) = resp.json::<serde_json::Value>().await {
                            if let Some(items) = body["data"]["items"].as_array() {
                                for item in items {
                                    let msg_id = item["message_id"].as_str().unwrap_or_default();
                                    if self.is_duplicate(msg_id).await {
                                        continue;
                                    }
                                    let sender = item["sender"]["id"].as_str().unwrap_or_default().to_string();
                                    let chat_id = item["chat_id"].as_str().unwrap_or_default().to_string();
                                    let msg_type = item["msg_type"].as_str().unwrap_or("text");
                                    let content_str = item["body"]["content"].as_str().unwrap_or("{}");
                                    let text = if msg_type == "text" {
                                        serde_json::from_str::<serde_json::Value>(content_str)
                                            .ok()
                                            .and_then(|v| v["text"].as_str().map(String::from))
                                            .unwrap_or_default()
                                    } else {
                                        format!("[{}]", msg_type)
                                    };
                                    if text.is_empty() { continue; }
                                    let msg = InboundMessage {
                                        id: msg_id.to_string(),
                                        channel: "feishu".to_string(),
                                        sender,
                                        sender_name: item["sender"]["sender_type"].as_str().map(String::from),
                                        content: text,
                                        reply_target: chat_id,
                                        is_group: item["chat_type"].as_str() == Some("group"),
                                        group_name: None,
                                        timestamp: item["create_time"].as_str()
                                            .and_then(|s| s.parse::<u64>().ok())
                                            .map(|ms| ms / 1000)
                                            .unwrap_or(0),
                                        media: None,
                                    };
                                    if tx.send(msg).await.is_err() { return Ok(()); }
                                }
                            }
                            if let Some(pt) = body["data"]["page_token"].as_str() {
                                last_page_token = pt.to_string();
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Feishu token refresh failed: {}", e);
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}
