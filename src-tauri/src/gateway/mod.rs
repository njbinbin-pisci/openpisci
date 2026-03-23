pub mod dingtalk;
pub mod discord;
pub mod feishu;
pub mod matrix;
pub mod slack;
pub mod teams;
pub mod telegram;
pub mod webhook;
pub mod wechat;
pub mod wecom;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    pub id: String,
    pub channel: String,
    pub sender: String,
    pub sender_name: Option<String>,
    pub content: String,
    pub reply_target: String,
    pub is_group: bool,
    pub group_name: Option<String>,
    pub timestamp: u64,
    pub media: Option<MediaAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    pub channel: String,
    pub recipient: String,
    pub content: String,
    pub reply_to: Option<String>,
    pub media: Option<MediaAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaAttachment {
    pub media_type: String,
    pub url: Option<String>,
    pub data: Option<Vec<u8>>,
    pub filename: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ChannelStatus {
    Disconnected,
    Connecting,
    Connected,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub name: String,
    pub status: ChannelStatus,
    pub connected_at: Option<u64>,
}

#[async_trait]
pub trait Channel: Send + Sync {
    fn name(&self) -> &str;
    async fn connect(&mut self) -> Result<()>;
    async fn disconnect(&mut self) -> Result<()>;
    async fn send(&self, msg: &OutboundMessage) -> Result<()>;
    async fn listen(&self, tx: mpsc::Sender<InboundMessage>) -> Result<()>;
    fn status(&self) -> ChannelStatus;
    async fn health_check(&self) -> bool {
        true
    }
    /// Signal the listen() loop to stop without acquiring a write lock.
    /// Called by stop_all() to avoid deadlock (listen holds read lock indefinitely).
    fn request_shutdown(&self) {}
}

// Use RwLock so listen() (read) and send() (read) can run concurrently.
// connect()/disconnect() take a write lock.
type SharedChannel = Arc<RwLock<Box<dyn Channel>>>;
type ChannelMap = HashMap<String, SharedChannel>;

pub struct GatewayManager {
    channels: RwLock<ChannelMap>,
    inbound_tx: mpsc::Sender<InboundMessage>,
    inbound_rx: Mutex<Option<mpsc::Receiver<InboundMessage>>>,
}

impl GatewayManager {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(256);
        Self {
            channels: RwLock::new(HashMap::new()),
            inbound_tx: tx,
            inbound_rx: Mutex::new(Some(rx)),
        }
    }

    pub async fn register_channel(&self, channel: Box<dyn Channel>) {
        let name = channel.name().to_string();
        info!("Registering gateway channel: {}", name);
        self.channels
            .write()
            .await
            .insert(name, Arc::new(RwLock::new(channel)));
    }

    pub async fn start_all(&self) -> Result<()> {
        let channels = self.channels.read().await;
        for (name, channel) in channels.iter() {
            {
                let mut ch = channel.write().await;
                // Skip channels that are already connected (idempotent)
                if ch.status() == ChannelStatus::Connected {
                    info!("Channel '{}' already connected, skipping", name);
                    continue;
                }
                if let Err(e) = ch.connect().await {
                    warn!("Failed to connect channel '{}': {}", name, e);
                    continue;
                }
            } // write lock released here before spawning listen task

            let tx = self.inbound_tx.clone();
            let ch_arc = channel.clone();
            let channel_name = name.clone();
            // Spawn a supervised listen task with auto-reconnect.
            // listen() and send() both use read locks so they can run concurrently.
            // connect()/disconnect() use write locks for exclusive access.
            tokio::spawn(async move {
                let mut backoff_secs = 1u64;
                const MAX_BACKOFF: u64 = 60;
                loop {
                    // listen() holds a read lock — concurrent with send() which also read-locks
                    let result = {
                        let ch = ch_arc.read().await;
                        ch.listen(tx.clone()).await
                    };
                    match result {
                        Ok(()) => {
                            // Normal exit means disconnect() was called — do NOT reconnect
                            info!(
                                "Channel '{}' listener stopped (disconnect requested)",
                                channel_name
                            );
                            return;
                        }
                        Err(e) => {
                            warn!(
                                "Channel '{}' listen error: {} — reconnecting in {}s",
                                channel_name, e, backoff_secs
                            );
                        }
                    }
                    // Only reach here on error — reconnect with exponential backoff
                    tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                    {
                        let mut ch = ch_arc.write().await;
                        // Don't reconnect if disconnect was called during the sleep
                        if ch.status() == ChannelStatus::Disconnected {
                            info!(
                                "Channel '{}' was disconnected during backoff, not reconnecting",
                                channel_name
                            );
                            return;
                        }
                        match ch.connect().await {
                            Ok(_) => {
                                info!("Channel '{}' reconnected successfully", channel_name);
                                backoff_secs = 1;
                            }
                            Err(e) => {
                                warn!("Channel '{}' reconnect failed: {}", channel_name, e);
                                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF);
                            }
                        }
                    }
                }
            });
            info!("Channel '{}' started with auto-reconnect", name);
        }

        // Spawn periodic health checker
        let channels_for_health = self.channels.read().await.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(120)).await;
                for (name, ch_arc) in &channels_for_health {
                    let ch = ch_arc.read().await;
                    let healthy = ch.health_check().await;
                    if !healthy {
                        warn!(
                            "Channel '{}' health check failed (status={:?})",
                            name,
                            ch.status()
                        );
                    }
                }
            }
        });

        Ok(())
    }

    pub async fn stop_all(&self) -> Result<()> {
        let channels = self.channels.read().await;
        for (name, channel) in channels.iter() {
            // Step 1: signal shutdown via read lock (non-blocking, won't deadlock with listen)
            {
                let ch = channel.read().await;
                ch.request_shutdown();
                info!("Requested shutdown for channel '{}'", name);
            }
            // Step 2: update status via write lock — listen() will have exited within ~2s
            // Use a short timeout to avoid blocking forever if listen is stuck
            match tokio::time::timeout(std::time::Duration::from_secs(5), channel.write()).await {
                Ok(mut ch) => {
                    if let Err(e) = ch.disconnect().await {
                        warn!("Failed to disconnect channel '{}': {}", name, e);
                    }
                }
                Err(_) => {
                    warn!(
                        "Timeout waiting for write lock on channel '{}' during stop_all",
                        name
                    );
                }
            }
        }
        Ok(())
    }

    pub async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        let channels = self.channels.read().await;
        if let Some(channel) = channels.get(&msg.channel) {
            let ch = channel.read().await;
            ch.send(msg).await
        } else {
            Err(anyhow::anyhow!("Channel '{}' not found", msg.channel))
        }
    }

    pub async fn take_receiver(&self) -> Option<mpsc::Receiver<InboundMessage>> {
        self.inbound_rx.lock().await.take()
    }

    pub async fn list_channels(&self) -> Vec<ChannelInfo> {
        let channels = self.channels.read().await;
        let mut infos = Vec::new();
        for (_, channel) in channels.iter() {
            let ch = channel.read().await;
            infos.push(ChannelInfo {
                name: ch.name().to_string(),
                status: ch.status(),
                connected_at: None,
            });
        }
        infos
    }
}
