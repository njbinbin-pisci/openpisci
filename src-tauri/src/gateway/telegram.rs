use super::{Channel, ChannelStatus, InboundMessage, OutboundMessage};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: String,
}

pub struct TelegramChannel {
    config: TelegramConfig,
    http: Client,
    status: ChannelStatus,
}

impl TelegramChannel {
    pub fn new(config: TelegramConfig) -> Self {
        Self {
            config,
            http: Client::new(),
            status: ChannelStatus::Disconnected,
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.config.bot_token, method)
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str { "telegram" }

    async fn connect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Connecting;
        let resp = self.http
            .get(self.api_url("getMe"))
            .send()
            .await?;
        let body: serde_json::Value = resp.json().await?;
        if body["ok"].as_bool() == Some(true) {
            self.status = ChannelStatus::Connected;
            let bot_name = body["result"]["username"].as_str().unwrap_or("unknown");
            info!("Telegram bot connected: @{}", bot_name);
            Ok(())
        } else {
            let err = body["description"].as_str().unwrap_or("Unknown error").to_string();
            self.status = ChannelStatus::Error(err.clone());
            Err(anyhow::anyhow!("Telegram auth failed: {}", err))
        }
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Disconnected;
        Ok(())
    }

    async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        self.http
            .post(self.api_url("sendMessage"))
            .json(&json!({
                "chat_id": msg.recipient,
                "text": msg.content,
                "parse_mode": "Markdown",
            }))
            .send()
            .await?;
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<InboundMessage>) -> Result<()> {
        let mut offset: i64 = 0;
        info!("Telegram long-polling listener started");
        loop {
            let resp = self.http
                .get(self.api_url("getUpdates"))
                .query(&[
                    ("offset", offset.to_string()),
                    ("timeout", "30".to_string()),
                ])
                .send()
                .await;

            let body: serde_json::Value = match resp {
                Ok(r) => match r.json().await {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("Telegram parse error: {}", e);
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                },
                Err(e) => {
                    warn!("Telegram request error: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            if let Some(updates) = body["result"].as_array() {
                for update in updates {
                    let update_id = update["update_id"].as_i64().unwrap_or(0);
                    offset = update_id + 1;

                    if let Some(message) = update.get("message") {
                        let text = message["text"].as_str().unwrap_or("").to_string();
                        if text.is_empty() { continue; }

                        let chat_id = message["chat"]["id"].as_i64().unwrap_or(0);
                        let sender_id = message["from"]["id"].as_i64().unwrap_or(0);
                        let sender_name = message["from"]["first_name"].as_str().map(String::from);
                        let is_group = message["chat"]["type"].as_str() != Some("private");
                        let group_name = if is_group {
                            message["chat"]["title"].as_str().map(String::from)
                        } else {
                            None
                        };

                        let msg = InboundMessage {
                            id: update_id.to_string(),
                            channel: "telegram".to_string(),
                            sender: sender_id.to_string(),
                            sender_name,
                            content: text,
                            reply_target: chat_id.to_string(),
                            is_group,
                            group_name,
                            timestamp: message["date"].as_u64().unwrap_or(0),
                            media: None,
                        };

                        if tx.send(msg).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}
