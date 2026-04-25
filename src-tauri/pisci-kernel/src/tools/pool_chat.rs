//! `pool_chat` — platform-neutral thin wrapper around
//! [`crate::pool::services::send_pool_message`] and
//! [`crate::pool::services::read_pool_messages`].
//!
//! Sender identity is taken from [`ToolContext::memory_owner_id`], so
//! one instance of this tool serves all scenes in a process; the prior
//! desktop code baked the `sender_id` into a fresh struct per scene
//! (one per Koi + one for Pisci) which is no longer necessary.

use crate::agent::tool::{Tool, ToolContext, ToolResult};
use crate::pool::coordinator::CoordinatorConfig;
use crate::pool::model::{CallerContext, SendPoolMessageArgs};
use crate::pool::{services, PoolStore};
use async_trait::async_trait;
use pisci_core::host::{PoolEventSink, SubagentRuntime};
use pisci_core::models::{KoiDefinition, PoolMessage};
use serde_json::{json, Value};
use std::sync::Arc;

pub struct PoolChatTool {
    pub store: PoolStore,
    pub sink: Arc<dyn PoolEventSink>,
    /// Mention fan-out seam. Safe to omit in headless/CLI hosts that
    /// don't run Koi subprocesses yet.
    pub subagent: Option<Arc<dyn SubagentRuntime>>,
    /// Coordinator configuration (task timeout, worktree usage).
    pub coordinator_cfg: CoordinatorConfig,
}

impl PoolChatTool {
    fn caller<'a>(&'a self, ctx: &'a ToolContext) -> CallerContext<'a> {
        CallerContext {
            memory_owner_id: &ctx.memory_owner_id,
            session_id: &ctx.session_id,
            session_source: None,
            pool_session_id: ctx.pool_session_id.as_deref(),
            cancel: Some(ctx.cancel.clone()),
        }
    }
}

#[async_trait]
impl Tool for PoolChatTool {
    fn name(&self) -> &str {
        "pool_chat"
    }

    fn description(&self) -> &str {
        "Communicate in the project pool chat with your team members. \
         \
         Actions: \
         - 'send': Post a message to pool chat as yourself. Use plain `@KoiName` / `@all` for notification only. Use `@!KoiName` / `@!all` only when you are explicitly delegating concrete work that should create or wake active execution. When the project needs explicit coordination, include a concrete handoff and a `[ProjectStatus] ...` signal. Koi completion is not final delivery until Pisci supervisor reviews and merges or requests rework. \
         - 'read': Read recent messages from the pool chat to see what's happening. \
         - 'reply': Reply to a specific message by ID. \
         Pool chat is the visible coordination channel for the team. If another agent must act next, a board update alone is not enough — say it explicitly here."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["send", "read", "reply"],
                    "description": "Action to perform"
                },
                "content": {
                    "type": "string",
                    "description": "For send/reply: the message content"
                },
                "pool_id": {
                    "type": "string",
                    "description": "Pool session ID (optional, defaults to current pool)"
                },
                "message_id": {
                    "type": "integer",
                    "description": "For reply: the message ID to reply to"
                },
                "limit": {
                    "type": "integer",
                    "description": "For read: max number of messages (default 20)"
                }
            },
            "required": ["action"]
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let caller = self.caller(ctx);
        let action = input["action"].as_str().unwrap_or("read");

        match action {
            "send" => {
                let content = input["content"].as_str().unwrap_or("");
                if content.trim().is_empty() {
                    return Ok(ToolResult::err("'content' is required for action 'send'"));
                }
                let args = SendPoolMessageArgs {
                    pool_id: input["pool_id"].as_str().unwrap_or("").to_string(),
                    sender_id: ctx.memory_owner_id.clone(),
                    content: content.to_string(),
                    reply_to_message_id: None,
                };
                match services::send_pool_message(
                    &self.store,
                    self.sink.clone(),
                    self.subagent.clone(),
                    &self.coordinator_cfg,
                    &caller,
                    args,
                )
                .await
                {
                    Ok(msg) => Ok(ToolResult::ok(format!(
                        "Message sent to pool (id: {}).",
                        msg.id
                    ))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "reply" => {
                let content = input["content"].as_str().unwrap_or("");
                if content.trim().is_empty() {
                    return Ok(ToolResult::err("'content' is required for action 'reply'"));
                }
                let message_id = match input["message_id"].as_i64() {
                    Some(id) => id,
                    None => {
                        return Ok(ToolResult::err(
                            "'message_id' is required for action 'reply'",
                        ))
                    }
                };
                let args = SendPoolMessageArgs {
                    pool_id: input["pool_id"].as_str().unwrap_or("").to_string(),
                    sender_id: ctx.memory_owner_id.clone(),
                    content: content.to_string(),
                    reply_to_message_id: Some(message_id),
                };
                match services::send_pool_message(
                    &self.store,
                    self.sink.clone(),
                    self.subagent.clone(),
                    &self.coordinator_cfg,
                    &caller,
                    args,
                )
                .await
                {
                    Ok(msg) => Ok(ToolResult::ok(format!(
                        "Reply sent (id: {}, replying to #{}).",
                        msg.id, message_id
                    ))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "read" => {
                let pool_id = input["pool_id"].as_str().unwrap_or("");
                let limit = input["limit"].as_i64().unwrap_or(20);
                match services::read_pool_messages(&self.store, &caller, pool_id, limit).await {
                    Ok(v) => Ok(ToolResult::ok(render_read(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            other => Ok(ToolResult::err(format!(
                "Unknown action '{}'. Use: send, read, reply",
                other
            ))),
        }
    }
}

fn render_read(v: &Value) -> String {
    let kois: Vec<KoiDefinition> = v
        .get("kois")
        .and_then(|k| k.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|x| serde_json::from_value(x).ok())
        .collect();
    let koi_names: std::collections::HashMap<String, String> = kois
        .iter()
        .map(|k| (k.id.clone(), format!("{} {}", k.icon, k.name)))
        .collect();

    let msgs: Vec<PoolMessage> = v
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|x| serde_json::from_value(x).ok())
        .collect();

    if msgs.is_empty() {
        return "No messages in this pool yet.".to_string();
    }

    let mut lines: Vec<String> = Vec::new();
    for m in &msgs {
        let sender_display = koi_names
            .get(&m.sender_id)
            .cloned()
            .unwrap_or_else(|| m.sender_id.clone());
        let time = m.created_at.format("%m-%d %H:%M").to_string();
        let content = if m.content.chars().count() > 500 {
            format!("{}...", m.content.chars().take(500).collect::<String>())
        } else {
            m.content.clone()
        };
        lines.push(format!(
            "[{}] #{} {} ({}): {}",
            time, m.id, sender_display, m.msg_type, content
        ));
    }

    format!(
        "Pool messages ({} shown):\n{}",
        msgs.len(),
        lines.join("\n")
    )
}
