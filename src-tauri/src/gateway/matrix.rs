use super::{Channel, ChannelStatus, InboundMessage, OutboundMessage};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixConfig {
    pub homeserver: String,
    pub access_token: String,
    pub room_id: String,
}

pub struct MatrixChannel {
    config: MatrixConfig,
    http: Client,
    status: ChannelStatus,
}

impl MatrixChannel {
    pub fn new(config: MatrixConfig) -> Self {
        Self {
            config,
            http: Client::new(),
            status: ChannelStatus::Disconnected,
        }
    }
}

#[async_trait]
impl Channel for MatrixChannel {
    fn name(&self) -> &str { "matrix" }

    async fn connect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Connected;
        info!("Matrix channel connected");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Disconnected;
        Ok(())
    }

    async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        let txn_id = Uuid::new_v4().to_string();
        let room = if msg.recipient.is_empty() {
            &self.config.room_id
        } else {
            &msg.recipient
        };
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.config.homeserver.trim_end_matches('/'),
            urlencoding::encode(room),
            txn_id
        );
        self.http
            .put(url)
            .bearer_auth(&self.config.access_token)
            .json(&json!({
                "msgtype": "m.text",
                "body": msg.content
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn listen(&self, _tx: mpsc::Sender<InboundMessage>) -> Result<()> {
        warn!("Matrix inbound sync is not enabled yet; outbound-only channel active");
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}
