/// Agent Loop — the core recursive query-tool-result cycle.
use super::messages::AgentEvent;
use super::tool::{ToolContext, ToolRegistry};
use crate::llm::{ContentBlock, ImageSource, LlmClient, LlmMessage, LlmRequest, MessageContent};
use crate::policy::{PolicyDecision, PolicyGate};
use crate::store::Database;
use anyhow::Result;
use futures::future::join_all;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

const DEFAULT_MAX_ITERATIONS: usize = 50;
const TOOL_TIMEOUT_SECS: u64 = 120;
const LLM_MAX_RETRIES: u32 = 3;
const READ_TOOL_MAX_CONCURRENCY: usize = 4;

static TOOL_RATE_STATE: Lazy<Mutex<HashMap<String, Vec<std::time::Instant>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// User-controlled confirmation flags from Settings.
#[derive(Debug, Clone)]
pub struct ConfirmFlags {
    pub confirm_shell: bool,
    pub confirm_file_write: bool,
}

type ConfirmationResponseMap = Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>>;

pub struct AgentLoop {
    pub client: Box<dyn LlmClient>,
    pub registry: Arc<ToolRegistry>,
    pub policy: Arc<PolicyGate>,
    pub system_prompt: String,
    pub model: String,
    pub max_tokens: u32,
    /// Optional database for audit logging
    pub db: Option<Arc<Mutex<Database>>>,
    /// App handle for emitting permission request events
    pub app_handle: Option<tauri::AppHandle>,
    /// Shared map of pending permission confirmation channels
    pub confirmation_responses: Option<ConfirmationResponseMap>,
    /// User confirmation preferences from Settings
    pub confirm_flags: ConfirmFlags,
}

impl AgentLoop {
    /// Execute a single tool call with policy checks, permission handling, timeout, audit logging.
    async fn execute_single_tool(
        &self,
        id: &str,
        name: &str,
        input: &serde_json::Value,
        ctx: &ToolContext,
        event_tx: &mpsc::Sender<AgentEvent>,
        _cancel: &Arc<AtomicBool>,
    ) -> Vec<ContentBlock> {
        let span = tracing::info_span!("tool_exec", tool = %name, session_id = %ctx.session_id);
        info!(parent: &span, "executing tool");
        let trace_id = uuid::Uuid::new_v4().simple().to_string();
        let mut blocks = Vec::new();

        if let Some(wait_reason) = self.check_tool_rate_limit(ctx).await {
            let _ = event_tx.send(AgentEvent::ToolEnd {
                id: id.to_string(),
                name: name.to_string(),
                result: wait_reason.clone(),
                is_error: true,
            }).await;
            blocks.push(ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: wait_reason,
                is_error: true,
            });
            return blocks;
        }

        // Policy check
        let decision = self.policy.check_tool_call(name, input);
        match &decision {
            PolicyDecision::Deny(reason) => {
                warn!("Tool '{}' denied by policy: {}", name, reason);
                let _ = event_tx.send(AgentEvent::ToolEnd {
                    id: id.to_string(), name: name.to_string(),
                    result: format!("Denied by policy: {}", reason), is_error: true,
                }).await;
                blocks.push(ContentBlock::ToolResult {
                    tool_use_id: id.to_string(), content: format!("Error: {}", reason), is_error: true,
                });
                return blocks;
            }
            PolicyDecision::Warn(msg) => {
                let tool_wants_confirm = self.registry.get(name)
                    .map(|t| t.needs_confirmation(input)).unwrap_or(false);
                let user_disabled = match name {
                    "shell" | "bash" | "powershell" | "powershell_query" => !self.confirm_flags.confirm_shell,
                    "file_write" | "file_edit" => !self.confirm_flags.confirm_file_write,
                    _ => false,
                };
                if tool_wants_confirm && !user_disabled {
                    if let (Some(_app), Some(confirms)) = (&self.app_handle, &self.confirmation_responses) {
                        let request_id = uuid::Uuid::new_v4().to_string();
                        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                        { confirms.lock().await.insert(request_id.clone(), resp_tx); }
                        let _ = event_tx.send(AgentEvent::PermissionRequest {
                            request_id, tool_name: name.to_string(),
                            tool_input: input.clone(), description: msg.clone(),
                        }).await;
                        match tokio::time::timeout(std::time::Duration::from_secs(60), resp_rx).await {
                            Ok(Ok(true)) => { debug!("User approved tool '{}' execution", name); }
                            _ => {
                                warn!("Tool '{}' denied by user or timed out", name);
                                let _ = event_tx.send(AgentEvent::ToolEnd {
                                    id: id.to_string(), name: name.to_string(),
                                    result: "Denied by user".into(), is_error: true,
                                }).await;
                                blocks.push(ContentBlock::ToolResult {
                                    tool_use_id: id.to_string(), content: "User denied this operation".into(), is_error: true,
                                });
                                return blocks;
                            }
                        }
                    }
                } else {
                    warn!("Tool '{}' policy warning: {}", name, msg);
                }
            }
            PolicyDecision::Allow => {}
        }

        let mut input_with_trace = input.clone();
        if let Some(obj) = input_with_trace.as_object_mut() {
            obj.insert("_trace_id".into(), serde_json::Value::String(trace_id.clone()));
        }
        let _ = event_tx.send(AgentEvent::ToolStart {
            id: id.to_string(), name: name.to_string(), input: input_with_trace,
        }).await;

        let result = match self.registry.get(name) {
            Some(tool) => {
                debug!("Executing tool: {}", name);
                match tokio::time::timeout(
                    std::time::Duration::from_secs(TOOL_TIMEOUT_SECS),
                    tool.call(input.clone(), ctx),
                ).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        warn!("Tool '{}' error: {}", name, e);
                        super::tool::ToolResult::err(format!("Tool error: {}", e))
                    }
                    Err(_) => {
                        warn!("Tool '{}' timed out after {}s", name, TOOL_TIMEOUT_SECS);
                        super::tool::ToolResult::err(format!("Tool '{}' timed out after {} seconds", name, TOOL_TIMEOUT_SECS))
                    }
                }
            }
            None => super::tool::ToolResult::err(format!("Tool '{}' not found", name)),
        };

        let end_result = format!("[trace_id:{}] {}", trace_id, result.content);
        let _ = event_tx.send(AgentEvent::ToolEnd {
            id: id.to_string(), name: name.to_string(),
            result: end_result, is_error: result.is_error,
        }).await;

        if let Some(ref db_arc) = self.db {
            let action = format!("{} [trace:{}]", audit_action_label(name, input), trace_id);
            let redacted_input = self.policy.redact_text(&summarize_tool_input(name, input));
            let redacted_result = self.policy.redact_text(&result.content);
            let input_summary = Some(truncate_str(&redacted_input, 300));
            let result_summary = Some(truncate_str(&redacted_result, 200));
            let is_err = result.is_error;
            let tool_name_clone = name.to_string();
            let session_id_clone = ctx.session_id.clone();
            let db_clone = db_arc.clone();
            tokio::spawn(async move {
                let db = db_clone.lock().await;
                let _ = db.append_audit(&session_id_clone, &tool_name_clone, &action, input_summary.as_deref(), result_summary.as_deref(), is_err);
            });
        }

        blocks.push(ContentBlock::ToolResult {
            tool_use_id: id.to_string(), content: result.content, is_error: result.is_error,
        });
        if let Some(img) = result.image {
            blocks.push(ContentBlock::Image {
                source: ImageSource { source_type: "base64".into(), media_type: img.media_type, data: img.base64 },
            });
        }
        blocks
    }

    async fn check_tool_rate_limit(&self, ctx: &ToolContext) -> Option<String> {
        let limit = self.policy.tool_rate_limit_per_minute as usize;
        if limit == 0 {
            return None;
        }
        let now = std::time::Instant::now();
        let mut state = TOOL_RATE_STATE.lock().await;
        let entries = state.entry(ctx.session_id.clone()).or_default();
        entries.retain(|t| now.duration_since(*t).as_secs() < 60);
        if entries.len() >= limit {
            return Some(format!(
                "Tool rate limit exceeded for session '{}' ({} calls/min)",
                ctx.session_id, limit
            ));
        }
        entries.push(now);
        None
    }

    /// Run the agent loop for a single user turn.
    ///
    /// Sends `AgentEvent`s through `event_tx` for streaming to the frontend.
    /// Returns `(final_messages, input_tokens, output_tokens)` when the LLM produces
    /// a final response with no tool calls, when `cancel` is set, or after MAX_ITERATIONS.
    ///
    /// NOTE: The caller is responsible for emitting `AgentEvent::Done` AFTER persisting
    /// the result to the database, to avoid a race condition where the frontend reloads
    /// messages before the DB write completes.
    pub async fn run(
        &self,
        mut messages: Vec<LlmMessage>,
        event_tx: mpsc::Sender<AgentEvent>,
        cancel: Arc<AtomicBool>,
        ctx: ToolContext,
    ) -> Result<(Vec<LlmMessage>, u32, u32)> {
        let span = tracing::info_span!("agent_loop", session_id = %ctx.session_id, model = %self.model);
        let _enter = span.enter();
        drop(_enter); // Don't hold across awaits — use span for structured correlation only
        info!(parent: &span, "agent loop starting");
        let mut total_input = 0u32;
        let mut total_output = 0u32;

        // Check for a resumable checkpoint from a previous (crashed) run
        if let Some(ref db_arc) = self.db {
            let db = db_arc.lock().await;
            match db.load_checkpoint(&ctx.session_id) {
                Ok(Some((iter, json))) => {
                    info!("Resuming from checkpoint at iteration {} for session {}", iter, ctx.session_id);
                    match serde_json::from_str::<Vec<LlmMessage>>(&json) {
                        Ok(saved) if !saved.is_empty() => {
                            messages = saved;
                            info!("Checkpoint restored: {} messages", messages.len());
                            // Mark checkpoint as consumed immediately to prevent re-use on next run
                            let _ = db.finish_checkpoint(&ctx.session_id, "resumed");
                        }
                        _ => {
                            warn!("Checkpoint JSON invalid; clearing and starting from scratch");
                            let _ = db.finish_checkpoint(&ctx.session_id, "invalid");
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => warn!("Could not load checkpoint: {}", e),
            }
        }

        let max_iterations = ctx.max_iterations.unwrap_or(DEFAULT_MAX_ITERATIONS as u32) as usize;
        for _iteration in 0..max_iterations {
            if cancel.load(Ordering::Relaxed) {
                break;
            }

            info!("agent loop iteration={} messages={}", _iteration, messages.len());

            // Build request
            let req = LlmRequest {
                messages: messages.clone(),
                system: Some(self.system_prompt.clone()),
                tools: self.registry.to_tool_defs(),
                model: self.model.clone(),
                max_tokens: self.max_tokens,
                stream: true,
            };

            // Call LLM with exponential-backoff retry for transient failures
            info!("calling LLM: model={}", self.model);
            let response = {
                let mut last_err = None;
                let mut resp = None;
                for attempt in 0..LLM_MAX_RETRIES {
                    match self.client.complete(req.clone()).await {
                        Ok(r) => { resp = Some(r); break; }
                        Err(e) => {
                            let msg = e.to_string();
                            let is_transient = msg.contains("timeout")
                                || msg.contains("connection")
                                || msg.contains("502")
                                || msg.contains("503")
                                || msg.contains("529")
                                || msg.contains("overloaded");
                            warn!("LLM call attempt {}/{} failed: {}", attempt + 1, LLM_MAX_RETRIES, msg);
                            if !is_transient || attempt + 1 == LLM_MAX_RETRIES {
                                last_err = Some(e);
                                break;
                            }
                            let backoff = std::time::Duration::from_secs(1 << attempt);
                            tokio::time::sleep(backoff).await;
                            last_err = Some(e);
                        }
                    }
                }
                match resp {
                    Some(r) => r,
                    None => return Err(last_err.unwrap_or_else(|| anyhow::anyhow!("LLM call failed"))),
                }
            };
            info!("LLM response: input_tokens={} output_tokens={} tool_calls={} text_len={}",
                response.input_tokens, response.output_tokens,
                response.tool_calls.len(), response.content.len());
            total_input += response.input_tokens;
            total_output += response.output_tokens;

            let text_buf = response.content.clone();
            let tool_calls: Vec<(String, String, serde_json::Value)> = response
                .tool_calls
                .iter()
                .map(|tc| (tc.id.clone(), tc.name.clone(), tc.input.clone()))
                .collect();

            // Emit text delta as a single event
            if !text_buf.is_empty() {
                let _ = event_tx.send(AgentEvent::TextDelta { delta: text_buf.clone() }).await;
            }

            // If no tool calls, we're done
            if tool_calls.is_empty() {
                // Add assistant message
                messages.push(LlmMessage {
                    role: "assistant".into(),
                    content: MessageContent::text(&text_buf),
                });
                break;
            }

            // Build assistant message with tool calls
            let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
            if !text_buf.is_empty() {
                assistant_blocks.push(ContentBlock::Text { text: text_buf.clone() });
            }
            for (id, name, input) in &tool_calls {
                assistant_blocks.push(ContentBlock::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
            }
            messages.push(LlmMessage {
                role: "assistant".into(),
                content: MessageContent::Blocks(assistant_blocks),
            });

            // Execute tools — read-only tools run concurrently, write tools run serially
            let mut tool_result_blocks: Vec<ContentBlock> = Vec::new();

            if cancel.load(Ordering::Relaxed) {
                break;
            }

            // Partition into read-only and write groups (preserving order)
            let read_only_calls: Vec<_> = tool_calls.iter()
                .filter(|(_, name, _)| self.registry.get(name).map(|t| t.is_read_only()).unwrap_or(false))
                .cloned().collect();
            let write_calls: Vec<_> = tool_calls.iter()
                .filter(|(_, name, _)| !self.registry.get(name).map(|t| t.is_read_only()).unwrap_or(false))
                .cloned().collect();

            // Execute read-only tools concurrently
            if !read_only_calls.is_empty() {
                let mut start = 0usize;
                while start < read_only_calls.len() {
                    let end = (start + READ_TOOL_MAX_CONCURRENCY).min(read_only_calls.len());
                    let batch = &read_only_calls[start..end];
                    let futs: Vec<_> = batch
                        .iter()
                        .map(|(id, name, input)| {
                            self.execute_single_tool(id, name, input, &ctx, &event_tx, &cancel)
                        })
                        .collect();
                    for blocks in join_all(futs).await {
                        tool_result_blocks.extend(blocks);
                    }
                    start = end;
                }
            }

            // Execute write tools serially
            for (id, name, input) in &write_calls {
                if cancel.load(Ordering::Relaxed) { break; }
                let blocks = self.execute_single_tool(id, name, input, &ctx, &event_tx, &cancel).await;
                tool_result_blocks.extend(blocks);
            }

            // Add tool results as user message
            messages.push(LlmMessage {
                role: "user".into(),
                content: MessageContent::Blocks(tool_result_blocks),
            });

            // Write checkpoint after each iteration so a crash can be resumed
            if let Some(ref db_arc) = self.db {
                let db = db_arc.lock().await;
                match serde_json::to_string(&messages) {
                    Ok(json) => {
                        if let Err(e) = db.upsert_checkpoint(&ctx.session_id, _iteration, &json) {
                            warn!("Failed to write checkpoint: {}", e);
                        }
                    }
                    Err(e) => warn!("Failed to serialise checkpoint messages: {}", e),
                }
            }
        }

        // Mark checkpoint as completed so it won't be resumed next run
        if let Some(ref db_arc) = self.db {
            let db = db_arc.lock().await;
            let _ = db.finish_checkpoint(&ctx.session_id, "completed");
            // Prune checkpoints older than 24 hours
            let _ = db.prune_checkpoints(24);
        }

        // Return token counts to the caller — it is the caller's responsibility to emit
        // AgentEvent::Done AFTER persisting the result to the database.
        Ok((messages, total_input, total_output))
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max {
        s.to_string()
    } else {
        let end = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
        format!("{}...", &s[..end])
    }
}

fn summarize_tool_input(tool_name: &str, input: &serde_json::Value) -> String {
    if tool_name == "browser" {
        let action = input["action"].as_str().unwrap_or("unknown");
        let mut parts = vec![format!("action={}", action)];
        if let Some(v) = input["url"].as_str() {
            parts.push(format!("url={}", v));
        }
        if let Some(v) = input["selector"].as_str() {
            parts.push(format!("selector={}", v));
        }
        if let Some(v) = input["tab_id"].as_str() {
            parts.push(format!("tab_id={}", v));
        }
        if let Some(v) = input["wait_condition"].as_str() {
            parts.push(format!("wait_condition={}", v));
        }
        return parts.join(", ");
    }
    input.to_string()
}

/// Generate a short human-readable label for the audit log's "action" column.
/// Each tool has a primary identifying field; fall back to the tool name itself.
fn audit_action_label(tool_name: &str, input: &serde_json::Value) -> String {
    fn truncate(s: &str, n: usize) -> String {
        if s.chars().count() <= n {
            s.to_string()
        } else {
            let t: String = s.chars().take(n).collect();
            format!("{}…", t)
        }
    }

    match tool_name {
        "shell" | "powershell" => {
            let cmd = input["command"].as_str().unwrap_or("");
            truncate(cmd, 60)
        }
        "powershell_query" => {
            let cmd = input["command"].as_str()
                .or_else(|| input["query"].as_str())
                .unwrap_or("");
            truncate(cmd, 60)
        }
        "file_read" => {
            let path = input["path"].as_str().unwrap_or("");
            format!("read {}", truncate(path, 55))
        }
        "file_write" => {
            let path = input["path"].as_str().unwrap_or("");
            format!("write {}", truncate(path, 54))
        }
        "web_search" => {
            let q = input["query"].as_str().unwrap_or("");
            truncate(q, 60)
        }
        "browser" => {
            let action = input["action"].as_str().unwrap_or("?");
            if let Some(url) = input["url"].as_str() {
                format!("{} {}", action, truncate(url, 50))
            } else if let Some(sel) = input["selector"].as_str() {
                format!("{} {}", action, truncate(sel, 50))
            } else {
                action.to_string()
            }
        }
        "screen_capture" => {
            input["mode"].as_str().unwrap_or("fullscreen").to_string()
        }
        "uia" => {
            let action = input["action"].as_str().unwrap_or("");
            if let Some(name) = input["name"].as_str() {
                format!("{} {}", action, truncate(name, 50))
            } else {
                action.to_string()
            }
        }
        "wmi" => {
            let q = input["query"].as_str().unwrap_or("");
            truncate(q, 60)
        }
        "com" => {
            let prog = input["prog_id"].as_str().unwrap_or("");
            let method = input["method"].as_str().unwrap_or("");
            if prog.is_empty() {
                method.to_string()
            } else {
                format!("{}.{}", prog, method)
            }
        }
        "office" => {
            let action = input["action"].as_str().unwrap_or("");
            let path = input["path"].as_str().unwrap_or("");
            format!("{} {}", action, truncate(path, 50))
        }
        _ => {
            // Generic: find the first non-empty string value
            if let Some(obj) = input.as_object() {
                for (_, v) in obj.iter().take(3) {
                    if let Some(s) = v.as_str() {
                        if !s.is_empty() {
                            return truncate(s, 60);
                        }
                    }
                }
            }
            tool_name.to_string()
        }
    }
}
