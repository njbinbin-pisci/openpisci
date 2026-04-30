/// WeChat iLink channel.
///
/// In the direct QR-bind flow used by Pisci, we talk to Tencent iLink directly
/// by long-polling `getupdates` with the bound `bot_token`. For compatibility
/// with the older OpenClaw plugin contract, we still keep a local HTTP server
/// fallback when no `bot_token` is configured.
///
/// Endpoints implemented (all POST, path prefix `/ilink/bot/`):
///   getupdates   – long-poll for new messages (35 s server-side timeout)
///   sendmessage  – stub (plugin never calls this; we push via getupdates)
///   getconfig    – returns a typing ticket stub
///   sendtyping   – no-op 200
///   getuploadurl – local fallback stub; direct iLink mode uses the real API
///
/// The original OpenClaw Gateway WebSocket compatibility layer that was here
/// previously is preserved in git history and can be reused for future
/// OpenClaw iOS/Android client support.
use super::{Channel, ChannelStatus, InboundMessage, MediaAttachment, OutboundMessage};
use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use ecb::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyInit};
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex, Notify};
use tracing::{info, warn};

type Aes128EcbEnc = ecb::Encryptor<aes::Aes128>;

const WECHAT_CDN_BASE_URL: &str = "https://novac2c.cdn.weixin.qq.com/c2c";
const UPLOAD_MEDIA_TYPE_IMAGE: u8 = 1;
const UPLOAD_MEDIA_TYPE_FILE: u8 = 3;
const MESSAGE_ITEM_TYPE_TEXT: u8 = 1;
const MESSAGE_ITEM_TYPE_IMAGE: u8 = 2;
const MESSAGE_ITEM_TYPE_FILE: u8 = 4;

// ── Config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WechatConfig {
    /// Optional Bearer token for the local HTTP server (guards the listener).
    pub gateway_token: String,
    /// TCP port for the local iLink Bot HTTP server (default 18788).
    pub port: u16,
    /// bot_token obtained after QR-code login; used to authenticate outbound
    /// sendmessage calls to the iLink API on behalf of the bound WeChat account.
    pub bot_token: String,
    /// Base URL for the iLink API (e.g. https://ilinkai.weixin.qq.com).
    /// Returned by the login API; may vary per account.
    pub base_url: String,
}

// ── Shared state between the HTTP server and the Channel::send() path ────────

struct WechatState {
    /// Messages queued for the next getupdates response.
    pending: Mutex<Vec<Value>>,
    /// Notified whenever a new message is pushed into `pending`.
    notify: Notify,
    /// Opaque cursor returned to the plugin so it can detect missed messages.
    sync_buf: Mutex<String>,
    /// Latest reply routing context keyed by WeChat peer id.
    reply_contexts: Mutex<HashMap<String, ReplyContext>>,
}

#[derive(Debug, Clone, Default)]
struct ReplyContext {
    context_token: String,
    session_id: String,
}

impl WechatState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(Vec::new()),
            notify: Notify::new(),
            sync_buf: Mutex::new(String::new()),
            reply_contexts: Mutex::new(HashMap::new()),
        })
    }

    async fn remember_reply_context(&self, user_id: &str, session_id: &str, context_token: &str) {
        if user_id.is_empty() {
            return;
        }

        let mut reply_contexts = self.reply_contexts.lock().await;
        let entry = reply_contexts.entry(user_id.to_string()).or_default();
        if !session_id.is_empty() {
            entry.session_id = session_id.to_string();
        }
        if !context_token.is_empty() {
            entry.context_token = context_token.to_string();
        }
    }

    async fn resolve_reply_context(&self, user_id: &str, context_token: &str) -> ReplyContext {
        if user_id.is_empty() {
            return ReplyContext::default();
        }

        let reply_contexts = self.reply_contexts.lock().await;
        if let Some(cached) = reply_contexts.get(user_id) {
            ReplyContext {
                context_token: if context_token.is_empty() {
                    cached.context_token.clone()
                } else {
                    context_token.to_string()
                },
                session_id: cached.session_id.clone(),
            }
        } else {
            ReplyContext {
                context_token: context_token.to_string(),
                session_id: String::new(),
            }
        }
    }
}

// ── Channel implementation ────────────────────────────────────────────────────

pub struct WechatChannel {
    config: WechatConfig,
    status: ChannelStatus,
    shutdown: Arc<AtomicBool>,
    state: Arc<WechatState>,
}

impl WechatChannel {
    pub fn new(config: WechatConfig) -> Self {
        Self {
            config,
            status: ChannelStatus::Disconnected,
            shutdown: Arc::new(AtomicBool::new(false)),
            state: WechatState::new(),
        }
    }
}

#[async_trait]
impl Channel for WechatChannel {
    fn name(&self) -> &str {
        "wechat"
    }

    async fn connect(&mut self) -> Result<()> {
        self.shutdown.store(false, Ordering::Relaxed);
        self.status = ChannelStatus::Connected;
        if self.config.bot_token.is_empty() {
            info!(
                "WeChat compatibility HTTP server ready (will listen on 127.0.0.1:{})",
                self.config.port
            );
        } else {
            info!("WeChat iLink direct listener ready (bot token configured)");
        }
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.shutdown.store(true, Ordering::Relaxed);
        self.status = ChannelStatus::Disconnected;
        info!("WeChat iLink HTTP server disconnected");
        Ok(())
    }

    /// Send a reply to the WeChat user.
    ///
    /// If a `bot_token` is configured (i.e. the user has completed QR login),
    /// we call the real iLink `sendmessage` API directly.  Otherwise we fall
    /// back to the local pending-queue mechanism (useful for testing without
    /// a real WeChat account).
    async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        if !self.config.bot_token.is_empty() {
            let base = if self.config.base_url.is_empty() {
                "https://ilinkai.weixin.qq.com"
            } else {
                &self.config.base_url
            };
            // Parse recipient: "user_id|context_token" or just "user_id"
            let (to_user_id, context_token) = if let Some(idx) = msg.recipient.find('|') {
                (
                    msg.recipient[..idx].to_string(),
                    msg.recipient[idx + 1..].to_string(),
                )
            } else {
                (msg.recipient.clone(), String::new())
            };
            let routing_state = msg.routing_state.as_ref();
            let routing_context_token = routing_state
                .and_then(|value| value["context_token"].as_str())
                .unwrap_or("");
            let routing_session_id = routing_state
                .and_then(|value| value["session_id"].as_str())
                .unwrap_or("");
            let effective_context_token = if context_token.is_empty() {
                routing_context_token
            } else {
                &context_token
            };

            let reply_context = self
                .state
                .resolve_reply_context(&to_user_id, effective_context_token)
                .await;
            let effective_session_id = if reply_context.session_id.is_empty() {
                routing_session_id.to_string()
            } else {
                reply_context.session_id.clone()
            };
            if context_token.is_empty() && !reply_context.context_token.is_empty() {
                info!(
                    "WeChat sendmessage reusing cached context_token for {}",
                    to_user_id
                );
            }
            if reply_context.context_token.is_empty() {
                warn!(
                    "WeChat sendmessage to {} has no context_token; delivery may be dropped",
                    to_user_id
                );
            }

            let client = reqwest::Client::new();
            let bodies = build_outbound_sendmessage_bodies(
                &client,
                base,
                &self.config.bot_token,
                &to_user_id,
                &effective_session_id,
                &reply_context.context_token,
                msg,
            )
            .await
            .map_err(|e| {
                warn!("WeChat media preparation failed: {}", e);
                e
            })?;

            for body in bodies {
                post_sendmessage(&client, base, &self.config.bot_token, &to_user_id, &body).await?;
            }

            self.state
                .remember_reply_context(
                    &to_user_id,
                    &effective_session_id,
                    &reply_context.context_token,
                )
                .await;
        } else {
            // No bot_token yet — queue locally (plugin will pick up via getupdates)
            let weixin_msg = outbound_to_weixin_message(msg);
            let mut pending = self.state.pending.lock().await;
            pending.push(weixin_msg);
            drop(pending);
            self.state.notify.notify_waiters();
        }
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<InboundMessage>) -> Result<()> {
        if !self.config.bot_token.is_empty() {
            return listen_ilink_updates(
                self.config.clone(),
                self.shutdown.clone(),
                self.state.clone(),
                tx,
            )
            .await;
        }

        let bind_addr = format!("127.0.0.1:{}", self.config.port);
        let listener = TcpListener::bind(&bind_addr).await.map_err(|e| {
            anyhow::anyhow!("WeChat HTTP server: failed to bind {}: {}", bind_addr, e)
        })?;
        info!(
            "WeChat iLink Bot HTTP server listening on {} (loopback only)",
            bind_addr
        );

        let shutdown = self.shutdown.clone();
        let token = self.config.gateway_token.clone();
        let state = self.state.clone();

        loop {
            if shutdown.load(Ordering::Relaxed) {
                info!("WeChat HTTP server: shutdown, stopping listener");
                return Ok(());
            }

            let accept =
                tokio::time::timeout(std::time::Duration::from_secs(2), listener.accept()).await;

            match accept {
                Ok(Ok((stream, addr))) => {
                    let tx = tx.clone();
                    let token = token.clone();
                    let state = state.clone();
                    let shutdown = shutdown.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_http(stream, tx, &token, state, shutdown).await {
                            warn!("WeChat HTTP: connection error from {}: {}", addr, e);
                        }
                    });
                }
                Ok(Err(e)) => {
                    warn!("WeChat HTTP server: accept error: {}", e);
                }
                Err(_) => {
                    // Timeout — loop back to check shutdown flag
                }
            }
        }
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }

    fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        // Wake any waiting long-polls so they can exit cleanly
        self.state.notify.notify_waiters();
        info!("WeChat HTTP server: shutdown flag set");
    }
}

async fn listen_ilink_updates(
    config: WechatConfig,
    shutdown: Arc<AtomicBool>,
    state: Arc<WechatState>,
    tx: mpsc::Sender<InboundMessage>,
) -> Result<()> {
    let client = reqwest::Client::new();
    let base = if config.base_url.is_empty() {
        "https://ilinkai.weixin.qq.com".to_string()
    } else {
        config.base_url.trim_end_matches('/').to_string()
    };
    let url = format!("{}/ilink/bot/getupdates", base);
    let mut cursor = String::new();

    info!("WeChat direct long-poll listener started against {}", base);

    while !shutdown.load(Ordering::Relaxed) {
        let body = json!({
            "get_updates_buf": cursor,
            "base_info": {
                "channel_version": "2.0.0"
            }
        });

        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", config.bot_token))
            .header("AuthorizationType", "ilink_bot_token")
            .header("Content-Type", "application/json")
            .header("X-WECHAT-UIN", build_wechat_uin())
            .header("iLink-App-ClientVersion", "1")
            .json(&body)
            .timeout(std::time::Duration::from_secs(38))
            .send()
            .await;

        let payload: Value = match response {
            Ok(resp) if resp.status().is_success() => match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    warn!("WeChat getupdates: failed to parse JSON: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
            },
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                warn!("WeChat getupdates HTTP {}: {}", status, text);
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            }
            Err(e) => {
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
                warn!("WeChat getupdates error: {}", e);
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            }
        };

        if payload["errcode"].as_i64() == Some(-14) || payload["ret"].as_i64() == Some(-14) {
            warn!("WeChat session expired; please bind/login again");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            continue;
        }

        if let Some(next_cursor) = payload["get_updates_buf"].as_str() {
            cursor = next_cursor.to_string();
        }

        cache_reply_contexts_from_payload(&state, &payload).await;
        for inbound in extract_inbound_messages(&payload) {
            let sender = inbound.sender.clone();
            let preview = inbound.content.chars().take(60).collect::<String>();
            info!("WeChat inbound from {}: {}", sender, preview);
            if tx.send(inbound).await.is_err() {
                warn!("WeChat inbound consumer dropped");
                return Ok(());
            }
        }
    }

    info!("WeChat direct long-poll listener stopped");
    Ok(())
}

// ── HTTP connection handler ───────────────────────────────────────────────────

/// Read one HTTP request from `stream`, dispatch to the appropriate handler,
/// and write the response.  We only need to handle a handful of fixed POST
/// paths so a full HTTP library is not necessary.
async fn handle_http(
    mut stream: tokio::net::TcpStream,
    tx: mpsc::Sender<InboundMessage>,
    gateway_token: &str,
    state: Arc<WechatState>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    // Read until we have the full headers + body.
    let mut buf = vec![0u8; 65536];
    let mut total = 0usize;

    loop {
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            stream.read(&mut buf[total..]),
        )
        .await
        .map_err(|_| anyhow::anyhow!("read timeout"))??;

        if n == 0 {
            break;
        }
        total += n;

        // Check if we have a complete HTTP request (headers + body).
        if let Some(body_start) = find_header_end(&buf[..total]) {
            let raw = &buf[..total];
            let header_str = std::str::from_utf8(&raw[..body_start]).unwrap_or("");

            // Parse method and path from the request line.
            let first_line = header_str.lines().next().unwrap_or("");
            let mut parts = first_line.split_whitespace();
            let method = parts.next().unwrap_or("");
            let path = parts.next().unwrap_or("");

            // Extract Content-Length so we know how many body bytes to read.
            let content_length = extract_content_length(header_str);
            let body_bytes_available = total - body_start;

            // If we haven't read the full body yet, keep reading.
            if body_bytes_available < content_length {
                if total >= buf.len() {
                    // Grow buffer
                    buf.resize(buf.len() + 65536, 0);
                }
                continue;
            }

            let body = &raw[body_start..body_start + content_length];
            let body_json: Value = serde_json::from_slice(body).unwrap_or(json!({}));

            // Auth check
            if !gateway_token.is_empty() {
                let auth = extract_header(header_str, "Authorization");
                let expected = format!("Bearer {}", gateway_token);
                if auth.trim() != expected.trim() {
                    write_json_response(
                        &mut stream,
                        401,
                        &json!({"ret": -1, "errmsg": "unauthorized"}),
                    )
                    .await?;
                    return Ok(());
                }
            }

            if method != "POST" {
                write_json_response(
                    &mut stream,
                    405,
                    &json!({"ret": -1, "errmsg": "method not allowed"}),
                )
                .await?;
                return Ok(());
            }

            let response = dispatch(path, &body_json, &tx, &state, &shutdown).await;
            write_json_response(&mut stream, 200, &response).await?;
            return Ok(());
        }

        if total >= buf.len() {
            buf.resize(buf.len() + 65536, 0);
        }
    }

    Ok(())
}

/// Route a POST request to the appropriate handler.
async fn dispatch(
    path: &str,
    body: &Value,
    tx: &mpsc::Sender<InboundMessage>,
    state: &Arc<WechatState>,
    shutdown: &Arc<AtomicBool>,
) -> Value {
    // Strip any leading path components; we only care about the last segment.
    let endpoint = path
        .trim_start_matches('/')
        .split('/')
        .next_back()
        .unwrap_or("");

    match endpoint {
        "getupdates" => handle_getupdates(body, tx, state, shutdown).await,
        "sendmessage" => json!({ "ret": 0 }),
        "getconfig" => {
            let user_id = body["ilink_user_id"].as_str().unwrap_or("");
            info!("WeChat getconfig for user {}", user_id);
            json!({ "ret": 0, "typing_ticket": "" })
        }
        "sendtyping" => json!({ "ret": 0 }),
        "getuploadurl" => json!({ "ret": 0, "upload_param": "", "thumb_upload_param": "" }),
        _ => {
            warn!("WeChat HTTP: unknown endpoint: {}", endpoint);
            json!({ "ret": -1, "errmsg": format!("unknown endpoint: {}", endpoint) })
        }
    }
}

/// Long-poll handler: waits up to 35 s for a message to appear in the pending
/// queue, then returns it.  If the shutdown flag is set while waiting, returns
/// an empty response so the plugin can reconnect or stop.
async fn handle_getupdates(
    body: &Value,
    tx: &mpsc::Sender<InboundMessage>,
    state: &Arc<WechatState>,
    shutdown: &Arc<AtomicBool>,
) -> Value {
    let sync_buf_in = body["get_updates_buf"].as_str().unwrap_or("").to_string();

    // Check for inbound messages embedded in the getupdates request.
    // The plugin sends user messages as part of the getupdates body when
    // `msgs` is present (push-style variant).
    cache_reply_contexts_from_payload(state, body).await;
    for inbound in extract_inbound_messages(body) {
        let _ = tx.send(inbound).await;
    }

    // Wait for a reply to become available (or timeout / shutdown).
    const LONG_POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(35);

    let wait = tokio::time::timeout(LONG_POLL_TIMEOUT, state.notify.notified());

    // If there are already pending messages, skip the wait.
    let has_pending = {
        let p = state.pending.lock().await;
        !p.is_empty()
    };

    if !has_pending && !shutdown.load(Ordering::Relaxed) {
        let _ = wait.await;
    }

    if shutdown.load(Ordering::Relaxed) {
        return json!({
            "ret": 0,
            "msgs": [],
            "get_updates_buf": sync_buf_in,
            "longpolling_timeout_ms": 5000,
        });
    }

    // Drain the pending queue.
    let msgs: Vec<Value> = {
        let mut pending = state.pending.lock().await;
        std::mem::take(&mut *pending)
    };

    // Update the sync cursor.
    let new_buf = {
        let mut buf = state.sync_buf.lock().await;
        *buf = format!("{}", now_ms());
        buf.clone()
    };

    json!({
        "ret": 0,
        "msgs": msgs,
        "get_updates_buf": new_buf,
        "longpolling_timeout_ms": 35000,
    })
}

// ── Message conversion helpers ────────────────────────────────────────────────

fn extract_inbound_messages(payload: &Value) -> Vec<InboundMessage> {
    let mut out = Vec::new();
    if let Some(msgs) = payload["msgs"].as_array() {
        out.extend(msgs.iter().filter_map(weixin_message_to_inbound));
    }
    if payload["msg"].is_object() {
        if let Some(inbound) = weixin_message_to_inbound(&payload["msg"]) {
            out.push(inbound);
        }
    }
    out
}

async fn cache_reply_contexts_from_payload(state: &Arc<WechatState>, payload: &Value) {
    if let Some(msgs) = payload["msgs"].as_array() {
        for msg in msgs {
            cache_reply_context_from_message(state, msg).await;
        }
    }
    if payload["msg"].is_object() {
        cache_reply_context_from_message(state, &payload["msg"]).await;
    }
}

async fn cache_reply_context_from_message(state: &Arc<WechatState>, msg: &Value) {
    if msg["message_type"].as_u64().unwrap_or(0) != 1 {
        return;
    }

    let from_user = msg["from_user_id"].as_str().unwrap_or("");
    if from_user.is_empty() {
        return;
    }

    let session_id = msg["session_id"].as_str().unwrap_or("");
    let context_token = msg["context_token"].as_str().unwrap_or("");
    if session_id.is_empty() && context_token.is_empty() {
        return;
    }

    state
        .remember_reply_context(from_user, session_id, context_token)
        .await;
}

async fn build_outbound_sendmessage_bodies(
    client: &reqwest::Client,
    base_url: &str,
    bot_token: &str,
    to_user_id: &str,
    session_id: &str,
    context_token: &str,
    msg: &OutboundMessage,
) -> Result<Vec<Value>> {
    let mut bodies = Vec::new();

    let text = msg.content.trim();
    if !text.is_empty() {
        bodies.push(build_sendmessage_body(
            to_user_id,
            session_id,
            context_token,
            text,
        ));
    }

    if let Some(media) = msg.media.as_ref() {
        let uploaded = upload_wechat_media(client, base_url, bot_token, to_user_id, media).await?;
        let media_item = build_media_message_item(media, &uploaded);
        bodies.push(build_sendmessage_body_with_item(
            to_user_id,
            session_id,
            context_token,
            media_item,
        ));
    }

    if bodies.is_empty() {
        bodies.push(build_sendmessage_body(
            to_user_id,
            session_id,
            context_token,
            "",
        ));
    }

    Ok(bodies)
}

async fn post_sendmessage(
    client: &reqwest::Client,
    base_url: &str,
    bot_token: &str,
    to_user_id: &str,
    body: &Value,
) -> Result<()> {
    let url = format!("{}/ilink/bot/sendmessage", base_url.trim_end_matches('/'));
    match client
        .post(&url)
        .header("Authorization", format!("Bearer {}", bot_token))
        .header("AuthorizationType", "ilink_bot_token")
        .header("Content-Type", "application/json")
        .header("X-WECHAT-UIN", build_wechat_uin())
        .header("iLink-App-ClientVersion", "1")
        .json(body)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let payload: Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    let msg = format!("WeChat sendmessage parse error: {}", e);
                    warn!("{}", msg);
                    return Err(anyhow::anyhow!(msg));
                }
            };
            let ret = payload["ret"].as_i64().unwrap_or(0);
            let errcode = payload["errcode"].as_i64().unwrap_or(0);
            if ret != 0 || errcode != 0 {
                let errmsg = payload["errmsg"].as_str().unwrap_or("unknown error");
                let msg = format!(
                    "WeChat sendmessage rejected: ret={}, errcode={}, errmsg={}",
                    ret, errcode, errmsg
                );
                warn!("{}", msg);
                return Err(anyhow::anyhow!(msg));
            }
            info!("WeChat sendmessage OK to {}", to_user_id);
            Ok(())
        }
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let msg = format!("WeChat sendmessage HTTP {}: {}", status, text);
            warn!("{}", msg);
            Err(anyhow::anyhow!(msg))
        }
        Err(e) => {
            let msg = format!("WeChat sendmessage error: {}", e);
            warn!("{}", msg);
            Err(anyhow::anyhow!(msg))
        }
    }
}

#[derive(Debug, Clone)]
struct UploadedWechatMedia {
    download_param: String,
    aes_key: [u8; 16],
    raw_size: usize,
    encrypted_size: usize,
}

async fn upload_wechat_media(
    client: &reqwest::Client,
    base_url: &str,
    bot_token: &str,
    to_user_id: &str,
    media: &MediaAttachment,
) -> Result<UploadedWechatMedia> {
    let data = media
        .data
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("WeChat media upload requires inline attachment data"))?;
    let media_type = if media.media_type.starts_with("image/") {
        UPLOAD_MEDIA_TYPE_IMAGE
    } else {
        UPLOAD_MEDIA_TYPE_FILE
    };
    let mut aes_key = [0_u8; 16];
    getrandom::getrandom(&mut aes_key)?;
    let mut filekey = [0_u8; 16];
    getrandom::getrandom(&mut filekey)?;
    let filekey = hex::encode(filekey);
    let raw_md5 = format!("{:x}", md5::compute(data));
    let encrypted_size = aes_ecb_padded_size(data.len());
    let aes_key_hex = hex::encode(aes_key);

    let upload_url_resp = request_wechat_upload_url(
        client,
        base_url,
        bot_token,
        &filekey,
        media_type,
        to_user_id,
        data.len(),
        &raw_md5,
        encrypted_size,
        &aes_key_hex,
    )
    .await?;

    let ciphertext = encrypt_aes_128_ecb(data, &aes_key)?;
    let upload_url = build_wechat_upload_url(&upload_url_resp, &filekey)?;
    let download_param = upload_encrypted_media_to_cdn(client, &upload_url, ciphertext).await?;

    info!(
        "WeChat media upload OK to {} filename={:?} mime={} raw={} encrypted={}",
        to_user_id,
        media.filename,
        media.media_type,
        data.len(),
        encrypted_size
    );

    Ok(UploadedWechatMedia {
        download_param,
        aes_key,
        raw_size: data.len(),
        encrypted_size,
    })
}

#[derive(Debug, Clone)]
struct WechatUploadUrlResp {
    upload_param: Option<String>,
    upload_full_url: Option<String>,
}

#[allow(clippy::too_many_arguments)]
async fn request_wechat_upload_url(
    client: &reqwest::Client,
    base_url: &str,
    bot_token: &str,
    filekey: &str,
    media_type: u8,
    to_user_id: &str,
    raw_size: usize,
    raw_md5: &str,
    encrypted_size: usize,
    aes_key_hex: &str,
) -> Result<WechatUploadUrlResp> {
    let url = format!("{}/ilink/bot/getuploadurl", base_url.trim_end_matches('/'));
    let body = json!({
        "filekey": filekey,
        "media_type": media_type,
        "to_user_id": to_user_id,
        "rawsize": raw_size,
        "rawfilemd5": raw_md5,
        "filesize": encrypted_size,
        "no_need_thumb": true,
        "aeskey": aes_key_hex,
        "base_info": {
            "channel_version": "2.0.0"
        }
    });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", bot_token))
        .header("AuthorizationType", "ilink_bot_token")
        .header("Content-Type", "application/json")
        .header("X-WECHAT-UIN", build_wechat_uin())
        .header("iLink-App-ClientVersion", "1")
        .json(&body)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("WeChat getuploadurl error: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "WeChat getuploadurl HTTP {}: {}",
            status,
            text
        ));
    }

    let payload: Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("WeChat getuploadurl parse error: {}", e))?;
    let ret = payload["ret"].as_i64().unwrap_or(0);
    let errcode = payload["errcode"].as_i64().unwrap_or(0);
    if ret != 0 || errcode != 0 {
        let errmsg = payload["errmsg"].as_str().unwrap_or("unknown error");
        return Err(anyhow::anyhow!(
            "WeChat getuploadurl rejected: ret={}, errcode={}, errmsg={}",
            ret,
            errcode,
            errmsg
        ));
    }

    Ok(WechatUploadUrlResp {
        upload_param: payload["upload_param"].as_str().map(str::to_string),
        upload_full_url: payload["upload_full_url"].as_str().map(str::to_string),
    })
}

fn build_wechat_upload_url(resp: &WechatUploadUrlResp, filekey: &str) -> Result<String> {
    if let Some(full_url) = resp
        .upload_full_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(full_url.to_string());
    }
    if let Some(upload_param) = resp
        .upload_param
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(format!(
            "{}/upload?encrypted_query_param={}&filekey={}",
            WECHAT_CDN_BASE_URL,
            urlencoding::encode(upload_param),
            urlencoding::encode(filekey)
        ));
    }
    Err(anyhow::anyhow!(
        "WeChat getuploadurl returned no upload URL (need upload_full_url or upload_param)"
    ))
}

async fn upload_encrypted_media_to_cdn(
    client: &reqwest::Client,
    upload_url: &str,
    ciphertext: Vec<u8>,
) -> Result<String> {
    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 1..=3 {
        match client
            .post(upload_url)
            .header("Content-Type", "application/octet-stream")
            .body(ciphertext.clone())
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
        {
            Ok(resp) if resp.status().as_u16() == 200 => {
                if let Some(param) = resp
                    .headers()
                    .get("x-encrypted-param")
                    .and_then(|value| value.to_str().ok())
                    .filter(|value| !value.trim().is_empty())
                {
                    return Ok(param.to_string());
                }
                last_error = Some(anyhow::anyhow!(
                    "WeChat CDN upload response missing x-encrypted-param header"
                ));
            }
            Ok(resp) if resp.status().is_client_error() => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(anyhow::anyhow!(
                    "WeChat CDN upload client error {}: {}",
                    status,
                    text
                ));
            }
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                last_error = Some(anyhow::anyhow!(
                    "WeChat CDN upload server error {}: {}",
                    status,
                    text
                ));
            }
            Err(e) => {
                last_error = Some(anyhow::anyhow!("WeChat CDN upload error: {}", e));
            }
        }

        if attempt < 3 {
            warn!("WeChat CDN upload attempt {} failed, retrying", attempt);
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("WeChat CDN upload failed")))
}

fn build_media_message_item(media: &MediaAttachment, uploaded: &UploadedWechatMedia) -> Value {
    let aes_key_b64 = base64::engine::general_purpose::STANDARD.encode(uploaded.aes_key);
    if media.media_type.starts_with("image/") {
        json!({
            "type": MESSAGE_ITEM_TYPE_IMAGE,
            "image_item": {
                "media": {
                    "encrypt_query_param": uploaded.download_param,
                    "aes_key": aes_key_b64,
                    "encrypt_type": 1
                },
                "mid_size": uploaded.encrypted_size
            }
        })
    } else {
        let file_name = media
            .filename
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("file");
        json!({
            "type": MESSAGE_ITEM_TYPE_FILE,
            "file_item": {
                "media": {
                    "encrypt_query_param": uploaded.download_param,
                    "aes_key": aes_key_b64,
                    "encrypt_type": 1
                },
                "file_name": file_name,
                "len": uploaded.raw_size.to_string()
            }
        })
    }
}

fn aes_ecb_padded_size(plaintext_size: usize) -> usize {
    ((plaintext_size + 16) / 16) * 16
}

fn encrypt_aes_128_ecb(plaintext: &[u8], key: &[u8; 16]) -> Result<Vec<u8>> {
    let mut buf = plaintext.to_vec();
    let plain_len = buf.len();
    buf.resize(plain_len + 16, 0);
    let encrypted = Aes128EcbEnc::new(key.into())
        .encrypt_padded_mut::<Pkcs7>(&mut buf, plain_len)
        .map_err(|e| anyhow::anyhow!("WeChat AES-128-ECB encrypt failed: {}", e))?;
    Ok(encrypted.to_vec())
}

fn build_sendmessage_body(
    to_user_id: &str,
    session_id: &str,
    context_token: &str,
    text: &str,
) -> Value {
    build_sendmessage_body_with_item(
        to_user_id,
        session_id,
        context_token,
        json!({
            "type": MESSAGE_ITEM_TYPE_TEXT,
            "text_item": { "text": text }
        }),
    )
}

fn build_sendmessage_body_with_item(
    to_user_id: &str,
    session_id: &str,
    context_token: &str,
    item: Value,
) -> Value {
    json!({
        "msg": {
            "to_user_id": to_user_id,
            "client_id": uuid::Uuid::new_v4().to_string(),
            "session_id": session_id,
            "message_type": 2,
            "message_state": 2,
            "context_token": context_token,
            "item_list": [item]
        },
        "base_info": {
            "channel_version": "2.0.0"
        }
    })
}

/// Convert an `OutboundMessage` (Agent → channel) into a `WeixinMessage` JSON
/// value that the plugin can deliver to the WeChat user.
fn outbound_to_weixin_message(msg: &OutboundMessage) -> Value {
    let msg_id = now_ms();
    json!({
        "message_id": msg_id,
        "to_user_id": msg.recipient,
        "message_type": 2,   // BOT
        "message_state": 2,  // FINISH
        "create_time_ms": msg_id,
        "item_list": [{
            "type": 1,        // TEXT
            "text_item": { "text": msg.content },
        }],
        "context_token": "",
    })
}

/// Extract an `InboundMessage` from a `WeixinMessage` JSON value sent by the
/// plugin. Text is preferred, but non-text user media (including voice) is
/// preserved as a structured placeholder so Pisci can decide how to handle it.
fn weixin_message_to_inbound(msg: &Value) -> Option<InboundMessage> {
    // Only handle USER messages (message_type == 1).
    let msg_type = msg["message_type"].as_u64().unwrap_or(0);
    if msg_type != 1 {
        return None;
    }

    let from_user = msg["from_user_id"].as_str().unwrap_or("").to_string();
    if from_user.is_empty() {
        return None;
    }

    let msg_id = msg["message_id"]
        .as_u64()
        .map(|n| n.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Extract text from the first TEXT item.
    let text = msg["item_list"]
        .as_array()?
        .iter()
        .find(|item| item["type"].as_u64() == Some(1))
        .and_then(|item| item["text_item"]["text"].as_str())
        .unwrap_or("")
        .to_string();

    let (content, media) = if text.trim().is_empty() {
        let items = msg["item_list"].as_array()?;
        let content = wechat_non_text_content(items)?;
        let media = wechat_media_placeholder(&msg_id, items);
        (content, media)
    } else {
        (text, None)
    };

    let session_id = msg["session_id"].as_str().unwrap_or("").to_string();
    let context_token = msg["context_token"].as_str().unwrap_or("").to_string();

    // reply_target encodes enough info for send() to route the reply back.
    // Format: "from_user_id|context_token"
    let reply_target = if context_token.is_empty() {
        from_user.clone()
    } else {
        format!("{}|{}", from_user, context_token)
    };

    Some(InboundMessage {
        id: msg_id,
        channel: "wechat".to_string(),
        sender: from_user.clone(),
        sender_name: Some(from_user.clone()),
        content,
        reply_target,
        conversation_key: Some(if session_id.is_empty() {
            format!("dm:{}", from_user)
        } else {
            format!("group:{}", session_id)
        }),
        is_group: !session_id.is_empty(),
        group_name: if session_id.is_empty() {
            None
        } else {
            Some(session_id.clone())
        },
        timestamp: msg["create_time_ms"].as_u64().unwrap_or_else(now_ms),
        media,
        routing_state: Some(json!({
            "context_token": context_token,
            "session_id": session_id,
            "from_user_id": from_user,
            "item_list": msg["item_list"],
        })),
    })
}

fn wechat_non_text_content(items: &[Value]) -> Option<String> {
    if items.iter().any(is_wechat_voice_item) {
        return Some("[语音消息]".to_string());
    }
    if items
        .iter()
        .any(|item| item["type"].as_u64() == Some(MESSAGE_ITEM_TYPE_IMAGE as u64))
    {
        return Some("[图片消息]".to_string());
    }
    if items
        .iter()
        .any(|item| item["type"].as_u64() == Some(MESSAGE_ITEM_TYPE_FILE as u64))
    {
        return Some("[文件消息]".to_string());
    }
    items.first().map(|item| {
        format!(
            "[微信非文本消息: type={}]",
            item["type"].as_u64().unwrap_or(0)
        )
    })
}

fn is_wechat_voice_item(item: &Value) -> bool {
    let item_type = item["type"].as_u64().unwrap_or(0);
    item_type == 3
        || item.get("voice_item").is_some()
        || item.get("audio_item").is_some()
        || item.get("speech_item").is_some()
}

fn wechat_media_placeholder(msg_id: &str, items: &[Value]) -> Option<MediaAttachment> {
    if items.iter().any(is_wechat_voice_item) {
        return Some(MediaAttachment {
            media_type: "audio/unknown".to_string(),
            url: Some(format!("wechat://message/{}", msg_id)),
            data: None,
            filename: Some(format!("wechat_voice_{}.bin", msg_id)),
        });
    }
    None
}

// ── Minimal HTTP helpers ──────────────────────────────────────────────────────

/// Find the byte offset of the start of the HTTP body (after `\r\n\r\n`).
/// Returns `None` if the header terminator has not been received yet.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// Extract the numeric value of the `Content-Length` header.
fn extract_content_length(headers: &str) -> usize {
    for line in headers.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with("content-length:") {
            if let Some(val) = lower.split(':').nth(1) {
                if let Ok(n) = val.trim().parse::<usize>() {
                    return n;
                }
            }
        }
    }
    0
}

/// Extract the value of a named header (case-insensitive).
fn extract_header<'a>(headers: &'a str, name: &str) -> &'a str {
    let lower_name = name.to_lowercase();
    for line in headers.lines() {
        let lower_line = line.to_lowercase();
        if lower_line.starts_with(&format!("{}:", lower_name)) {
            return line[name.len() + 1..].trim();
        }
    }
    ""
}

/// Write a JSON response with the given HTTP status code.
async fn write_json_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &Value,
) -> Result<()> {
    let body_str = body.to_string();
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        status_text(status),
        body_str.len(),
        body_str
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        401 => "Unauthorized",
        405 => "Method Not Allowed",
        _ => "Error",
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn build_wechat_uin() -> String {
    let mut bytes = [0_u8; 4];
    if getrandom::getrandom(&mut bytes).is_err() {
        return base64::engine::general_purpose::STANDARD.encode(now_ms().to_string());
    }
    let uin = u32::from_be_bytes(bytes).to_string();
    base64::engine::general_purpose::STANDARD.encode(uin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_inbound_messages_from_getupdates_payload() {
        let payload = json!({
            "ret": 0,
            "msgs": [{
                "message_id": 123,
                "from_user_id": "wx-user-1",
                "session_id": "",
                "message_type": 1,
                "create_time_ms": 1700000000_u64,
                "context_token": "ctx-1",
                "item_list": [{
                    "type": 1,
                    "text_item": { "text": "hello from wechat" }
                }]
            }],
            "get_updates_buf": "cursor-2"
        });

        let messages = extract_inbound_messages(&payload);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].channel, "wechat");
        assert_eq!(messages[0].sender, "wx-user-1");
        assert_eq!(messages[0].content, "hello from wechat");
        assert_eq!(messages[0].reply_target, "wx-user-1|ctx-1");
    }

    #[test]
    fn preserves_wechat_voice_message_placeholder() {
        let payload = json!({
            "ret": 0,
            "msgs": [{
                "message_id": 456,
                "from_user_id": "wx-user-1",
                "session_id": "",
                "message_type": 1,
                "create_time_ms": 1700000000_u64,
                "context_token": "ctx-voice",
                "item_list": [{
                    "type": 3,
                    "voice_item": { "duration_ms": 1200 }
                }]
            }]
        });

        let messages = extract_inbound_messages(&payload);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "[语音消息]");
        let media = messages[0].media.as_ref().expect("voice media placeholder");
        assert_eq!(media.media_type, "audio/unknown");
        assert_eq!(media.url.as_deref(), Some("wechat://message/456"));
    }

    #[test]
    fn build_sendmessage_body_includes_context_and_base_info() {
        let body = build_sendmessage_body("wx-user-1", "session-1", "ctx-1", "hello");
        assert_eq!(body["msg"]["to_user_id"], "wx-user-1");
        assert_eq!(body["msg"]["session_id"], "session-1");
        assert_eq!(body["msg"]["context_token"], "ctx-1");
        assert_eq!(body["msg"]["message_type"], 2);
        assert_eq!(body["msg"]["message_state"], 2);
        assert_eq!(body["msg"]["item_list"][0]["text_item"]["text"], "hello");
        assert_eq!(body["base_info"]["channel_version"], "2.0.0");
        assert!(body["msg"]["client_id"].as_str().is_some());
    }

    #[test]
    fn aes_ecb_padding_matches_wechat_cdn_size_rule() {
        assert_eq!(aes_ecb_padded_size(0), 16);
        assert_eq!(aes_ecb_padded_size(15), 16);
        assert_eq!(aes_ecb_padded_size(16), 32);
        assert_eq!(aes_ecb_padded_size(17), 32);

        let key = [7_u8; 16];
        let encrypted = encrypt_aes_128_ecb(b"1234567890abcdef", &key).unwrap();
        assert_eq!(encrypted.len(), 32);
    }

    #[test]
    fn build_wechat_upload_url_prefers_full_url_and_falls_back_to_param() {
        let full = build_wechat_upload_url(
            &WechatUploadUrlResp {
                upload_param: Some("ignored".into()),
                upload_full_url: Some("https://cdn.example/upload?x=1".into()),
            },
            "filekey",
        )
        .unwrap();
        assert_eq!(full, "https://cdn.example/upload?x=1");

        let fallback = build_wechat_upload_url(
            &WechatUploadUrlResp {
                upload_param: Some("a b&c".into()),
                upload_full_url: None,
            },
            "key/1",
        )
        .unwrap();
        assert!(fallback.starts_with(WECHAT_CDN_BASE_URL));
        assert!(fallback.contains("encrypted_query_param=a%20b%26c"));
        assert!(fallback.contains("filekey=key%2F1"));
    }

    #[test]
    fn media_message_items_match_wechat_protocol_shape() {
        let uploaded = UploadedWechatMedia {
            download_param: "dl-param".into(),
            aes_key: [1_u8; 16],
            raw_size: 42,
            encrypted_size: 48,
        };

        let image = build_media_message_item(
            &MediaAttachment {
                media_type: "image/png".into(),
                url: None,
                data: Some(vec![1, 2, 3]),
                filename: Some("pic.png".into()),
            },
            &uploaded,
        );
        assert_eq!(image["type"], MESSAGE_ITEM_TYPE_IMAGE);
        assert_eq!(
            image["image_item"]["media"]["encrypt_query_param"],
            "dl-param"
        );
        assert_eq!(image["image_item"]["mid_size"], 48);

        let file = build_media_message_item(
            &MediaAttachment {
                media_type: "application/pdf".into(),
                url: None,
                data: Some(vec![1, 2, 3]),
                filename: Some("doc.pdf".into()),
            },
            &uploaded,
        );
        assert_eq!(file["type"], MESSAGE_ITEM_TYPE_FILE);
        assert_eq!(file["file_item"]["file_name"], "doc.pdf");
        assert_eq!(file["file_item"]["len"], "42");
    }

    #[tokio::test]
    async fn caches_last_non_empty_context_token_per_user() {
        let state = WechatState::new();

        cache_reply_context_from_message(
            &state,
            &json!({
                "from_user_id": "wx-user-1",
                "session_id": "session-1",
                "message_type": 1,
                "context_token": "ctx-1",
            }),
        )
        .await;

        cache_reply_context_from_message(
            &state,
            &json!({
                "from_user_id": "wx-user-1",
                "session_id": "session-1",
                "message_type": 1,
                "context_token": "",
            }),
        )
        .await;

        let resolved = state.resolve_reply_context("wx-user-1", "").await;
        assert_eq!(resolved.context_token, "ctx-1");
        assert_eq!(resolved.session_id, "session-1");
    }
}
