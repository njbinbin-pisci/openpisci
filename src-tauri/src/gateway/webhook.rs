use super::{Channel, ChannelStatus, InboundMessage, OutboundMessage};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub outbound_url: String,
    pub bearer_token: Option<String>,
}

pub struct WebhookChannel {
    config: WebhookConfig,
    http: Client,
    status: ChannelStatus,
}

impl WebhookChannel {
    pub fn new(config: WebhookConfig) -> Self {
        Self {
            config,
            http: Client::new(),
            status: ChannelStatus::Disconnected,
        }
    }
}

#[async_trait]
impl Channel for WebhookChannel {
    fn name(&self) -> &str { "webhook" }

    async fn connect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Connected;
        info!("Webhook channel connected");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Disconnected;
        Ok(())
    }

    async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        let payload = json!({
            "channel": msg.channel,
            "recipient": msg.recipient,
            "content": msg.content,
            "reply_to": msg.reply_to,
        });
        let mut req = self.http.post(&self.config.outbound_url).json(&payload);
        if let Some(token) = &self.config.bearer_token {
            if !token.trim().is_empty() {
                req = req.bearer_auth(token);
            }
        }
        req.send().await?.error_for_status()?;
        Ok(())
    }

    async fn listen(&self, _tx: mpsc::Sender<InboundMessage>) -> Result<()> {
        warn!("Webhook inbound server is not enabled; outbound-only channel active");
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}
