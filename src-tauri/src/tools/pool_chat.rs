/// pool_chat — lets Koi agents communicate in the project pool chat.
///
/// Koi agents use this tool to:
/// - Send messages to the pool (share progress, results, discussions)
/// - Read recent messages (see what other team members have done)
/// - Reply to specific messages
///
/// @mentions in sent messages trigger notifications:
/// - Busy Koi: notification injected into their running AgentLoop
/// - Idle Koi: spawned to check messages and respond autonomously
use crate::agent::tool::{Tool, ToolContext, ToolResult};
use crate::koi::runtime::KOI_SESSIONS;
use crate::store::Database;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

pub struct PoolChatTool {
    pub app: AppHandle,
    pub db: Arc<Mutex<Database>>,
    pub sender_id: String,
    pub sender_name: String,
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
         - 'send': Post a message to pool chat as yourself. Use @KoiName to get someone's attention, or @all to notify everyone. \
         - 'read': Read recent messages from the pool chat to see what's happening. \
         - 'reply': Reply to a specific message by ID."
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
        let action = input["action"].as_str().unwrap_or("read");
        match action {
            "send" => self.send_message(&input, ctx).await,
            "read" => self.read_messages(&input, ctx).await,
            "reply" => self.reply_message(&input, ctx).await,
            _ => Ok(ToolResult::err(format!(
                "Unknown action '{}'. Use: send, read, reply",
                action
            ))),
        }
    }
}

impl PoolChatTool {
    async fn resolve_pool_session(
        &self,
        input: &Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<crate::koi::PoolSession> {
        let requested = input["pool_id"]
            .as_str()
            .map(str::trim)
            .filter(|id| !id.is_empty() && *id != "current")
            .map(str::to_string)
            .or_else(|| ctx.pool_session_id.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                "No pool_id available. Provide pool_id or ensure you are working in a pool context."
            )
            })?;

        let db = self.db.lock().await;
        match db.resolve_pool_session_identifier(&requested)? {
            Some(session) => Ok(session),
            None => Err(anyhow::anyhow!("Pool '{}' not found", requested)),
        }
    }

    fn ensure_pool_writable(
        &self,
        pool: &crate::koi::PoolSession,
        action: &str,
    ) -> anyhow::Result<()> {
        if pool.status == "active" {
            return Ok(());
        }
        Err(anyhow::anyhow!(
            "Pool '{}' is {}. Action '{}' is disabled until the pool is resumed.",
            pool.name,
            pool.status,
            action
        ))
    }

    async fn send_message(&self, input: &Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let content = match input["content"].as_str() {
            Some(c) if !c.trim().is_empty() => c.trim(),
            _ => return Ok(ToolResult::err("'content' is required for action 'send'")),
        };
        let pool = match self.resolve_pool_session(input, ctx).await {
            Ok(pool) => pool,
            Err(err) => return Ok(ToolResult::err(err.to_string())),
        };
        if let Err(err) = self.ensure_pool_writable(&pool, "send") {
            return Ok(ToolResult::err(err.to_string()));
        }
        let pool_id = pool.id;

        let msg = {
            let db = self.db.lock().await;
            db.insert_pool_message(&pool_id, &self.sender_id, content, "text", "{}")?
        };

        let event_name = format!("pool_message_{}", pool_id);
        let _ = self
            .app
            .emit(&event_name, serde_json::to_value(&msg).unwrap_or_default());

        self.dispatch_mentions(&pool_id, content).await;

        Ok(ToolResult::ok(format!(
            "Message sent to pool (id: {}).",
            msg.id
        )))
    }

    async fn read_messages(&self, input: &Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let pool_id = match self.resolve_pool_session(input, ctx).await {
            Ok(pool) => pool.id,
            Err(err) => return Ok(ToolResult::err(err.to_string())),
        };
        let limit = input["limit"].as_i64().unwrap_or(20);

        let db = self.db.lock().await;
        let messages = db.get_pool_messages(&pool_id, limit, 0)?;
        drop(db);

        if messages.is_empty() {
            return Ok(ToolResult::ok("No messages in this pool yet."));
        }

        let kois = {
            let db = self.db.lock().await;
            db.list_kois().unwrap_or_default()
        };
        let koi_names: std::collections::HashMap<String, String> = kois
            .iter()
            .map(|k| (k.id.clone(), format!("{} {}", k.icon, k.name)))
            .collect();

        let mut lines: Vec<String> = Vec::new();
        for m in &messages {
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

        Ok(ToolResult::ok(format!(
            "Pool messages ({} shown):\n{}",
            messages.len(),
            lines.join("\n")
        )))
    }

    async fn reply_message(&self, input: &Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let content = match input["content"].as_str() {
            Some(c) if !c.trim().is_empty() => c.trim(),
            _ => return Ok(ToolResult::err("'content' is required for action 'reply'")),
        };
        let message_id = match input["message_id"].as_i64() {
            Some(id) => id,
            None => {
                return Ok(ToolResult::err(
                    "'message_id' is required for action 'reply'",
                ))
            }
        };
        let pool = match self.resolve_pool_session(input, ctx).await {
            Ok(pool) => pool,
            Err(err) => return Ok(ToolResult::err(err.to_string())),
        };
        if let Err(err) = self.ensure_pool_writable(&pool, "reply") {
            return Ok(ToolResult::err(err.to_string()));
        }
        let pool_id = pool.id;

        let msg = {
            let db = self.db.lock().await;
            db.insert_pool_message_ext(
                &pool_id,
                &self.sender_id,
                content,
                "text",
                "{}",
                None,
                Some(message_id),
                None,
            )?
        };

        let event_name = format!("pool_message_{}", pool_id);
        let _ = self
            .app
            .emit(&event_name, serde_json::to_value(&msg).unwrap_or_default());

        self.dispatch_mentions(&pool_id, content).await;

        Ok(ToolResult::ok(format!(
            "Reply sent (id: {}, replying to #{}).",
            msg.id, message_id
        )))
    }

    /// Detect @KoiName / @all mentions in content and dispatch notifications.
    /// Busy Koi: inject into running AgentLoop.
    /// Idle Koi: spawn activation to check messages.
    async fn dispatch_mentions(&self, pool_id: &str, content: &str) {
        if !content.contains('@') {
            return;
        }

        let kois = {
            let db = self.db.lock().await;
            db.list_kois().unwrap_or_default()
        };

        let mention_all = content.contains("@all");

        for koi in &kois {
            if koi.status == "offline" || koi.id == self.sender_id {
                continue;
            }
            let mention = format!("@{}", koi.name);
            if !mention_all && !content.contains(&mention) {
                continue;
            }

            let session_key = format!("{}:{}", koi.id, pool_id);
            let sessions = KOI_SESSIONS.lock().await;
            if let Some(tx) = sessions.get(&session_key) {
                let notification = format!(
                    "[Pool Notification] @{} from {} (in pool chat):\n{}\n\n\
                     Decide autonomously how to respond: use pool_chat to reply, accept the request, \
                     ask for clarification, decline, or continue your current work. \
                     This is your decision — you are not required to act on every mention.",
                    koi.name, self.sender_name, content
                );
                let _ = tx.send(notification).await;
                tracing::info!("Injected @mention notification to busy Koi '{}'", koi.name);
            } else {
                drop(sessions);
                let app = self.app.clone();
                let db = self.db.clone();
                let koi_id = koi.id.clone();
                let koi_name = koi.name.clone();
                let pool_id = pool_id.to_string();
                let sender_name = self.sender_name.clone();
                tokio::spawn(async move {
                    tracing::info!(
                        "Activating idle Koi '{}' for @mention from '{}'",
                        koi_name,
                        sender_name
                    );
                    let runtime = crate::koi::runtime::KoiRuntime::from_tauri(app, db);
                    if let Err(e) = runtime.activate_for_messages(&koi_id, &pool_id).await {
                        tracing::warn!("Failed to activate Koi '{}' for messages: {}", koi_name, e);
                    }
                });
            }
        }
    }
}
