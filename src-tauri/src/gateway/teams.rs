use super::{Channel, ChannelStatus, InboundMessage, OutboundMessage};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamsConfig {
    pub webhook_url: String,
}

pub struct TeamsChannel {
    config: TeamsConfig,
    http: Client,
    status: ChannelStatus,
}

impl TeamsChannel {
    pub fn new(config: TeamsConfig) -> Self {
        Self {
            config,
            http: Client::new(),
            status: ChannelStatus::Disconnected,
        }
    }
}

#[async_trait]
impl Channel for TeamsChannel {
    fn name(&self) -> &str {
        "teams"
    }

    async fn connect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Connected;
        info!("Teams webhook channel connected");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Disconnected;
        Ok(())
    }

    async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        self.http
            .post(&self.config.webhook_url)
            .json(&json!({ "text": msg.content }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn listen(&self, _tx: mpsc::Sender<InboundMessage>) -> Result<()> {
        warn!("Teams inbound is not enabled in webhook mode; outbound-only channel active");
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}
