use super::{Channel, ChannelStatus, InboundMessage, OutboundMessage};
use anyhow::Result;
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};
use uuid::Uuid;

const WECOM_WS_URL: &str = "wss://openws.work.weixin.qq.com";
const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
const PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
const OUTBOUND_BUFFER: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WecomConfig {
    pub bot_id: String,
    pub bot_secret: String,
}

pub struct WecomChannel {
    config: WecomConfig,
    status: ChannelStatus,
    shutdown: Arc<AtomicBool>,
    outbound_tx: Arc<RwLock<Option<mpsc::Sender<Value>>>>,
}

impl WecomChannel {
    pub fn new(config: WecomConfig) -> Self {
        Self {
            config,
            status: ChannelStatus::Disconnected,
            shutdown: Arc::new(AtomicBool::new(false)),
            outbound_tx: Arc::new(RwLock::new(None)),
        }
    }

    async fn enqueue_frame(&self, frame: Value) -> Result<()> {
        let tx = self
            .outbound_tx
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("WeCom long connection is not ready yet"))?;
        tx.send(frame)
            .await
            .map_err(|_| anyhow::anyhow!("WeCom outbound queue is closed"))
    }
}

#[async_trait]
impl Channel for WecomChannel {
    fn name(&self) -> &str {
        "wecom"
    }

    async fn connect(&mut self) -> Result<()> {
        self.shutdown.store(false, Ordering::Relaxed);
        self.status = ChannelStatus::Connecting;
        if self.config.bot_id.trim().is_empty() || self.config.bot_secret.trim().is_empty() {
            let err = anyhow::anyhow!("missing wecom bot_id / bot_secret");
            self.status = ChannelStatus::Error(err.to_string());
            return Err(err);
        }
        self.status = ChannelStatus::Connected;
        info!(
            "WeCom channel configured for smart-robot long connection, bot_id={}",
            self.config.bot_id
        );
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.shutdown.store(true, Ordering::Relaxed);
        *self.outbound_tx.write().await = None;
        self.status = ChannelStatus::Disconnected;
        Ok(())
    }

    async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        self.enqueue_frame(build_outbound_frame(msg)?).await
    }

    async fn listen(&self, tx: mpsc::Sender<InboundMessage>) -> Result<()> {
        let mut backoff = std::time::Duration::from_secs(1);

        while !self.shutdown.load(Ordering::Relaxed) {
            info!("WeCom: connecting long-connection WebSocket");
            match tokio_tungstenite::connect_async(WECOM_WS_URL).await {
                Ok((ws_stream, _)) => {
                    info!("WeCom: WebSocket connected");
                    backoff = std::time::Duration::from_secs(1);

                    let (mut ws_sink, mut ws_reader) = ws_stream.split();
                    let (outbound_tx, mut outbound_rx) = mpsc::channel::<Value>(OUTBOUND_BUFFER);
                    *self.outbound_tx.write().await = Some(outbound_tx);

                    let (subscribe_req_id, subscribe_frame) = build_subscribe_frame(&self.config);
                    ws_sink
                        .send(tokio_tungstenite::tungstenite::Message::Text(
                            subscribe_frame.to_string(),
                        ))
                        .await
                        .map_err(|e| anyhow::anyhow!("WeCom subscribe send failed: {}", e))?;

                    let mut ping_elapsed = std::time::Duration::ZERO;

                    loop {
                        while let Ok(frame) = outbound_rx.try_recv() {
                            ws_sink
                                .send(tokio_tungstenite::tungstenite::Message::Text(
                                    frame.to_string(),
                                ))
                                .await
                                .map_err(|e| {
                                    anyhow::anyhow!("WeCom outbound send failed: {}", e)
                                })?;
                        }

                        match tokio::time::timeout(POLL_INTERVAL, ws_reader.next()).await {
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
                                        warn!("WeCom: WebSocket closed by server");
                                        break;
                                    }
                                    _ => continue,
                                };

                                let frame = match serde_json::from_str::<Value>(&text) {
                                    Ok(frame) => frame,
                                    Err(e) => {
                                        warn!("WeCom: invalid frame JSON: {}", e);
                                        continue;
                                    }
                                };

                                if let Some(errcode) = frame["errcode"].as_i64() {
                                    let req_id =
                                        frame["headers"]["req_id"].as_str().unwrap_or_default();
                                    let errmsg =
                                        frame["errmsg"].as_str().unwrap_or("unknown error");
                                    if errcode != 0 {
                                        if req_id == subscribe_req_id {
                                            return Err(anyhow::anyhow!(
                                                "WeCom subscribe error {}: {}",
                                                errcode,
                                                errmsg
                                            ));
                                        }
                                        warn!(
                                            "WeCom request {} failed with {}: {}",
                                            req_id, errcode, errmsg
                                        );
                                    } else if req_id == subscribe_req_id {
                                        info!("WeCom: subscription established");
                                    }
                                    continue;
                                }

                                if is_disconnected_event(&frame) {
                                    warn!("WeCom: received disconnected_event, reconnecting");
                                    break;
                                }

                                if let Some(inbound) = parse_text_callback(&frame) {
                                    let preview =
                                        inbound.content.chars().take(60).collect::<String>();
                                    info!("WeCom inbound from {}: {}", inbound.sender, preview);
                                    if tx.send(inbound).await.is_err() {
                                        *self.outbound_tx.write().await = None;
                                        return Ok(());
                                    }
                                }
                            }
                            Ok(Some(Err(e))) => {
                                warn!("WeCom: WebSocket error: {}", e);
                                break;
                            }
                            Ok(None) => {
                                warn!("WeCom: WebSocket stream ended");
                                break;
                            }
                            Err(_) => {
                                if self.shutdown.load(Ordering::Relaxed) {
                                    let _ = ws_sink
                                        .send(tokio_tungstenite::tungstenite::Message::Close(None))
                                        .await;
                                    *self.outbound_tx.write().await = None;
                                    return Ok(());
                                }
                                ping_elapsed += POLL_INTERVAL;
                                if ping_elapsed >= PING_INTERVAL {
                                    ping_elapsed = std::time::Duration::ZERO;
                                    ws_sink
                                        .send(tokio_tungstenite::tungstenite::Message::Text(
                                            build_ping_frame().to_string(),
                                        ))
                                        .await
                                        .map_err(|e| {
                                            anyhow::anyhow!("WeCom heartbeat send failed: {}", e)
                                        })?;
                                }
                            }
                        }

                        if self.shutdown.load(Ordering::Relaxed) {
                            let _ = ws_sink
                                .send(tokio_tungstenite::tungstenite::Message::Close(None))
                                .await;
                            *self.outbound_tx.write().await = None;
                            return Ok(());
                        }
                    }

                    *self.outbound_tx.write().await = None;
                }
                Err(e) => {
                    warn!("WeCom: WebSocket connect failed: {}", e);
                }
            }

            if self.shutdown.load(Ordering::Relaxed) {
                return Ok(());
            }

            warn!("WeCom: reconnecting in {:?}", backoff);
            let mut remaining = backoff;
            let chunk = std::time::Duration::from_secs(1);
            while remaining > std::time::Duration::ZERO {
                if self.shutdown.load(Ordering::Relaxed) {
                    return Ok(());
                }
                tokio::time::sleep(chunk.min(remaining)).await;
                remaining = remaining.saturating_sub(chunk);
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }

        Ok(())
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }

    fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

fn build_subscribe_frame(config: &WecomConfig) -> (String, Value) {
    let req_id = new_req_id();
    (
        req_id.clone(),
        json!({
            "cmd": "aibot_subscribe",
            "headers": {
                "req_id": req_id,
            },
            "body": {
                "bot_id": config.bot_id,
                "secret": config.bot_secret,
            }
        }),
    )
}

fn build_ping_frame() -> Value {
    json!({
        "cmd": "ping",
        "headers": {
            "req_id": new_req_id(),
        }
    })
}

fn build_outbound_frame(msg: &OutboundMessage) -> Result<Value> {
    let content = msg.content.trim();
    if content.is_empty() {
        return Err(anyhow::anyhow!("WeCom message content is empty"));
    }

    if msg.reply_to.is_some() {
        if let Some(req_id) = msg
            .routing_state
            .as_ref()
            .and_then(|state| state["req_id"].as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(json!({
                "cmd": "aibot_respond_msg",
                "headers": {
                    "req_id": req_id,
                },
                "body": {
                    "msgtype": "markdown",
                    "markdown": {
                        "content": content,
                    }
                }
            }));
        }
    }

    let (chatid, chat_type) = parse_recipient(&msg.recipient)?;
    Ok(json!({
        "cmd": "aibot_send_msg",
        "headers": {
            "req_id": new_req_id(),
        },
        "body": {
            "chatid": chatid,
            "chat_type": chat_type,
            "msgtype": "markdown",
            "markdown": {
                "content": content,
            }
        }
    }))
}

fn parse_recipient(recipient: &str) -> Result<(String, u32)> {
    if let Some(chatid) = recipient.strip_prefix("group:") {
        let chatid = chatid.trim();
        if chatid.is_empty() {
            return Err(anyhow::anyhow!("WeCom group recipient is empty"));
        }
        return Ok((chatid.to_string(), 2));
    }
    if let Some(userid) = recipient.strip_prefix("user:") {
        let userid = userid.trim();
        if userid.is_empty() {
            return Err(anyhow::anyhow!("WeCom user recipient is empty"));
        }
        return Ok((userid.to_string(), 1));
    }

    let fallback = recipient.trim();
    if fallback.is_empty() {
        return Err(anyhow::anyhow!("WeCom recipient is empty"));
    }
    Ok((fallback.to_string(), 1))
}

fn parse_text_callback(frame: &Value) -> Option<InboundMessage> {
    if frame["cmd"].as_str()? != "aibot_msg_callback" {
        return None;
    }

    let req_id = frame["headers"]["req_id"].as_str()?.trim();
    let body = &frame["body"];
    if body["msgtype"].as_str()? != "text" {
        return None;
    }

    let sender = body["from"]["userid"].as_str()?.trim().to_string();
    let content = body["text"]["content"].as_str()?.trim().to_string();
    if sender.is_empty() || content.is_empty() || req_id.is_empty() {
        return None;
    }

    let is_group = body["chattype"].as_str() == Some("group");
    let chatid = if is_group {
        body["chatid"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        sender.clone()
    };
    if chatid.is_empty() {
        return None;
    }

    let reply_target = if is_group {
        format!("group:{}", chatid)
    } else {
        format!("user:{}", sender)
    };
    let conversation_key = if is_group {
        format!("group:{}", chatid)
    } else {
        format!("user:{}", sender)
    };
    let msg_id = body["msgid"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(req_id)
        .to_string();
    let timestamp = body["create_time"]
        .as_u64()
        .unwrap_or(0)
        .saturating_mul(1000);

    Some(InboundMessage {
        id: msg_id,
        channel: "wecom".to_string(),
        sender: sender.clone(),
        sender_name: Some(sender.clone()),
        content,
        reply_target,
        conversation_key: Some(conversation_key),
        is_group,
        group_name: None,
        timestamp,
        media: None,
        routing_state: Some(json!({
            "req_id": req_id,
            "chatid": chatid,
            "chat_type": if is_group { 2 } else { 1 },
        })),
    })
}

fn is_disconnected_event(frame: &Value) -> bool {
    frame["cmd"].as_str() == Some("aibot_event_callback")
        && frame["body"]["event"]["eventtype"].as_str() == Some("disconnected_event")
}

fn new_req_id() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_text_callback() {
        let frame = json!({
            "cmd": "aibot_msg_callback",
            "headers": {
                "req_id": "req-1",
            },
            "body": {
                "msgid": "msg-1",
                "create_time": 1700000000u64,
                "aibotid": "bot-1",
                "chattype": "single",
                "from": {
                    "userid": "zhangsan",
                },
                "msgtype": "text",
                "text": {
                    "content": "hello",
                }
            }
        });

        let msg = parse_text_callback(&frame).expect("single text callback");
        assert_eq!(msg.sender, "zhangsan");
        assert_eq!(msg.reply_target, "user:zhangsan");
        assert_eq!(msg.conversation_key.as_deref(), Some("user:zhangsan"));
        assert_eq!(msg.content, "hello");
        assert_eq!(msg.timestamp, 1700000000000);
        assert_eq!(msg.routing_state.unwrap()["req_id"], "req-1");
    }

    #[test]
    fn parses_group_text_callback() {
        let frame = json!({
            "cmd": "aibot_msg_callback",
            "headers": {
                "req_id": "req-2",
            },
            "body": {
                "msgid": "msg-2",
                "create_time": 1700000001u64,
                "aibotid": "bot-1",
                "chatid": "chat-1",
                "chattype": "group",
                "from": {
                    "userid": "lisi",
                },
                "msgtype": "text",
                "text": {
                    "content": "@Pisci hi",
                }
            }
        });

        let msg = parse_text_callback(&frame).expect("group text callback");
        assert!(msg.is_group);
        assert_eq!(msg.reply_target, "group:chat-1");
        assert_eq!(msg.conversation_key.as_deref(), Some("group:chat-1"));
        assert_eq!(msg.routing_state.unwrap()["chat_type"], 2);
    }

    #[test]
    fn builds_reply_frame_when_req_id_available() {
        let outbound = OutboundMessage {
            channel: "wecom".to_string(),
            recipient: "user:zhangsan".to_string(),
            content: "reply text".to_string(),
            reply_to: Some("msg-1".to_string()),
            media: None,
            routing_state: Some(json!({
                "req_id": "req-9"
            })),
        };

        let frame = build_outbound_frame(&outbound).expect("reply frame");
        assert_eq!(frame["cmd"], "aibot_respond_msg");
        assert_eq!(frame["headers"]["req_id"], "req-9");
        assert_eq!(frame["body"]["markdown"]["content"], "reply text");
    }

    #[test]
    fn builds_proactive_group_frame() {
        let outbound = OutboundMessage {
            channel: "wecom".to_string(),
            recipient: "group:chat-9".to_string(),
            content: "push text".to_string(),
            reply_to: None,
            media: None,
            routing_state: None,
        };

        let frame = build_outbound_frame(&outbound).expect("send frame");
        assert_eq!(frame["cmd"], "aibot_send_msg");
        assert_eq!(frame["body"]["chatid"], "chat-9");
        assert_eq!(frame["body"]["chat_type"], 2);
    }

    #[test]
    fn detects_disconnected_event() {
        let frame = json!({
            "cmd": "aibot_event_callback",
            "body": {
                "event": {
                    "eventtype": "disconnected_event"
                }
            }
        });
        assert!(is_disconnected_event(&frame));
    }
}
