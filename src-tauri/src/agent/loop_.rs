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
    /// User-configured vision override (from settings.vision_enabled).
    /// None = auto-detect from model name.
    pub vision_override: Option<bool>,
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
                // Log key input fields to aid debugging (path, command, query, etc.)
                let input_hint = match name {
                    "file_read" | "file_write" => input["path"].as_str().unwrap_or("?").to_string(),
                    "shell" => format!(
                        "[{}] {}",
                        input["interpreter"].as_str().unwrap_or("powershell"),
                        input["command"].as_str().unwrap_or("?").chars().take(100).collect::<String>()
                    ),
                    "powershell_query" => format!(
                        "query={} arch={}",
                        input["query"].as_str().unwrap_or("?"),
                        input["arch"].as_str().unwrap_or("x64")
                    ),
                    "web_search" => input["query"].as_str().unwrap_or("?").chars().take(80).collect(),
                    "browser" => format!("action={} url={}", input["action"].as_str().unwrap_or("?"), input["url"].as_str().unwrap_or("")),
                    "com_invoke" => format!(
                        "action={} prog_id={} arch={}",
                        input["action"].as_str().unwrap_or("?"),
                        input["prog_id"].as_str().unwrap_or("?"),
                        input["arch"].as_str().unwrap_or("x64")
                    ),
                    "wmi" => format!(
                        "preset={} query={}",
                        input["preset"].as_str().unwrap_or(""),
                        input["query"].as_str().unwrap_or("?").chars().take(80).collect::<String>()
                    ),
                    "uia" => format!(
                        "action={} name={} window={}",
                        input["action"].as_str().unwrap_or("?"),
                        input["name"].as_str().unwrap_or(""),
                        input["window_title"].as_str().unwrap_or("")
                    ),
                    _ => input.to_string().chars().take(100).collect(),
                };
                debug!("Executing tool: {} | input: {}", name, input_hint);
                match tokio::time::timeout(
                    std::time::Duration::from_secs(TOOL_TIMEOUT_SECS),
                    tool.call(input.clone(), ctx),
                ).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        let err_msg = e.to_string();
                        warn!("Tool '{}' error: {} | input: {}", name, err_msg, input_hint);
                        // Provide actionable error messages for common failure patterns
                        let friendly = friendly_tool_error(name, &err_msg);
                        super::tool::ToolResult::err(friendly)
                    }
                    Err(_) => {
                        warn!("Tool '{}' timed out after {}s", name, TOOL_TIMEOUT_SECS);
                        super::tool::ToolResult::err(format!(
                            "工具 '{}' 执行超时（{}秒）。可能原因：命令阻塞、网络超时或进程挂起。请尝试简化命令或分步执行。",
                            name, TOOL_TIMEOUT_SECS
                        ))
                    }
                }
            }
            None => {
                warn!("Tool '{}' not found in registry", name);
                let available: Vec<String> = self.registry.all().iter().map(|t| t.name().to_string()).collect();
                super::tool::ToolResult::err(format!(
                    "工具 '{}' 未找到。当前可用工具：{}。请检查工具名称是否正确，或在设置中启用该工具。",
                    name,
                    available.join(", ")
                ))
            }
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

            // Signal frontend that a new LLM call is starting — it should replace the
            // current streaming bubble with a fresh one (slide old out, slide new in).
            let _ = event_tx.send(AgentEvent::TextSegmentStart {
                iteration: _iteration as u32 + 1,
            }).await;

            // Build request
            let req = LlmRequest {
                messages: messages.clone(),
                system: Some(self.system_prompt.clone()),
                tools: self.registry.to_tool_defs(),
                model: self.model.clone(),
                max_tokens: self.max_tokens,
                stream: true,
                vision_override: self.vision_override,
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

/// Convert low-level tool errors into actionable, user-friendly messages.
fn friendly_tool_error(tool_name: &str, raw_error: &str) -> String {
    let raw_lower = raw_error.to_lowercase();

    // File system errors
    if raw_lower.contains("no such file") || raw_lower.contains("not found") || raw_lower.contains("cannot find") {
        return format!(
            "[{}] 文件或路径不存在。请确认路径正确，或先用 file_write 创建文件。\n详情：{}",
            tool_name, raw_error
        );
    }
    if raw_lower.contains("permission denied") || raw_lower.contains("access is denied")
        || raw_lower.contains("拒绝访问") || raw_lower.contains("0x80070005")
    {
        if tool_name == "shell" || tool_name == "file_write" {
            return format!(
                "[{}] 权限不足（Access Denied）。\
                 如需管理员权限，请对 shell 工具使用 elevated: true 参数，\
                 Windows 会弹出 UAC 对话框请用户确认。\n详情：{}",
                tool_name, raw_error
            );
        }
        return format!(
            "[{}] 权限不足，无法访问该文件/目录。\
             如需管理员权限，请使用 shell 工具并设置 elevated: true。\n详情：{}",
            tool_name, raw_error
        );
    }
    if raw_lower.contains("already exists") {
        return format!(
            "[{}] 文件或目录已存在。如需覆盖，请使用 file_write（会自动覆盖）。\n详情：{}",
            tool_name, raw_error
        );
    }

    // Network errors
    if raw_lower.contains("connection refused") || raw_lower.contains("connection reset") {
        return format!(
            "[{}] 网络连接失败。请检查网络连接或目标服务是否可用。\n详情：{}",
            tool_name, raw_error
        );
    }
    if raw_lower.contains("timeout") || raw_lower.contains("timed out") {
        return format!(
            "[{}] 网络请求超时。请检查网络状态，或稍后重试。\n详情：{}",
            tool_name, raw_error
        );
    }
    if raw_lower.contains("dns") || raw_lower.contains("resolve") || raw_lower.contains("no route") {
        return format!(
            "[{}] DNS 解析失败，无法访问目标地址。请检查网络连接。\n详情：{}",
            tool_name, raw_error
        );
    }

    // Shell/process errors
    if tool_name == "shell" || tool_name == "powershell_query" {
        if raw_lower.contains("not recognized") || raw_lower.contains("not found") {
            return format!(
                "[{}] 命令未找到。请确认命令名称正确，或该程序已安装并在 PATH 中。\n详情：{}",
                tool_name, raw_error
            );
        }
        if raw_lower.contains("exit code") {
            return format!(
                "[{}] 命令执行失败（非零退出码）。请检查命令语法和参数。\n详情：{}",
                tool_name, raw_error
            );
        }
    }

    // Browser errors
    if tool_name == "browser" {
        if raw_lower.contains("chrome") || raw_lower.contains("browser") || raw_lower.contains("cdp") {
            return format!(
                "[{}] 浏览器连接失败。请确认 Chrome 已安装，或在设置中检查浏览器配置。\n详情：{}",
                tool_name, raw_error
            );
        }
        if raw_lower.contains("element") || raw_lower.contains("selector") {
            return format!(
                "[{}] 页面元素未找到。页面可能尚未加载完成，或选择器有误。建议先截图确认页面状态。\n详情：{}",
                tool_name, raw_error
            );
        }
    }

    // WMI / COM errors
    if tool_name == "wmi" || tool_name == "com" {
        if raw_lower.contains("wmi") || raw_lower.contains("com") || raw_lower.contains("dispatch") {
            return format!(
                "[{}] Windows 系统接口调用失败。请确认以管理员权限运行，或该功能在当前系统版本可用。\n详情：{}",
                tool_name, raw_error
            );
        }
    }

    // com_invoke errors
    if tool_name == "com_invoke" {
        if raw_lower.contains("regdb_e_classnotreg") || raw_lower.contains("0x80040154") {
            return format!(
                "[com_invoke] COM 对象未注册（REGDB_E_CLASSNOTREG）。\
                 最常见原因：该 COM 对象是 32 位组件，需要用 arch=x86 参数。\
                 请重试并添加 arch: \"x86\"。\n详情：{}",
                raw_error
            );
        }
        if raw_lower.contains("0x80020009") || raw_lower.contains("disp_e_exception") {
            return format!(
                "[com_invoke] COM 方法调用抛出异常。请检查方法名称和参数是否正确。\n详情：{}",
                raw_error
            );
        }
        if raw_lower.contains("0x80070005") || raw_lower.contains("e_accessdenied") {
            return format!(
                "[com_invoke] COM 对象访问被拒绝。可能需要管理员权限，或该对象不允许外部调用。\n详情：{}",
                raw_error
            );
        }
        if raw_lower.contains("progid") || raw_lower.contains("new-object") {
            return format!(
                "[com_invoke] 无法创建 COM 对象。请确认 ProgID 正确，软件已安装，\
                 并尝试 arch=x86（32位软件）。\n详情：{}",
                raw_error
            );
        }
    }

    // Generic fallback
    format!("[{}] 工具执行失败：{}", tool_name, raw_error)
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
