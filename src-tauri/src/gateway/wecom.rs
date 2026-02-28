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
pub struct WecomConfig {
    pub corp_id: String,
    pub agent_secret: String,
    pub agent_id: String,
    pub inbox_file: Option<String>,
}

struct TokenCache {
    token: String,
    expires_at: std::time::Instant,
}

pub struct WecomChannel {
    config: WecomConfig,
    http: Client,
    status: ChannelStatus,
    token_cache: Arc<RwLock<Option<TokenCache>>>,
    consumed_lines: Arc<RwLock<usize>>,
}

impl WecomChannel {
    pub fn new(config: WecomConfig) -> Self {
        Self {
            config,
            http: Client::new(),
            status: ChannelStatus::Disconnected,
            token_cache: Arc::new(RwLock::new(None)),
            consumed_lines: Arc::new(RwLock::new(0)),
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

        let url = format!(
            "https://qyapi.weixin.qq.com/cgi-bin/gettoken?corpid={}&corpsecret={}",
            self.config.corp_id, self.config.agent_secret
        );
        let resp = self.http.get(&url).send().await?;
        let body: serde_json::Value = resp.json().await?;
        let errcode = body["errcode"].as_i64().unwrap_or(-1);
        if errcode != 0 {
            return Err(anyhow::anyhow!(
                "WeCom token error {}: {}",
                errcode,
                body["errmsg"].as_str().unwrap_or("unknown")
            ));
        }
        let token = body["access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing access_token in WeCom response"))?
            .to_string();
        let expires_in = body["expires_in"].as_u64().unwrap_or(7200);

        let mut cache = self.token_cache.write().await;
        *cache = Some(TokenCache {
            token: token.clone(),
            expires_at: std::time::Instant::now()
                + std::time::Duration::from_secs(expires_in.saturating_sub(300)),
        });
        Ok(token)
    }

    async fn send_text(&self, user_id: &str, text: &str) -> Result<()> {
        let token = self.get_access_token().await?;
        let url = format!(
            "https://qyapi.weixin.qq.com/cgi-bin/message/send?access_token={}",
            token
        );
        self.http
            .post(&url)
            .json(&json!({
                "touser": user_id,
                "msgtype": "text",
                "agentid": self.config.agent_id.parse::<i64>().unwrap_or(0),
                "text": { "content": text }
            }))
            .send()
            .await?;
        Ok(())
    }
}

#[async_trait]
impl Channel for WecomChannel {
    fn name(&self) -> &str { "wecom" }

    async fn connect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Connecting;
        match self.get_access_token().await {
            Ok(_) => {
                self.status = ChannelStatus::Connected;
                info!("WeCom channel connected");
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

    async fn listen(&self, _tx: mpsc::Sender<InboundMessage>) -> Result<()> {
        if let Some(path) = &self.config.inbox_file {
            // Local relay mode: external callback service can append JSONL records into this file.
            // Format per line: {"id":"...","sender":"...","sender_name":"...","content":"...","reply_target":"...","timestamp":1700000000}
            info!("WeCom inbound relay enabled via file: {}", path);
            loop {
                match tokio::fs::read_to_string(path).await {
                    Ok(content) => {
                        let lines = content.lines().collect::<Vec<_>>();
                        let mut consumed = self.consumed_lines.write().await;
                        for line in lines.iter().skip(*consumed) {
                            if line.trim().is_empty() {
                                continue;
                            }
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                                let msg = InboundMessage {
                                    id: v["id"].as_str().unwrap_or_default().to_string(),
                                    channel: "wecom".to_string(),
                                    sender: v["sender"].as_str().unwrap_or_default().to_string(),
                                    sender_name: v["sender_name"].as_str().map(String::from),
                                    content: v["content"].as_str().unwrap_or_default().to_string(),
                                    reply_target: v["reply_target"].as_str().unwrap_or_default().to_string(),
                                    is_group: v["is_group"].as_bool().unwrap_or(false),
                                    group_name: v["group_name"].as_str().map(String::from),
                                    timestamp: v["timestamp"].as_u64().unwrap_or(0),
                                    media: None,
                                };
                                if _tx.send(msg).await.is_err() {
                                    return Ok(());
                                }
                            }
                        }
                        *consumed = lines.len();
                    }
                    Err(e) => {
                        tracing::warn!("WeCom inbox relay read failed ({}): {}", path, e);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        } else {
            tracing::warn!(
                "WeCom inbound listener requires callback relay. \
                 Configure 'wecom_inbox_file' to enable local relay mode."
            );
            info!("WeCom channel: outbound-only mode, listener suspended.");
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        }
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}
