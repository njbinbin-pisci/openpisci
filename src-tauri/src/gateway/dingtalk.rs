use super::{Channel, ChannelStatus, InboundMessage, OutboundMessage};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DingtalkConfig {
    pub app_key: String,
    pub app_secret: String,
    pub robot_code: Option<String>,
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
}

impl DingtalkChannel {
    pub fn new(config: DingtalkConfig) -> Self {
        Self {
            config,
            http: Client::new(),
            status: ChannelStatus::Disconnected,
            token_cache: Arc::new(RwLock::new(None)),
        }
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

        let resp = self.http
            .post("https://api.dingtalk.com/v1.0/oauth2/accessToken")
            .json(&json!({
                "appKey": self.config.app_key,
                "appSecret": self.config.app_secret,
            }))
            .send()
            .await?;

        let body: serde_json::Value = resp.json().await?;
        let token = body["accessToken"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing accessToken in DingTalk response"))?
            .to_string();
        let expires_in = body["expireIn"].as_u64().unwrap_or(7200);

        let mut cache = self.token_cache.write().await;
        *cache = Some(TokenCache {
            token: token.clone(),
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(expires_in.saturating_sub(300)),
        });

        Ok(token)
    }

    async fn send_text(&self, conversation_id: &str, text: &str) -> Result<()> {
        let token = self.get_access_token().await?;
        self.http
            .post("https://api.dingtalk.com/v1.0/robot/oToMessages/batchSend")
            .header("x-acs-dingtalk-access-token", &token)
            .json(&json!({
                "robotCode": self.config.robot_code.as_deref().unwrap_or(""),
                "userIds": [conversation_id],
                "msgKey": "sampleText",
                "msgParam": serde_json::to_string(&json!({"content": text}))?,
            }))
            .send()
            .await?;
        Ok(())
    }
}

#[async_trait]
impl Channel for DingtalkChannel {
    fn name(&self) -> &str { "dingtalk" }

    async fn connect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Connecting;
        match self.get_access_token().await {
            Ok(_) => {
                self.status = ChannelStatus::Connected;
                info!("DingTalk channel connected");
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
        self.send_text(&msg.recipient, &msg.content).await
    }

    async fn listen(&self, tx: mpsc::Sender<InboundMessage>) -> Result<()> {
        // DingTalk Stream API: register for long-poll messages via HTTP.
        // Desktop apps cannot host webhooks, so we poll the robot received messages API.
        info!("DingTalk listener started (polling mode)");
        let mut last_max_id: i64 = 0;
        loop {
            match self.get_access_token().await {
                Ok(token) => {
                    let resp = self.http
                        .post("https://api.dingtalk.com/v1.0/robot/oToMessages/batchQuery")
                        .header("x-acs-dingtalk-access-token", &token)
                        .json(&serde_json::json!({
                            "robotCode": self.config.robot_code.as_deref().unwrap_or(""),
                            "maxResults": 20
                        }))
                        .send().await;
                    if let Ok(r) = resp {
                        if let Ok(body) = r.json::<serde_json::Value>().await {
                            if let Some(list) = body["result"]["list"].as_array() {
                                for item in list {
                                    let conv_id = item["conversationId"].as_str().unwrap_or_default();
                                    let sender_id = item["senderStaffId"].as_str()
                                        .or_else(|| item["senderId"].as_str())
                                        .unwrap_or_default().to_string();
                                    let text = item["text"]["content"].as_str().unwrap_or_default().trim().to_string();
                                    let msg_id = item["msgId"].as_str().unwrap_or_default().to_string();
                                    let create_at = item["createAt"].as_i64().unwrap_or(0);
                                    if create_at <= last_max_id || text.is_empty() { continue; }
                                    if create_at > last_max_id { last_max_id = create_at; }
                                    let is_group = item["conversationType"].as_str() == Some("2");
                                    let msg = InboundMessage {
                                        id: msg_id,
                                        channel: "dingtalk".to_string(),
                                        sender: sender_id,
                                        sender_name: item["senderNick"].as_str().map(String::from),
                                        content: text,
                                        reply_target: conv_id.to_string(),
                                        is_group,
                                        group_name: item["conversationTitle"].as_str().map(String::from),
                                        timestamp: (create_at / 1000) as u64,
                                        media: None,
                                    };
                                    if tx.send(msg).await.is_err() { return Ok(()); }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("DingTalk token refresh failed: {}", e);
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}
