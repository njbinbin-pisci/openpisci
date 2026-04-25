/// Feishu / Lark IM channel — WebSocket long connection using the official SDK protocol.
///
/// The official Go SDK (larksuite/oapi-sdk-go) uses a **Protobuf-encoded binary frame**
/// over WebSocket, NOT JSON text frames.  The protocol is:
///
/// 1. POST /callback/ws/endpoint  { AppID, AppSecret }  → { code, data: { URL, ClientConfig } }
/// 2. Dial the returned WSS URL (no extra query params needed — the URL already contains
///    device_id and service_id as query params)
/// 3. All messages are Binary WebSocket frames carrying a Protobuf-encoded `Frame` struct:
///    Frame { SeqID, LogID, Service, Method, Headers[], PayloadEncoding, PayloadType, Payload }
///    - Method 0 = Control frame  (ping/pong)
///    - Method 1 = Data frame     (event / card)
/// 4. Ping: send a Control frame with header type="ping" every PingInterval seconds
/// 5. ACK: for every Data frame received, send back the same frame with Payload replaced by
///    a JSON response  { "code": 200, "headers": { "messageId": "..." }, "data": "" }
use super::{Channel, ChannelStatus, InboundMessage, OutboundMessage};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

// ─── Config ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuConfig {
    pub app_id: String,
    pub app_secret: String,
    #[serde(default = "default_domain")]
    pub domain: String,
}

fn default_domain() -> String {
    "feishu".to_string()
}

impl FeishuConfig {
    fn base_url(&self) -> &str {
        if self.domain == "lark" {
            "https://open.larksuite.com"
        } else {
            "https://open.feishu.cn"
        }
    }
}

// ─── Token cache (for send_text only) ────────────────────────────────────────

struct TokenCache {
    token: String,
    expires_at: std::time::Instant,
}

// ─── Channel struct ───────────────────────────────────────────────────────────

pub struct FeishuChannel {
    config: FeishuConfig,
    http: Client,
    status: ChannelStatus,
    token_cache: Arc<RwLock<Option<TokenCache>>>,
    seen_messages: Arc<RwLock<HashMap<String, std::time::Instant>>>,
    /// Set to true by disconnect() to signal the listen() loop to exit.
    shutdown: Arc<AtomicBool>,
}

impl FeishuChannel {
    pub fn new(config: FeishuConfig) -> Self {
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
            seen_messages: Arc::new(RwLock::new(HashMap::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    // ── tenant_access_token (used only for send_text) ─────────────────────────

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
        let resp = self
            .http
            .post(&url)
            .json(&json!({
                "app_id": self.config.app_id,
                "app_secret": self.config.app_secret,
            }))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Network error reaching Feishu API: {}", e))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Invalid JSON from Feishu auth API: {}", e))?;

        if let Some(code) = body["code"].as_i64() {
            if code != 0 {
                let msg = body["msg"].as_str().unwrap_or("unknown error");
                return Err(anyhow::anyhow!(
                    "Feishu auth failed (code {}): {}. Check App ID and App Secret.",
                    code,
                    msg
                ));
            }
        }

        let token = body["tenant_access_token"]
            .as_str()
            .ok_or_else(|| {
                anyhow::anyhow!("Missing tenant_access_token in Feishu response: {:?}", body)
            })?
            .to_string();
        let expires_in = body["expire"].as_u64().unwrap_or(7200);

        let mut cache = self.token_cache.write().await;
        *cache = Some(TokenCache {
            token: token.clone(),
            expires_at: std::time::Instant::now()
                + std::time::Duration::from_secs(expires_in.saturating_sub(300)),
        });

        Ok(token)
    }

    // ── upload_image ──────────────────────────────────────────────────────────

    /// Upload raw image bytes to Feishu and return the image_key.
    /// image_type: "message" (for sending in chat) or "avatar".
    async fn upload_image(&self, data: &[u8], filename: &str) -> Result<String> {
        let token = self.get_tenant_access_token().await?;
        let url = format!("{}/open-apis/im/v1/images", self.config.base_url());

        // Detect content type from filename extension
        let mime = if filename.ends_with(".png") {
            "image/png"
        } else if filename.ends_with(".gif") {
            "image/gif"
        } else {
            "image/jpeg"
        };

        let part = reqwest::multipart::Part::bytes(data.to_vec())
            .file_name(filename.to_string())
            .mime_str(mime)?;
        let form = reqwest::multipart::Form::new()
            .text("image_type", "message")
            .part("image", part);

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .multipart(form)
            .send()
            .await?;

        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        if !status.is_success() || body["code"].as_i64().unwrap_or(0) != 0 {
            anyhow::bail!(
                "Feishu upload_image failed: HTTP {} code={} msg={}",
                status,
                body["code"],
                body["msg"].as_str().unwrap_or("unknown")
            );
        }

        let image_key = body["data"]["image_key"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing image_key in Feishu upload response"))?
            .to_string();

        info!("Feishu: uploaded image → image_key={}", image_key);
        Ok(image_key)
    }

    // ── send_image ────────────────────────────────────────────────────────────

    async fn send_image(
        &self,
        receive_id: &str,
        image_key: &str,
        reply_to: Option<&str>,
    ) -> Result<()> {
        let token = self.get_tenant_access_token().await?;
        let url = format!("{}/open-apis/im/v1/messages", self.config.base_url());

        let receive_id_type = if receive_id.starts_with("ou_") {
            "open_id"
        } else if receive_id.starts_with("on_") {
            "union_id"
        } else {
            "chat_id"
        };

        let mut body = json!({
            "receive_id": receive_id,
            "msg_type": "image",
            "content": serde_json::to_string(&json!({"image_key": image_key}))?,
        });
        if reply_to.is_some() {
            body["reply_in_thread"] = json!(true);
        }

        let resp = self
            .http
            .post(&url)
            .query(&[("receive_id_type", receive_id_type)])
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let resp_json: serde_json::Value = resp.json().await.unwrap_or_default();
        if !status.is_success() || resp_json["code"].as_i64().unwrap_or(0) != 0 {
            anyhow::bail!(
                "Feishu send_image failed: HTTP {} code={} msg={}",
                status,
                resp_json["code"],
                resp_json["msg"].as_str().unwrap_or("unknown")
            );
        }

        info!(
            "Feishu: sent image to {} (type={})",
            receive_id, receive_id_type
        );
        Ok(())
    }

    // ── upload_file ───────────────────────────────────────────────────────────

    /// Upload a file to Feishu and return the file_key.
    /// file_type: "opus" | "mp4" | "pdf" | "doc" | "xls" | "ppt" | "stream" (generic binary)
    async fn upload_file(&self, data: &[u8], filename: &str, file_type: &str) -> Result<String> {
        let token = self.get_tenant_access_token().await?;
        let url = format!("{}/open-apis/im/v1/files", self.config.base_url());

        let part = reqwest::multipart::Part::bytes(data.to_vec()).file_name(filename.to_string());
        let form = reqwest::multipart::Form::new()
            .text("file_type", file_type.to_string())
            .text("file_name", filename.to_string())
            .part("file", part);

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .multipart(form)
            .send()
            .await?;

        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        if !status.is_success() || body["code"].as_i64().unwrap_or(0) != 0 {
            anyhow::bail!(
                "Feishu upload_file failed: HTTP {} code={} msg={}",
                status,
                body["code"],
                body["msg"].as_str().unwrap_or("unknown")
            );
        }

        let file_key = body["data"]["file_key"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing file_key in Feishu upload response"))?
            .to_string();

        info!(
            "Feishu: uploaded file '{}' → file_key={}",
            filename, file_key
        );
        Ok(file_key)
    }

    // ── send_file ─────────────────────────────────────────────────────────────

    async fn send_file(
        &self,
        receive_id: &str,
        file_key: &str,
        reply_to: Option<&str>,
    ) -> Result<()> {
        let token = self.get_tenant_access_token().await?;
        let url = format!("{}/open-apis/im/v1/messages", self.config.base_url());

        let receive_id_type = if receive_id.starts_with("ou_") {
            "open_id"
        } else if receive_id.starts_with("on_") {
            "union_id"
        } else {
            "chat_id"
        };

        let mut body = json!({
            "receive_id": receive_id,
            "msg_type": "file",
            "content": serde_json::to_string(&json!({"file_key": file_key}))?,
        });
        if reply_to.is_some() {
            body["reply_in_thread"] = json!(true);
        }

        let resp = self
            .http
            .post(&url)
            .query(&[("receive_id_type", receive_id_type)])
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let resp_json: serde_json::Value = resp.json().await.unwrap_or_default();
        if !status.is_success() || resp_json["code"].as_i64().unwrap_or(0) != 0 {
            anyhow::bail!(
                "Feishu send_file failed: HTTP {} code={} msg={}",
                status,
                resp_json["code"],
                resp_json["msg"].as_str().unwrap_or("unknown")
            );
        }

        info!(
            "Feishu: sent file to {} (type={})",
            receive_id, receive_id_type
        );
        Ok(())
    }

    // ── send_text ─────────────────────────────────────────────────────────────

    async fn send_text(&self, receive_id: &str, text: &str, reply_to: Option<&str>) -> Result<()> {
        let token = self.get_tenant_access_token().await?;
        let url = format!("{}/open-apis/im/v1/messages", self.config.base_url());

        // Infer receive_id_type from the ID prefix:
        //   ou_ → open_id (p2p bot chat)
        //   oc_ → chat_id (group chat)
        //   on_ → union_id
        //   default → chat_id
        let receive_id_type = if receive_id.starts_with("ou_") {
            "open_id"
        } else if receive_id.starts_with("on_") {
            "union_id"
        } else {
            "chat_id"
        };

        let mut body = json!({
            "receive_id": receive_id,
            "msg_type": "text",
            "content": serde_json::to_string(&json!({"text": text}))?,
        });
        if reply_to.is_some() {
            body["reply_in_thread"] = json!(true);
        }
        let resp = self
            .http
            .post(&url)
            .query(&[("receive_id_type", receive_id_type)])
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Feishu send_text failed: HTTP {} — {}", status, body_text);
        }

        let resp_json: serde_json::Value = resp.json().await.unwrap_or_default();
        if resp_json["code"].as_i64().unwrap_or(0) != 0 {
            anyhow::bail!(
                "Feishu send_text API error: code={} msg={}",
                resp_json["code"],
                resp_json["msg"].as_str().unwrap_or("unknown")
            );
        }

        info!(
            "Feishu: sent message to {} (type={})",
            receive_id, receive_id_type
        );
        Ok(())
    }

    /// Map a MIME type (or filename extension) to a Feishu file_type string.
    /// Feishu supported file_type values: opus, mp4, pdf, doc, xls, ppt, stream
    fn mime_to_feishu_file_type<'a>(mime: &str, filename: &str) -> &'a str {
        if mime.contains("pdf") {
            return "pdf";
        }
        if mime.contains("mp4") || mime.contains("video") {
            return "mp4";
        }
        if mime.contains("opus") || mime.contains("ogg") {
            return "opus";
        }
        if mime.contains("word")
            || mime.contains("msword")
            || mime.contains("officedocument.wordprocessingml")
        {
            return "doc";
        }
        if mime.contains("excel") || mime.contains("spreadsheetml") || mime.contains("ms-excel") {
            return "xls";
        }
        if mime.contains("powerpoint") || mime.contains("presentationml") {
            return "ppt";
        }
        let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
        match ext.as_str() {
            "pdf" => "pdf",
            "mp4" | "mov" | "avi" | "mkv" => "mp4",
            "opus" | "ogg" => "opus",
            "doc" | "docx" => "doc",
            "xls" | "xlsx" | "csv" => "xls",
            "ppt" | "pptx" => "ppt",
            _ => "stream",
        }
    }
}

// ─── Channel trait impl ───────────────────────────────────────────────────────

#[async_trait]
impl Channel for FeishuChannel {
    fn name(&self) -> &str {
        "feishu"
    }

    async fn connect(&mut self) -> Result<()> {
        // Reset shutdown flag so listen() can run after reconnect
        self.shutdown.store(false, Ordering::Relaxed);
        self.status = ChannelStatus::Connecting;
        match self.get_tenant_access_token().await {
            Ok(_) => {
                self.status = ChannelStatus::Connected;
                info!(
                    "Feishu channel credentials verified (domain: {})",
                    self.config.domain
                );
                Ok(())
            }
            Err(e) => {
                self.status = ChannelStatus::Error(e.to_string());
                Err(e)
            }
        }
    }

    async fn disconnect(&mut self) -> Result<()> {
        // Signal the listen() loop to exit on next iteration
        self.shutdown.store(true, Ordering::Relaxed);
        self.status = ChannelStatus::Disconnected;
        info!("Feishu: disconnect requested, listener will stop");
        Ok(())
    }

    async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        if let Some(ref media) = msg.media {
            if let Some(ref data) = media.data {
                let filename = media.filename.as_deref().unwrap_or("file");
                let mime = media.media_type.as_str();

                if mime.starts_with("image/") {
                    // ── Image ──────────────────────────────────────────────
                    match self.upload_image(data, filename).await {
                        Ok(image_key) => {
                            self.send_image(&msg.recipient, &image_key, msg.reply_to.as_deref())
                                .await?;
                            if !msg.content.is_empty() {
                                self.send_text(
                                    &msg.recipient,
                                    &msg.content,
                                    msg.reply_to.as_deref(),
                                )
                                .await?;
                            }
                            return Ok(());
                        }
                        Err(e) => {
                            warn!("Feishu: image upload failed ({}), falling back to text", e)
                        }
                    }
                } else {
                    // ── File (PDF, Office docs, audio, video, binary, etc.) ─
                    // Map MIME type / filename extension to Feishu file_type
                    let file_type = Self::mime_to_feishu_file_type(mime, filename);
                    match self.upload_file(data, filename, file_type).await {
                        Ok(file_key) => {
                            self.send_file(&msg.recipient, &file_key, msg.reply_to.as_deref())
                                .await?;
                            if !msg.content.is_empty() {
                                self.send_text(
                                    &msg.recipient,
                                    &msg.content,
                                    msg.reply_to.as_deref(),
                                )
                                .await?;
                            }
                            return Ok(());
                        }
                        Err(e) => warn!("Feishu: file upload failed ({}), falling back to text", e),
                    }
                }
            }
        }
        // Default: send as text
        self.send_text(&msg.recipient, &msg.content, msg.reply_to.as_deref())
            .await
    }

    /// Feishu WebSocket long connection — implements the official SDK binary Protobuf protocol.
    ///
    /// Endpoint: POST /callback/ws/endpoint  { AppID, AppSecret }
    /// Frames:   Binary WebSocket carrying Protobuf-encoded Frame structs
    async fn listen(&self, tx: mpsc::Sender<InboundMessage>) -> Result<()> {
        info!("Feishu listener started (official SDK WebSocket protocol)");

        let config = self.config.clone();
        let seen_messages = self.seen_messages.clone();
        let http = self.http.clone();
        let shutdown = self.shutdown.clone();

        let mut backoff = std::time::Duration::from_secs(1);
        const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(60);

        loop {
            if shutdown.load(Ordering::Relaxed) {
                info!("Feishu: shutdown flag set, listener exiting");
                return Ok(());
            }
            // ── Step 1: Get WebSocket endpoint URL ────────────────────────────
            // The official SDK posts AppID + AppSecret directly (no token needed).
            let endpoint_url = format!("{}/callback/ws/endpoint", config.base_url());
            let ws_url = match http
                .post(&endpoint_url)
                .json(&json!({
                    "AppID": config.app_id,
                    "AppSecret": config.app_secret,
                }))
                .send()
                .await
            {
                Ok(resp) => match resp.json::<serde_json::Value>().await {
                    Ok(body) => {
                        let code = body["code"].as_i64().unwrap_or(-1);
                        if code != 0 {
                            warn!(
                                "Feishu WS endpoint error (code {}): {}",
                                code,
                                body["msg"].as_str().unwrap_or("unknown")
                            );
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(MAX_BACKOFF);
                            continue;
                        }
                        // Response: { code: 0, data: { URL: "wss://...", ClientConfig: {...} } }
                        match body["data"]["URL"].as_str() {
                            Some(u) => {
                                // Extract ping interval from ClientConfig if provided
                                let ping_secs = body["data"]["ClientConfig"]["PingInterval"]
                                    .as_u64()
                                    .unwrap_or(120);
                                (u.to_string(), ping_secs)
                            }
                            None => {
                                warn!(
                                    "Feishu WS endpoint: missing data.URL in response: {:?}",
                                    body
                                );
                                tokio::time::sleep(backoff).await;
                                backoff = (backoff * 2).min(MAX_BACKOFF);
                                continue;
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Feishu WS endpoint parse error: {}", e);
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                        continue;
                    }
                },
                Err(e) => {
                    warn!("Feishu WS endpoint request error: {}", e);
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    continue;
                }
            };

            let (ws_url_str, ping_interval_secs) = ws_url;
            info!("Feishu: connecting to WebSocket: {}", ws_url_str);

            // ── Step 2: Connect WebSocket ─────────────────────────────────────
            match tokio_tungstenite::connect_async(&ws_url_str).await {
                Ok((ws_stream, _)) => {
                    info!("Feishu: WebSocket connected (binary Protobuf protocol)");
                    backoff = std::time::Duration::from_secs(1);

                    use futures::{SinkExt, StreamExt};
                    let (mut ws_sink, mut ws_reader) = futures::StreamExt::split(ws_stream);
                    let ping_timeout = std::time::Duration::from_secs(ping_interval_secs);

                    // Extract service_id from the URL query params for ping frames.
                    // URL looks like: wss://host/connect?device_id=...&service_id=123&...
                    let service_id: i32 = ws_url_str
                        .split('?')
                        .nth(1)
                        .unwrap_or("")
                        .split('&')
                        .find_map(|part| {
                            let (k, v) = part.split_once('=')?;
                            if k == "service_id" {
                                v.parse::<i32>().ok()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);

                    // Poll interval: check shutdown every 2 seconds regardless of ping interval
                    let poll_interval = std::time::Duration::from_secs(2);
                    let mut ping_elapsed = std::time::Duration::ZERO;

                    loop {
                        tokio::select! {
                            // WebSocket message with short poll timeout
                            result = tokio::time::timeout(poll_interval, ws_reader.next()) => {
                                match result {
                                    Ok(Some(Ok(msg))) => {
                                        use tokio_tungstenite::tungstenite::Message;
                                        match msg {
                                            Message::Binary(bytes) => {
                                                match proto_decode_frame(&bytes) {
                                                    Ok(frame) => {
                                        handle_feishu_frame(
                                            frame,
                                            &mut ws_sink,
                                            &tx,
                                            &seen_messages,
                                            &http,
                                            &config,
                                        )
                                        .await;
                                                    }
                                                    Err(e) => {
                                                        warn!("Feishu: failed to decode frame: {} (bytes: {:?})", e, &bytes[..bytes.len().min(32)]);
                                                    }
                                                }
                                            }
                                            Message::Text(t) => {
                                                warn!("Feishu: unexpected text frame: {}", &t[..t.len().min(200)]);
                                            }
                                            Message::Ping(data) => {
                                                let _ = ws_sink.send(Message::Pong(data)).await;
                                            }
                                            Message::Pong(_) => {}
                                            Message::Close(_) => {
                                                warn!("Feishu: WebSocket closed by server");
                                                break;
                                            }
                                            _ => {}
                                        }
                                    }
                                    Ok(Some(Err(e))) => {
                                        warn!("Feishu: WebSocket error: {}", e);
                                        break;
                                    }
                                    Ok(None) => {
                                        warn!("Feishu: WebSocket stream ended");
                                        break;
                                    }
                                    Err(_) => {
                                        // poll_interval elapsed — check shutdown, then maybe ping
                                        if shutdown.load(Ordering::Relaxed) {
                                            info!("Feishu: shutdown requested, closing WebSocket");
                                            let _ = ws_sink.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                                            return Ok(());
                                        }
                                        ping_elapsed += poll_interval;
                                        if ping_elapsed >= ping_timeout {
                                            ping_elapsed = std::time::Duration::ZERO;
                                            let ping = proto_encode_ping_frame(service_id);
                                            let _ = ws_sink
                                                .send(tokio_tungstenite::tungstenite::Message::Binary(ping))
                                                .await;
                                        }
                                    }
                                }
                            }
                        }

                        // Check shutdown after every message too
                        if shutdown.load(Ordering::Relaxed) {
                            info!("Feishu: shutdown requested after message, closing WebSocket");
                            let _ = ws_sink
                                .send(tokio_tungstenite::tungstenite::Message::Close(None))
                                .await;
                            return Ok(());
                        }
                    }
                }
                Err(e) => {
                    warn!("Feishu: WebSocket connect failed: {}", e);
                }
            }

            if shutdown.load(Ordering::Relaxed) {
                info!("Feishu: shutdown requested, not reconnecting");
                return Ok(());
            }
            warn!("Feishu: reconnecting in {:?}", backoff);
            // Sleep in small chunks so shutdown is detected quickly
            let mut remaining = backoff;
            let chunk = std::time::Duration::from_secs(1);
            while remaining > std::time::Duration::ZERO {
                if shutdown.load(Ordering::Relaxed) {
                    info!("Feishu: shutdown during reconnect wait, exiting");
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
        info!("Feishu: shutdown flag set via request_shutdown()");
    }
}

// ─── Protobuf Frame handling ──────────────────────────────────────────────────

/// Decoded representation of the Feishu SDK Frame protobuf message.
#[derive(Debug)]
struct FeishuFrame {
    seq_id: u64,
    #[allow(dead_code)]
    log_id: u64,
    service: i32,
    method: i32, // 0 = Control, 1 = Data
    headers: Vec<(String, String)>,
    #[allow(dead_code)]
    payload_encoding: String,
    #[allow(dead_code)]
    payload_type: String,
    payload: Vec<u8>,
}

/// Handle a decoded Feishu frame: ACK data frames, parse events.
async fn handle_feishu_frame<S>(
    frame: FeishuFrame,
    ws_sink: &mut S,
    tx: &mpsc::Sender<InboundMessage>,
    seen_messages: &Arc<RwLock<HashMap<String, std::time::Instant>>>,
    http: &Client,
    config: &FeishuConfig,
) where
    S: futures::Sink<tokio_tungstenite::tungstenite::Message> + Unpin,
    S::Error: std::fmt::Debug,
{
    use futures::SinkExt;
    let method = frame.method;
    let msg_type = frame
        .headers
        .iter()
        .find(|(k, _)| k == "type")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    let msg_id = frame
        .headers
        .iter()
        .find(|(k, _)| k == "message_id")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();

    match method {
        // Control frame: only "ping" needs a pong; "pong" replies to our own ping, nothing to do.
        0 if msg_type == "ping" => {
            let pong = build_pong_frame(&frame);
            let _ = ws_sink
                .send(tokio_tungstenite::tungstenite::Message::Binary(pong))
                .await;
        }
        0 => {}
        1 => {
            // Data frame — ACK first
            let ack = build_ack_frame(&frame);
            let _ = ws_sink
                .send(tokio_tungstenite::tungstenite::Message::Binary(ack))
                .await;

            if msg_type == "event" {
                parse_and_dispatch_event(frame, msg_id, tx, seen_messages, http, config).await;
            }
        }
        _ => {}
    }
}

async fn parse_and_dispatch_event(
    frame: FeishuFrame,
    msg_id: String,
    tx: &mpsc::Sender<InboundMessage>,
    seen_messages: &Arc<RwLock<HashMap<String, std::time::Instant>>>,
    http: &Client,
    config: &FeishuConfig,
) {
    // Deduplicate
    {
        let mut seen = seen_messages.write().await;
        let now = std::time::Instant::now();
        seen.retain(|_, t| now.duration_since(*t).as_secs() < 300);
        if seen.contains_key(&msg_id) {
            return;
        }
        seen.insert(msg_id.clone(), now);
    }

    // Payload is JSON event body
    let payload_str = match std::str::from_utf8(&frame.payload) {
        Ok(s) => s,
        Err(_) => return,
    };

    let event: serde_json::Value = match serde_json::from_str(payload_str) {
        Ok(v) => v,
        Err(e) => {
            warn!("Feishu: failed to parse event JSON: {}", e);
            return;
        }
    };

    // Support both schema v1 and v2 event envelopes
    let event_type = event["header"]["event_type"]
        .as_str()
        .or_else(|| event["event"]["header"]["event_type"].as_str())
        .unwrap_or("");

    if event_type != "im.message.receive_v1" {
        return;
    }

    let ev = &event["event"];
    let message_id = ev["message"]["message_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let sender_open_id = ev["sender"]["sender_id"]["open_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let chat_id = ev["message"]["chat_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let chat_type = ev["message"]["chat_type"].as_str().unwrap_or("p2p");
    let msg_type = ev["message"]["message_type"].as_str().unwrap_or("text");
    let content_str = ev["message"]["content"].as_str().unwrap_or("{}");

    // Parse content and optionally download media
    let (text_content, media) = match msg_type {
        "text" => {
            let text = serde_json::from_str::<serde_json::Value>(content_str)
                .ok()
                .and_then(|v| v["text"].as_str().map(String::from))
                .unwrap_or_default();
            (text, None)
        }
        "image" => {
            // Parse image_key from content JSON: {"image_key": "img_xxx"}
            let image_key = serde_json::from_str::<serde_json::Value>(content_str)
                .ok()
                .and_then(|v| v["image_key"].as_str().map(String::from))
                .unwrap_or_default();

            if image_key.is_empty() {
                ("[图片]".to_string(), None)
            } else {
                // Download image bytes using tenant_access_token
                let media = download_feishu_image(http, config, &image_key).await;
                let text = if media.is_some() {
                    "[图片]".to_string()
                } else {
                    format!("[图片: {}]", image_key)
                };
                (text, media)
            }
        }
        "sticker" => ("[表情包]".to_string(), None),
        "audio" => ("[语音消息]".to_string(), None),
        "media" => ("[视频消息]".to_string(), None),
        "file" => {
            let file_key = serde_json::from_str::<serde_json::Value>(content_str)
                .ok()
                .and_then(|v| v["file_key"].as_str().map(String::from))
                .unwrap_or_default();
            (format!("[文件: {}]", file_key), None)
        }
        other => (format!("[{}]", other), None),
    };

    if text_content.is_empty() {
        return;
    }

    let reply_target = if chat_type == "p2p" {
        sender_open_id.clone()
    } else {
        chat_id.clone()
    };

    let inbound = InboundMessage {
        id: message_id,
        channel: "feishu".to_string(),
        sender: sender_open_id.clone(),
        sender_name: None,
        content: text_content,
        reply_target,
        conversation_key: Some(if chat_type == "group" {
            format!("chat:{}", chat_id)
        } else {
            format!("user:{}", sender_open_id)
        }),
        is_group: chat_type == "group",
        group_name: None,
        timestamp: 0,
        media,
        routing_state: None,
    };

    if tx.send(inbound).await.is_err() {
        // receiver dropped — exit silently
    }
}

/// Download a Feishu image by image_key, returning a MediaAttachment with raw bytes.
async fn download_feishu_image(
    http: &Client,
    config: &FeishuConfig,
    image_key: &str,
) -> Option<super::MediaAttachment> {
    // Get tenant_access_token
    let token = {
        let url = format!(
            "{}/open-apis/auth/v3/tenant_access_token/internal",
            config.base_url()
        );
        let resp = http
            .post(&url)
            .json(&json!({"app_id": config.app_id, "app_secret": config.app_secret}))
            .send()
            .await
            .ok()?;
        let body: serde_json::Value = resp.json().await.ok()?;
        body["tenant_access_token"].as_str()?.to_string()
    };

    // Download image: GET /open-apis/im/v1/images/{image_key}
    let url = format!("{}/open-apis/im/v1/images/{}", config.base_url(), image_key);
    let resp = http.get(&url).bearer_auth(&token).send().await.ok()?;

    if !resp.status().is_success() {
        warn!(
            "Feishu: failed to download image {}: HTTP {}",
            image_key,
            resp.status()
        );
        return None;
    }

    // Detect MIME from Content-Type header
    let mime = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/jpeg")
        .split(';')
        .next()
        .unwrap_or("image/jpeg")
        .trim()
        .to_string();

    let bytes = resp.bytes().await.ok()?.to_vec();
    if bytes.is_empty() {
        return None;
    }

    info!(
        "Feishu: downloaded image {} ({} bytes, {})",
        image_key,
        bytes.len(),
        mime
    );

    // Determine extension from MIME
    let ext = match mime.as_str() {
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "jpg",
    };
    let filename = format!("feishu_{}.{}", image_key, ext);

    Some(super::MediaAttachment {
        media_type: mime,
        url: None,
        data: Some(bytes),
        filename: Some(filename),
    })
}

// ─── Protobuf encode/decode ───────────────────────────────────────────────────
//
// The Frame message (pbbp2.proto) uses standard proto2 encoding:
//   field 1 (SeqID)            varint  required
//   field 2 (LogID)            varint  required
//   field 3 (service)          varint  required
//   field 4 (method)           varint  required
//   field 5 (headers)          length-delimited repeated (embedded Header messages)
//   field 6 (payload_encoding) length-delimited optional
//   field 7 (payload_type)     length-delimited optional
//   field 8 (payload)          length-delimited optional
//   field 9 (LogIDNew)         length-delimited optional
//
// Header message:
//   field 1 (key)   length-delimited required
//   field 2 (value) length-delimited required

fn proto_decode_frame(buf: &[u8]) -> Result<FeishuFrame> {
    let mut pos = 0;
    let mut seq_id: u64 = 0;
    let mut log_id: u64 = 0;
    let mut service: i32 = 0;
    let mut method: i32 = 0;
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut payload_encoding = String::new();
    let mut payload_type = String::new();
    let mut payload: Vec<u8> = Vec::new();

    while pos < buf.len() {
        let (tag_wire, n) = read_varint(buf, pos)?;
        pos += n;
        let field_num = (tag_wire >> 3) as u32;
        let wire_type = tag_wire & 0x7;

        match (field_num, wire_type) {
            (1, 0) => {
                let (v, n) = read_varint(buf, pos)?;
                seq_id = v;
                pos += n;
            }
            (2, 0) => {
                let (v, n) = read_varint(buf, pos)?;
                log_id = v;
                pos += n;
            }
            (3, 0) => {
                let (v, n) = read_varint(buf, pos)?;
                service = v as i32;
                pos += n;
            }
            (4, 0) => {
                let (v, n) = read_varint(buf, pos)?;
                method = v as i32;
                pos += n;
            }
            (5, 2) => {
                let (len, n) = read_varint(buf, pos)?;
                pos += n;
                let end = pos + len as usize;
                if end > buf.len() {
                    return Err(anyhow::anyhow!("buffer overflow reading header"));
                }
                let h = decode_header(&buf[pos..end])?;
                headers.push(h);
                pos = end;
            }
            (6, 2) => {
                let (len, n) = read_varint(buf, pos)?;
                pos += n;
                let end = pos + len as usize;
                payload_encoding = String::from_utf8_lossy(&buf[pos..end]).into_owned();
                pos = end;
            }
            (7, 2) => {
                let (len, n) = read_varint(buf, pos)?;
                pos += n;
                let end = pos + len as usize;
                payload_type = String::from_utf8_lossy(&buf[pos..end]).into_owned();
                pos = end;
            }
            (8, 2) => {
                let (len, n) = read_varint(buf, pos)?;
                pos += n;
                let end = pos + len as usize;
                if end > buf.len() {
                    return Err(anyhow::anyhow!("buffer overflow reading payload"));
                }
                payload = buf[pos..end].to_vec();
                pos = end;
            }
            (9, 2) => {
                // LogIDNew — skip
                let (len, n) = read_varint(buf, pos)?;
                pos += n + len as usize;
            }
            (_, 0) => {
                let (_, n) = read_varint(buf, pos)?;
                pos += n;
            }
            (_, 2) => {
                let (len, n) = read_varint(buf, pos)?;
                pos += n + len as usize;
            }
            _ => break,
        }
    }

    Ok(FeishuFrame {
        seq_id,
        log_id,
        service,
        method,
        headers,
        payload_encoding,
        payload_type,
        payload,
    })
}

fn decode_header(buf: &[u8]) -> Result<(String, String)> {
    let mut pos = 0;
    let mut key = String::new();
    let mut value = String::new();
    while pos < buf.len() {
        let (tag_wire, n) = read_varint(buf, pos)?;
        pos += n;
        let field_num = (tag_wire >> 3) as u32;
        let wire_type = tag_wire & 0x7;
        if wire_type == 2 {
            let (len, n) = read_varint(buf, pos)?;
            pos += n;
            let end = pos + len as usize;
            let s = String::from_utf8_lossy(&buf[pos..end]).into_owned();
            pos = end;
            match field_num {
                1 => key = s,
                2 => value = s,
                _ => {}
            }
        } else if wire_type == 0 {
            let (_, n) = read_varint(buf, pos)?;
            pos += n;
        } else {
            break;
        }
    }
    Ok((key, value))
}

fn read_varint(buf: &[u8], mut pos: usize) -> Result<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    let start = pos;
    loop {
        if pos >= buf.len() {
            return Err(anyhow::anyhow!("varint overflow at pos {}", pos));
        }
        let b = buf[pos] as u64;
        pos += 1;
        result |= (b & 0x7f) << shift;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            return Err(anyhow::anyhow!("varint too long"));
        }
    }
    Ok((result, pos - start))
}

fn write_varint(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            buf.push(byte);
            break;
        } else {
            buf.push(byte | 0x80);
        }
    }
}

fn write_length_delimited(buf: &mut Vec<u8>, field: u32, data: &[u8]) {
    write_varint(buf, ((field << 3) | 2) as u64);
    write_varint(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

fn write_varint_field(buf: &mut Vec<u8>, field: u32, v: u64) {
    write_varint(buf, (field << 3) as u64);
    write_varint(buf, v);
}

fn encode_header(key: &str, value: &str) -> Vec<u8> {
    let mut h = Vec::new();
    write_length_delimited(&mut h, 1, key.as_bytes());
    write_length_delimited(&mut h, 2, value.as_bytes());
    h
}

fn proto_encode_frame(
    seq_id: u64,
    service: i32,
    method: i32,
    headers: &[(&str, &str)],
    payload: &[u8],
) -> Vec<u8> {
    let mut buf = Vec::new();
    write_varint_field(&mut buf, 1, seq_id);
    write_varint_field(&mut buf, 2, 0); // LogID = 0
    write_varint_field(&mut buf, 3, service as u64);
    write_varint_field(&mut buf, 4, method as u64);
    for (k, v) in headers {
        let h = encode_header(k, v);
        write_length_delimited(&mut buf, 5, &h);
    }
    if !payload.is_empty() {
        write_length_delimited(&mut buf, 8, payload);
    }
    buf
}

/// Build a Protobuf-encoded Ping control frame (method=0, type=ping).
fn proto_encode_ping_frame(service_id: i32) -> Vec<u8> {
    proto_encode_frame(
        next_seq_id(),
        service_id,
        0, // Control
        &[("type", "ping")],
        &[],
    )
}

/// Build an ACK response frame for a received data frame.
fn build_ack_frame(frame: &FeishuFrame) -> Vec<u8> {
    let msg_id = frame
        .headers
        .iter()
        .find(|(k, _)| k == "message_id")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");

    let resp = json!({
        "code": 200,
        "headers": { "messageId": msg_id },
        "data": ""
    });
    let payload = resp.to_string().into_bytes();

    // Copy headers from the incoming frame for the response
    let headers: Vec<(&str, &str)> = frame
        .headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    proto_encode_frame(
        frame.seq_id,
        frame.service,
        frame.method,
        &headers,
        &payload,
    )
}

/// Build a Pong control frame in response to a server Ping.
fn build_pong_frame(frame: &FeishuFrame) -> Vec<u8> {
    let headers: Vec<(&str, &str)> = [("type", "pong")].iter().map(|(k, v)| (*k, *v)).collect();
    proto_encode_frame(frame.seq_id, frame.service, 0, &headers, &[])
}

static SEQ_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_seq_id() -> u64 {
    SEQ_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
