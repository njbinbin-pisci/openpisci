use crate::commands::chat::gateway::connect_gateway_channels;
use crate::gateway::{ChannelInfo, ChannelStatus, GatewayManager};
use crate::store::{AppState, Database, Settings};
use async_trait::async_trait;
use pisci_kernel::agent::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

pub struct ImChannelListTool {
    pub gateway: Option<Arc<GatewayManager>>,
    pub settings: Option<Arc<Mutex<Settings>>>,
}

pub struct ImChannelConnectTool {
    pub gateway: Option<Arc<GatewayManager>>,
    pub app_handle: Option<AppHandle>,
}

pub struct ImChannelBindingLookupTool {
    pub db: Option<Arc<Mutex<Database>>>,
    pub gateway: Option<Arc<GatewayManager>>,
}

pub struct ImChannelBindingListTool {
    pub db: Option<Arc<Mutex<Database>>>,
    pub gateway: Option<Arc<GatewayManager>>,
}

fn configured_channels_from_settings(settings: &Settings) -> Vec<&'static str> {
    let mut out = Vec::new();
    if settings.feishu_enabled {
        out.push("feishu");
    }
    if settings.wecom_enabled {
        out.push("wecom");
    }
    if settings.dingtalk_enabled {
        out.push("dingtalk");
    }
    if settings.telegram_enabled {
        out.push("telegram");
    }
    if settings.slack_enabled {
        out.push("slack");
    }
    if settings.discord_enabled {
        out.push("discord");
    }
    if settings.teams_enabled {
        out.push("teams");
    }
    if settings.matrix_enabled {
        out.push("matrix");
    }
    if settings.webhook_enabled {
        out.push("webhook");
    }
    if settings.wechat_enabled {
        out.push("wechat");
    }
    out
}

async fn configured_channel_json(settings: Option<&Arc<Mutex<Settings>>>) -> Vec<Value> {
    let Some(settings) = settings else {
        return Vec::new();
    };
    let settings = settings.lock().await;
    configured_channels_from_settings(&settings)
        .into_iter()
        .map(|name| json!({ "name": name }))
        .collect()
}

fn channel_status_label(status: &ChannelStatus) -> &'static str {
    match status {
        ChannelStatus::Disconnected => "disconnected",
        ChannelStatus::Connecting => "connecting",
        ChannelStatus::Connected => "connected",
        ChannelStatus::Error(_) => "error",
    }
}

fn channel_status_error(status: &ChannelStatus) -> Option<String> {
    match status {
        ChannelStatus::Error(message) => Some(message.clone()),
        _ => None,
    }
}

fn channel_info_json(info: &ChannelInfo) -> Value {
    json!({
        "name": info.name,
        "status": channel_status_label(&info.status),
        "connected": matches!(info.status, ChannelStatus::Connected),
        "error": channel_status_error(&info.status),
        "connected_at": info.connected_at,
    })
}

async fn list_channel_json(gateway: &GatewayManager) -> Vec<Value> {
    gateway
        .list_channels()
        .await
        .iter()
        .map(channel_info_json)
        .collect()
}

fn resolve_lookup_key(input: &Value, ctx: &ToolContext) -> Result<(String, String), String> {
    let binding_key = input
        .get("binding_key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    if let Some(key) = binding_key {
        return Ok(("binding_key".to_string(), key));
    }

    let session_id = input
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| {
            input
                .get("use_current_session")
                .and_then(|v| v.as_bool())
                .filter(|v| *v)
                .map(|_| ctx.session_id.clone())
        });
    if let Some(session_id) = session_id {
        return Ok(("session_id".to_string(), session_id));
    }

    let task_id = input
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    if let Some(task_id) = task_id {
        return Ok(("task_id".to_string(), task_id));
    }

    let pool_id = input
        .get("pool_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| ctx.pool_session_id.clone());
    if let Some(pool_id) = pool_id {
        return Ok(("pool_id".to_string(), pool_id));
    }

    Err("provide one of 'binding_key', 'session_id', 'pool_id', or 'task_id' (or set 'use_current_session' to true)".to_string())
}

fn scheduler_session_id(task_id: &str) -> String {
    format!("sched_{}", task_id)
}

#[async_trait]
impl Tool for ImChannelListTool {
    fn name(&self) -> &str {
        "im_channel_list"
    }

    fn description(&self) -> &str {
        "List IM channel names the agent can reason about. Returns both configured channels from Settings and currently connected channels from the live gateway. Use this first when you need to know whether a channel like 'wechat' exists, whether it is already connected, and whether you should call im_channel_connect next. Read-only."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(&self, _input: Value, _ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let connected_channels = match self.gateway.as_ref() {
            Some(gateway) => list_channel_json(gateway).await,
            None => Vec::new(),
        };
        let configured_channels = configured_channel_json(self.settings.as_ref()).await;
        Ok(ToolResult::ok(
            json!({
                "configured_channels": configured_channels,
                "connected_channels": connected_channels,
            })
            .to_string(),
        ))
    }
}

#[async_trait]
impl Tool for ImChannelConnectTool {
    fn name(&self) -> &str {
        "im_channel_connect"
    }

    fn description(&self) -> &str {
        "Connect the IM channels currently enabled in Settings. This is write-capable because it starts external channel transports. It does not disconnect channels."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Optional channel name hint to verify after connection. Connection still follows the enabled channels in Settings."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        if self.gateway.is_none() {
            return Ok(ToolResult::err("IM gateway is unavailable in this context"));
        }
        let app =
            match self.app_handle.as_ref() {
                Some(app) => app.clone(),
                None => return Ok(ToolResult::err(
                    "desktop app handle unavailable; cannot connect IM channels in this runtime",
                )),
            };
        let state = app.state::<AppState>();
        let desired = input
            .get("channel")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_ascii_lowercase);
        match connect_gateway_channels(state).await {
            Ok(status) => {
                let channels: Vec<Value> = status.channels.iter().map(channel_info_json).collect();
                let matched = desired.as_ref().map(|name| {
                    channels.iter().any(|item| {
                        item.get("name")
                            .and_then(|v| v.as_str())
                            .map(|value| value.eq_ignore_ascii_case(name))
                            .unwrap_or(false)
                    })
                });
                Ok(ToolResult::ok(
                    json!({
                        "connected": true,
                        "channels": channels,
                        "requested_channel_found": matched,
                    })
                    .to_string(),
                ))
            }
            Err(err) => Ok(ToolResult::err(format!(
                "failed to connect IM channels: {}",
                err
            ))),
        }
    }
}

#[async_trait]
impl Tool for ImChannelBindingLookupTool {
    fn name(&self) -> &str {
        "im_channel_binding_lookup"
    }

    fn description(&self) -> &str {
        "Resolve which IM binding_key should be used for a session, pool, or scheduled task. Supports lookup by binding_key, session_id, pool_id, or task_id. Use this before im_send_message when your runtime is not already inside an IM-driven session. Read-only."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "binding_key": {
                    "type": "string",
                    "description": "Look up a specific binding by binding_key."
                },
                "session_id": {
                    "type": "string",
                    "description": "Resolve the most recent IM binding associated with this Pisci session_id."
                },
                "pool_id": {
                    "type": "string",
                    "description": "Resolve the IM binding that originally created this pool, if any."
                },
                "task_id": {
                    "type": "string",
                    "description": "Convenience alias for scheduled tasks; resolves via session_id 'sched_<task_id>'."
                },
                "use_current_session": {
                    "type": "boolean",
                    "description": "When true, use the current runtime session_id if no explicit identifier was provided."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let db = match self.db.as_ref() {
            Some(db) => db.clone(),
            None => {
                return Ok(ToolResult::err(
                    "database handle unavailable; cannot resolve IM binding",
                ))
            }
        };
        let (lookup_kind, lookup_value) = match resolve_lookup_key(&input, ctx) {
            Ok(result) => result,
            Err(err) => return Ok(ToolResult::err(err)),
        };
        let binding = {
            let db = db.lock().await;
            match lookup_kind.as_str() {
                "binding_key" => db.get_im_session_binding(&lookup_value),
                "session_id" => db.find_im_session_binding_for_session(&lookup_value),
                "pool_id" => db.find_im_session_binding_for_pool(&lookup_value),
                "task_id" => {
                    db.find_im_session_binding_for_session(&scheduler_session_id(&lookup_value))
                }
                _ => unreachable!("validated lookup kind"),
            }
        };
        let binding = match binding {
            Ok(Some(binding)) => binding,
            Ok(None) => {
                return Ok(ToolResult::ok(
                    json!({
                        "found": false,
                        "lookup": {
                            "kind": lookup_kind,
                            "value": lookup_value,
                        }
                    })
                    .to_string(),
                ))
            }
            Err(err) => {
                return Ok(ToolResult::err(format!(
                    "failed to resolve IM binding for {} '{}': {}",
                    lookup_kind, lookup_value, err
                )))
            }
        };

        let channel_status = match self.gateway.as_ref() {
            Some(gateway) => gateway
                .list_channels()
                .await
                .into_iter()
                .find(|channel| channel.name == binding.channel),
            None => None,
        };

        Ok(ToolResult::ok(
            json!({
                "found": true,
                "lookup": {
                    "kind": lookup_kind,
                    "value": lookup_value,
                },
                "binding": {
                    "binding_key": binding.binding_key,
                    "channel": binding.channel,
                    "session_id": binding.session_id,
                    "peer_id": binding.peer_id,
                    "peer_name": binding.peer_name,
                    "is_group": binding.is_group,
                    "group_name": binding.group_name,
                    "latest_reply_target": binding.latest_reply_target,
                    "routing_state_json": binding.routing_state_json,
                },
                "channel_status": channel_status.as_ref().map(channel_info_json),
                "usable": channel_status
                    .as_ref()
                    .map(|info| matches!(info.status, ChannelStatus::Connected))
                    .unwrap_or(false),
            })
            .to_string(),
        ))
    }
}

fn render_binding_json(
    binding: &crate::store::db::ImSessionBinding,
    status: Option<&ChannelInfo>,
) -> Value {
    json!({
        "binding_key": binding.binding_key,
        "channel": binding.channel,
        "session_id": binding.session_id,
        "peer_id": binding.peer_id,
        "peer_name": binding.peer_name,
        "is_group": binding.is_group,
        "group_name": binding.group_name,
        "latest_reply_target": binding.latest_reply_target,
        "routing_state_json": binding.routing_state_json,
        "last_inbound_at": binding.last_inbound_at.to_rfc3339(),
        "usable": status
            .map(|info| matches!(info.status, ChannelStatus::Connected))
            .unwrap_or(false),
        "channel_status": status.map(channel_info_json),
    })
}

#[async_trait]
impl Tool for ImChannelBindingListTool {
    fn name(&self) -> &str {
        "im_channel_binding_list"
    }

    fn description(&self) -> &str {
        "List candidate IM target tokens for a specific channel name such as 'wechat'. Use this when you know you want to send through a channel but do not yet know the target token. Supports optional narrowing by session_id, task_id, or pool_id. Returns binding_key values you can turn into notify_user targets as 'im_binding:<binding_key>' or pass directly to im_send_message. Read-only."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["channel"],
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Channel name to search bindings for, e.g. 'wechat', 'wecom', 'feishu'."
                },
                "session_id": {
                    "type": "string",
                    "description": "Optional exact session_id to narrow the candidate list."
                },
                "task_id": {
                    "type": "string",
                    "description": "Optional scheduled task id; narrows via session_id 'sched_<task_id>'."
                },
                "pool_id": {
                    "type": "string",
                    "description": "Optional pool id; if that pool has an origin binding on this channel it is returned first."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of recent candidates to return when no exact context match is found. Default 5.",
                    "minimum": 1,
                    "maximum": 20
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let db = match self.db.as_ref() {
            Some(db) => db.clone(),
            None => {
                return Ok(ToolResult::err(
                    "database handle unavailable; cannot list IM bindings",
                ))
            }
        };
        let channel = match input["channel"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(channel) => channel.to_ascii_lowercase(),
            None => {
                return Ok(ToolResult::err(
                    "'channel' is required for im_channel_binding_list",
                ))
            }
        };
        let session_id = input["session_id"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let task_id = input["task_id"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let pool_id = input["pool_id"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let limit = input["limit"].as_u64().unwrap_or(5).clamp(1, 20) as usize;

        let mut bindings = Vec::new();
        {
            let db = db.lock().await;
            if let Some(pool_id) = pool_id {
                if let Ok(Some(binding)) = db.find_im_session_binding_for_pool(pool_id) {
                    if binding.channel.eq_ignore_ascii_case(&channel) {
                        bindings.push(binding);
                    }
                }
            }
            let narrowed_session = session_id
                .map(str::to_string)
                .or_else(|| task_id.map(scheduler_session_id));
            if let Some(session_id) = narrowed_session {
                if let Ok(Some(binding)) =
                    db.get_im_session_binding_by_session(&session_id, &channel)
                {
                    if !bindings
                        .iter()
                        .any(|existing: &crate::store::db::ImSessionBinding| {
                            existing.binding_key == binding.binding_key
                        })
                    {
                        bindings.push(binding);
                    }
                }
            }
            let recent = match db.list_im_session_bindings(Some(&channel), limit) {
                Ok(list) => list,
                Err(err) => {
                    return Ok(ToolResult::err(format!(
                        "failed to list IM bindings for channel '{}': {}",
                        channel, err
                    )))
                }
            };
            for binding in recent {
                if !bindings
                    .iter()
                    .any(|existing| existing.binding_key == binding.binding_key)
                {
                    bindings.push(binding);
                }
            }
        }

        let live_status = match self.gateway.as_ref() {
            Some(gateway) => gateway
                .list_channels()
                .await
                .into_iter()
                .find(|item| item.name.eq_ignore_ascii_case(&channel)),
            None => None,
        };

        Ok(ToolResult::ok(
            json!({
                "channel": channel,
                "candidates": bindings
                    .iter()
                    .map(|binding| render_binding_json(binding, live_status.as_ref()))
                    .collect::<Vec<_>>(),
            })
            .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pisci_kernel::agent::tool::ToolSettings;
    use pisci_kernel::store::db::ImSessionBindingUpsert;
    use pisci_kernel::store::Settings;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::Mutex;

    fn test_ctx() -> ToolContext {
        ToolContext {
            session_id: "sess_123".to_string(),
            workspace_root: PathBuf::from("/tmp"),
            bypass_permissions: false,
            settings: Arc::new(ToolSettings::default()),
            max_iterations: None,
            memory_owner_id: "pisci".to_string(),
            pool_session_id: Some("pool_123".to_string()),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn resolve_lookup_prefers_explicit_binding_key() {
        let ctx = test_ctx();
        let (kind, value) = resolve_lookup_key(
            &json!({
                "binding_key": "wechat::u:1",
                "session_id": "sess_other"
            }),
            &ctx,
        )
        .unwrap();
        assert_eq!(kind, "binding_key");
        assert_eq!(value, "wechat::u:1");
    }

    #[test]
    fn resolve_lookup_uses_current_session_when_requested() {
        let ctx = test_ctx();
        let (kind, value) = resolve_lookup_key(
            &json!({
                "use_current_session": true
            }),
            &ctx,
        )
        .unwrap();
        assert_eq!(kind, "session_id");
        assert_eq!(value, "sess_123");
    }

    #[test]
    fn resolve_lookup_falls_back_to_pool_context() {
        let ctx = test_ctx();
        let (kind, value) = resolve_lookup_key(&json!({}), &ctx).unwrap();
        assert_eq!(kind, "pool_id");
        assert_eq!(value, "pool_123");
    }

    #[test]
    fn scheduler_task_session_id_is_stable() {
        assert_eq!(scheduler_session_id("task_9"), "sched_task_9");
    }

    #[test]
    fn configured_channels_reflect_enabled_settings() {
        let settings = Settings {
            feishu_enabled: true,
            wechat_enabled: true,
            ..Settings::default()
        };
        let channels = configured_channels_from_settings(&settings);
        assert_eq!(channels, vec!["feishu", "wechat"]);
    }

    async fn seed_binding_db() -> Arc<Mutex<Database>> {
        let db = Database::open_in_memory().expect("in-memory db");
        db.ensure_im_session("sess_123", "wechat", "im_wechat")
            .expect("seed session");
        db.ensure_im_session("sched_task_42", "wechat", "scheduled_task")
            .expect("seed task session");

        db.upsert_im_session_binding(&ImSessionBindingUpsert {
            binding_key: "wechat::dm:user-1".to_string(),
            channel: "wechat".to_string(),
            external_conversation_key: "dm:user-1".to_string(),
            session_id: "sess_123".to_string(),
            peer_id: "user-1".to_string(),
            peer_name: Some("Alice".to_string()),
            is_group: false,
            group_name: None,
            latest_reply_target: "user-1|ctx-a".to_string(),
            routing_state_json: Some(r#"{"context_token":"ctx-a"}"#.to_string()),
        })
        .expect("seed main binding");

        db.upsert_im_session_binding(&ImSessionBindingUpsert {
            binding_key: "wechat::dm:user-2".to_string(),
            channel: "wechat".to_string(),
            external_conversation_key: "dm:user-2".to_string(),
            session_id: "sched_task_42".to_string(),
            peer_id: "user-2".to_string(),
            peer_name: Some("Bob".to_string()),
            is_group: false,
            group_name: None,
            latest_reply_target: "user-2|ctx-b".to_string(),
            routing_state_json: None,
        })
        .expect("seed task binding");

        let pool = db
            .create_pool_session("pool with im", 0)
            .expect("create pool");
        db.set_pool_origin_im_binding(&pool.id, Some("wechat::dm:user-1"))
            .expect("attach pool binding");

        Arc::new(Mutex::new(db))
    }

    #[tokio::test]
    async fn binding_lookup_resolves_session_binding() {
        let tool = ImChannelBindingLookupTool {
            db: Some(seed_binding_db().await),
            gateway: Some(Arc::new(GatewayManager::new())),
        };

        let result = tool
            .call(json!({ "session_id": "sess_123" }), &test_ctx())
            .await
            .expect("tool call");

        assert!(!result.is_error);
        let payload: Value = serde_json::from_str(&result.content).expect("json payload");
        assert_eq!(payload["found"], json!(true));
        assert_eq!(
            payload["binding"]["binding_key"],
            json!("wechat::dm:user-1")
        );
        assert_eq!(payload["lookup"]["kind"], json!("session_id"));
        assert_eq!(payload["usable"], json!(false));
    }

    #[tokio::test]
    async fn binding_lookup_resolves_pool_binding() {
        let db = seed_binding_db().await;
        let pool_id = {
            let db = db.lock().await;
            db.list_pool_sessions().expect("list pools")[0].id.clone()
        };
        let tool = ImChannelBindingLookupTool {
            db: Some(db),
            gateway: None,
        };

        let result = tool
            .call(json!({ "pool_id": pool_id }), &test_ctx())
            .await
            .expect("tool call");

        assert!(!result.is_error);
        let payload: Value = serde_json::from_str(&result.content).expect("json payload");
        assert_eq!(payload["found"], json!(true));
        assert_eq!(payload["lookup"]["kind"], json!("pool_id"));
        assert_eq!(payload["binding"]["peer_name"], json!("Alice"));
    }

    #[tokio::test]
    async fn binding_lookup_resolves_task_binding() {
        let tool = ImChannelBindingLookupTool {
            db: Some(seed_binding_db().await),
            gateway: None,
        };

        let result = tool
            .call(json!({ "task_id": "task_42" }), &test_ctx())
            .await
            .expect("tool call");

        assert!(!result.is_error);
        let payload: Value = serde_json::from_str(&result.content).expect("json payload");
        assert_eq!(payload["found"], json!(true));
        assert_eq!(payload["lookup"]["kind"], json!("task_id"));
        assert_eq!(
            payload["binding"]["binding_key"],
            json!("wechat::dm:user-2")
        );
    }

    #[tokio::test]
    async fn binding_lookup_returns_not_found_payload() {
        let db = Arc::new(Mutex::new(
            Database::open_in_memory().expect("in-memory db"),
        ));
        let tool = ImChannelBindingLookupTool {
            db: Some(db),
            gateway: None,
        };

        let result = tool
            .call(json!({ "session_id": "missing-session" }), &test_ctx())
            .await
            .expect("tool call");

        assert!(!result.is_error);
        let payload: Value = serde_json::from_str(&result.content).expect("json payload");
        assert_eq!(payload["found"], json!(false));
        assert_eq!(payload["lookup"]["value"], json!("missing-session"));
    }

    #[tokio::test]
    async fn binding_list_filters_candidates_by_channel_and_task() {
        let tool = ImChannelBindingListTool {
            db: Some(seed_binding_db().await),
            gateway: None,
        };

        let result = tool
            .call(
                json!({ "channel": "wechat", "task_id": "task_42" }),
                &test_ctx(),
            )
            .await
            .expect("tool call");

        assert!(!result.is_error);
        let payload: Value = serde_json::from_str(&result.content).expect("json payload");
        let candidates = payload["candidates"].as_array().expect("candidate array");
        assert!(!candidates.is_empty());
        assert_eq!(candidates[0]["binding_key"], json!("wechat::dm:user-2"));
        assert_eq!(candidates[0]["channel"], json!("wechat"));
    }
}
