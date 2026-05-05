//! Native `im_send_message` tool — let the agent post a Markdown message
//! to an IM conversation using the existing [`crate::gateway::GatewayManager`].
//!
//! This is the "native" side of the layered enterprise-capability
//! architecture (see `pisci-core::scene::SceneKind::IMHeadless` doc):
//! credentials live on the platform application (bot_id/secret,
//! app_id/secret, app_key/secret), the channel uses them to maintain
//! a long-running WebSocket transport, and this tool re-uses the
//! *same* connection to push outbound messages without requiring a
//! second HTTP roundtrip or a separate token cache.
//!
//! Two (+1 auto) addressing modes:
//!   1. `binding_key` — preferred for replying to an inbound IM
//!      conversation. Looks up [`crate::store::db::ImSessionBinding`]
//!      and reuses its `latest_reply_target` + `routing_state_json`,
//!      so the channel can resolve `req_id` / `sessionWebhook` /
//!      DingTalk `msg_param_map`, etc.
//!   2. `channel` + `recipient` — for proactive messages the agent
//!      can address by raw recipient identifier (e.g. `userid` for
//!      WeCom or `open_id` for Feishu). The channel must already be
//!      registered with `GatewayManager`.
//!   3. auto-resolve — when no explicit addressing is provided, the
//!      tool resolves the IM binding from the current `session_id`
//!      via [`Database::find_im_session_binding_for_session`]. This
//!      works automatically in IM-driven sessions (WeChat, Feishu, etc.)
//!      without the agent needing to know its `binding_key`.
//!
//! If the requested IM channel is not connected (`channel_enabled =
//! false`), the tool returns a clean error rather than silently
//! falling back to HTTP, so the agent surfaces the actual gap to the
//! user.

use crate::app::markers::guess_mime_from_path;
use crate::gateway::{GatewayManager, MediaAttachment, OutboundMessage};
use crate::store::Database;
use async_trait::async_trait;
use pisci_kernel::agent::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

pub struct ImSendMessageTool {
    pub gateway: Option<Arc<GatewayManager>>,
    pub db: Option<Arc<Mutex<Database>>>,
}

#[async_trait]
impl Tool for ImSendMessageTool {
    fn name(&self) -> &str {
        "im_send_message"
    }

    fn description(&self) -> &str {
        "Send a Markdown text message, and optionally one local file attachment, to an IM conversation through the connected IM channel (WeCom / Feishu / DingTalk / WeChat / Slack / etc.). \
         \n\nADDRESSING (use one of):\
         \n- 'binding_key': preferred when replying to an existing IM conversation. The binding stores the channel name, the latest reply target, and any channel-specific routing state (e.g. WeCom 'req_id', DingTalk 'sessionWebhook'). Pass the 'binding_key' you received from an inbound IM message handler.\
         \n- 'channel' + 'recipient': for proactive (unprompted) messages. 'channel' is a registered channel name ('wecom', 'feishu', 'dingtalk', 'wechat', ...). 'recipient' is the channel-native target id (WeCom userid / Feishu open_id / DingTalk staffId / etc.). Optional 'routing_state' is forwarded verbatim if you know the channel-specific shape.\
         \n- auto-resolve: when none of the above are provided, the tool automatically resolves the IM binding from the current session. This works when you are in an IM-driven conversation (e.g. replying to a WeChat/Feishu user) — no explicit addressing parameters are needed.\
         \n\nThis tool returns an error when the requested channel is not currently connected. Channels are configured separately under Settings → IM; this tool only consumes the existing transport, it does NOT enable a channel.\
         \n\nOptional 'file_path' sends a local file attachment when the channel supports media upload. WeChat supports image/* and generic file attachments through iLink CDN upload. \
         \n\nKeep messages short and use Markdown for emphasis where the underlying channel supports it. Avoid sending walls of debug output."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Message body (Markdown). Required."
                },
                "binding_key": {
                    "type": "string",
                    "description": "Stable conversation key from an inbound IM message (e.g. 'wecom::user:bot-123:user-456'). When provided, channel/recipient/routing_state are auto-filled from the persisted binding."
                },
                "channel": {
                    "type": "string",
                    "description": "Registered channel name when sending without a binding_key (e.g. 'wecom', 'feishu', 'dingtalk')."
                },
                "recipient": {
                    "type": "string",
                    "description": "Channel-native recipient id when sending without a binding_key."
                },
                "reply_to": {
                    "type": "string",
                    "description": "Optional message id to thread the reply against (channel-specific support)."
                },
                "routing_state": {
                    "description": "Optional channel-specific routing state object (forwarded as-is). Only useful when sending without a binding_key."
                },
                "file_path": {
                    "type": "string",
                    "description": "Optional absolute path to a local file to send as an attachment. Supported by channels with media upload support, including WeChat."
                },
                "media_type": {
                    "type": "string",
                    "description": "Optional MIME type override for file_path. If omitted, Pisci infers it from the file extension."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let text = match input["text"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(t) => t.to_string(),
            None => {
                return Ok(ToolResult::err(
                    "'text' is required and must be a non-empty string",
                ))
            }
        };

        let gateway = match self.gateway.as_ref() {
            Some(g) => g.clone(),
            None => {
                return Ok(ToolResult::err(
                    "IM gateway is unavailable in this context (channels not initialised)",
                ))
            }
        };

        let binding_key = input["binding_key"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let channel_arg = input["channel"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let recipient_arg = input["recipient"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let media = match input["file_path"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(path) => match std::fs::read(path) {
                Ok(data) => {
                    let filename = std::path::Path::new(path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "file".to_string());
                    let media_type = input["media_type"]
                        .as_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| guess_mime_from_path(path));
                    Some(MediaAttachment {
                        media_type,
                        url: None,
                        data: Some(data),
                        filename: Some(filename),
                    })
                }
                Err(err) => {
                    return Ok(ToolResult::err(format!(
                        "failed to read file_path '{}': {}",
                        path, err
                    )))
                }
            },
            None => None,
        };

        let outbound = if let Some(key) = binding_key {
            // Explicit binding_key provided — look it up directly.
            let db = match self.db.as_ref() {
                Some(d) => d.clone(),
                None => {
                    return Ok(ToolResult::err(
                        "database handle unavailable; cannot resolve binding_key",
                    ))
                }
            };
            let binding = {
                let db = db.lock().await;
                match db.get_im_session_binding(key) {
                    Ok(Some(b)) => b,
                    Ok(None) => {
                        return Ok(ToolResult::err(format!("IM binding '{}' not found", key)))
                    }
                    Err(err) => {
                        return Ok(ToolResult::err(format!(
                            "failed to look up binding '{}': {}",
                            key, err
                        )))
                    }
                }
            };
            let routing_state = binding
                .routing_state_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
            let recipient = if binding.latest_reply_target.trim().is_empty() {
                binding.peer_id.clone()
            } else {
                binding.latest_reply_target.clone()
            };
            OutboundMessage {
                channel: binding.channel.clone(),
                recipient,
                content: text,
                reply_to: input["reply_to"].as_str().map(|s| s.to_string()),
                media: media.clone(),
                routing_state,
            }
        } else if channel_arg.is_some() || recipient_arg.is_some() {
            // Explicit channel + recipient provided.
            let channel = match channel_arg {
                Some(c) => c.to_string(),
                None => {
                    return Ok(ToolResult::err(
                        "'channel' is required when 'recipient' is provided without 'binding_key'",
                    ))
                }
            };
            let recipient = match recipient_arg {
                Some(r) => r.to_string(),
                None => {
                    return Ok(ToolResult::err(
                        "'recipient' is required when 'channel' is provided without 'binding_key'",
                    ))
                }
            };
            let routing_state = input.get("routing_state").cloned().filter(|v| !v.is_null());
            OutboundMessage {
                channel,
                recipient,
                content: text,
                reply_to: input["reply_to"].as_str().map(|s| s.to_string()),
                media,
                routing_state,
            }
        } else {
            // No explicit addressing — auto-resolve from current session.
            let db = match self.db.as_ref() {
                Some(d) => d.clone(),
                None => {
                    return Ok(ToolResult::err(
                        "either 'binding_key' or both 'channel' and 'recipient' are required \
                         (no database handle to auto-resolve from session)",
                    ))
                }
            };
            let binding = {
                let db = db.lock().await;
                match db.find_im_session_binding_for_session(&_ctx.session_id) {
                    Ok(Some(b)) => b,
                    Ok(None) => {
                        return Ok(ToolResult::err(format!(
                            "no IM binding found for current session '{}'; \
                             provide 'binding_key' or 'channel' + 'recipient'",
                            _ctx.session_id
                        )))
                    }
                    Err(err) => {
                        return Ok(ToolResult::err(format!(
                            "failed to look up binding for session '{}': {}",
                            _ctx.session_id, err
                        )))
                    }
                }
            };
            info!(
                "im_send_message: auto-resolved binding_key='{}' from session_id='{}'",
                binding.binding_key, _ctx.session_id
            );
            let routing_state = binding
                .routing_state_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
            let recipient = if binding.latest_reply_target.trim().is_empty() {
                binding.peer_id.clone()
            } else {
                binding.latest_reply_target.clone()
            };
            OutboundMessage {
                channel: binding.channel.clone(),
                recipient,
                content: text,
                reply_to: input["reply_to"].as_str().map(|s| s.to_string()),
                media,
                routing_state,
            }
        };

        match gateway.send(&outbound).await {
            Ok(()) => {
                info!(
                    "im_send_message: channel={} recipient={} chars={}",
                    outbound.channel,
                    outbound.recipient,
                    outbound.content.chars().count()
                );
                Ok(ToolResult::ok(format!(
                    "Sent message via channel '{}' to '{}'.",
                    outbound.channel, outbound.recipient
                )))
            }
            Err(err) => {
                warn!(
                    "im_send_message: gateway.send failed channel={} recipient={}: {}",
                    outbound.channel, outbound.recipient, err
                );
                Ok(ToolResult::err(format!(
                    "Gateway send failed (channel='{}'): {}",
                    outbound.channel, err
                )))
            }
        }
    }
}
