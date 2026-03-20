/// Agent Loop — the core recursive query-tool-result cycle.
///
/// Runtime guards inspired by OpenClaw's middleware architecture:
/// - Per-tool loop detection (generic_repeat, known_poll, ping_pong, circuit_breaker)
/// - No-progress detection via result hash comparison
/// - Tool result size guard (dynamic, based on context window)
/// - In-memory message compaction for long-running tasks
/// - Checkpoint size guard for DB persistence
use super::messages::AgentEvent;
use super::tool::{ToolContext, ToolRegistry};
use super::vision;
use crate::llm::{ContentBlock, ImageSource, LlmClient, LlmMessage, LlmRequest, MessageContent};
use crate::policy::{PolicyDecision, PolicyGate};
use crate::store::Database;
use anyhow::Result;
use futures::future::join_all;
use once_cell::sync::Lazy;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
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

// ── Runtime guard thresholds (inspired by OpenClaw) ──────────────────────────
// OpenClaw uses 10/20/30; we use slightly lower values for desktop scenarios
// where iterations are more expensive and user patience is lower.
const TOOL_CALL_HISTORY_SIZE: usize = 25;
const WARNING_THRESHOLD: usize = 8;
const CRITICAL_THRESHOLD: usize = 16;
const CIRCUIT_BREAKER_THRESHOLD: usize = 25;
const PING_PONG_WARNING: usize = 8;
const PING_PONG_CRITICAL: usize = 16;
const TOOL_RESULT_HARD_MAX_CHARS: usize = 48_000;
const CONTEXT_SINGLE_RESULT_SHARE: f64 = 0.5;
const CHECKPOINT_MAX_BYTES: usize = 8_000_000;

/// Tools that are known polling/status-checking tools. These get stricter
/// no-progress detection (inspired by OpenClaw's known_poll_no_progress).
const KNOWN_POLL_TOOLS: &[&str] = &["process_control", "shell", "powershell_query"];

static TOOL_RATE_STATE: Lazy<Mutex<HashMap<String, Vec<std::time::Instant>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// User-controlled confirmation flags from Settings.
#[derive(Debug, Clone)]
pub struct ConfirmFlags {
    pub confirm_shell: bool,
    pub confirm_file_write: bool,
}

type ConfirmationResponseMap = Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>>;

// ── Loop Detection (per-tool tracking, inspired by OpenClaw) ─────────────────

/// Severity level for loop detection, matching OpenClaw's warning/critical model.
#[derive(Debug, Clone, Copy, PartialEq)]
enum LoopLevel {
    Ok,
    Warning,
    Critical,
}

/// Which detector triggered.
#[derive(Debug, Clone)]
enum LoopDetector {
    GenericRepeat,
    KnownPollNoProgress,
    PingPong,
    GlobalCircuitBreaker,
}

/// Result of loop detection analysis.
#[derive(Debug, Clone)]
struct LoopDetectionResult {
    level: LoopLevel,
    detector: Option<LoopDetector>,
    count: usize,
    message: String,
}

impl LoopDetectionResult {
    fn ok() -> Self {
        Self {
            level: LoopLevel::Ok,
            detector: None,
            count: 0,
            message: String::new(),
        }
    }
}

/// A single recorded tool call with its outcome, for per-tool history tracking.
#[derive(Clone, Debug)]
struct ToolCallRecord {
    name: String,
    input_hash: u64,
    result_hash: u64,
}

/// Per-session tool call history for loop detection.
/// Maintains a sliding window of recent tool calls (like OpenClaw's toolCallHistory).
struct LoopDetectorState {
    history: Vec<ToolCallRecord>,
}

impl LoopDetectorState {
    fn new() -> Self {
        Self {
            history: Vec::new(),
        }
    }

    /// Record a completed tool call with its result hash.
    fn record(&mut self, name: &str, input: &serde_json::Value, result_hash: u64) {
        let input_hash = stable_hash_input(name, input);
        self.history.push(ToolCallRecord {
            name: name.to_string(),
            input_hash,
            result_hash,
        });
        if self.history.len() > TOOL_CALL_HISTORY_SIZE {
            self.history.remove(0);
        }
    }

    /// Run all detectors against the current history, return the most severe result.
    fn detect(&self, pending_name: &str, pending_input: &serde_json::Value) -> LoopDetectionResult {
        let pending_hash = stable_hash_input(pending_name, pending_input);

        // 1. Global circuit breaker: same tool+input with no progress
        let no_progress_streak = self.count_no_progress_streak(pending_name, pending_hash);
        if no_progress_streak >= CIRCUIT_BREAKER_THRESHOLD {
            return LoopDetectionResult {
                level: LoopLevel::Critical,
                detector: Some(LoopDetector::GlobalCircuitBreaker),
                count: no_progress_streak,
                message: format!(
                    "全局熔断：工具 '{}' 已连续{}次调用且结果无变化，强制终止该工具调用。请换一种方法。",
                    pending_name, no_progress_streak
                ),
            };
        }

        // 2. Known poll tools: stricter thresholds for status-checking tools
        let is_poll = KNOWN_POLL_TOOLS.iter().any(|t| pending_name.contains(t));
        if is_poll {
            let streak = self.count_same_tool_streak(pending_name, pending_hash);
            if streak >= CRITICAL_THRESHOLD {
                return LoopDetectionResult {
                    level: LoopLevel::Critical,
                    detector: Some(LoopDetector::KnownPollNoProgress),
                    count: streak,
                    message: format!(
                        "轮询工具 '{}' 已连续调用{}次且无进展，强制终止。请检查目标状态或换一种方法。",
                        pending_name, streak
                    ),
                };
            }
            if streak >= WARNING_THRESHOLD {
                return LoopDetectionResult {
                    level: LoopLevel::Warning,
                    detector: Some(LoopDetector::KnownPollNoProgress),
                    count: streak,
                    message: format!(
                        "轮询工具 '{}' 已连续调用{}次，结果无变化。建议检查是否需要换一种方法或增加等待时间。",
                        pending_name, streak
                    ),
                };
            }
        }

        // 3. Ping-pong detection: A→B→A→B alternating pattern
        let ping_pong_count = self.detect_ping_pong(pending_name, pending_hash);
        if ping_pong_count >= PING_PONG_CRITICAL {
            return LoopDetectionResult {
                level: LoopLevel::Critical,
                detector: Some(LoopDetector::PingPong),
                count: ping_pong_count,
                message: format!(
                    "检测到工具交替调用循环（ping-pong），已持续{}次。强制终止，请分析原因并换一种方法。",
                    ping_pong_count
                ),
            };
        }
        if ping_pong_count >= PING_PONG_WARNING {
            return LoopDetectionResult {
                level: LoopLevel::Warning,
                detector: Some(LoopDetector::PingPong),
                count: ping_pong_count,
                message: format!(
                    "检测到工具交替调用模式，已持续{}次。请检查是否陷入了循环，考虑换一种方法。",
                    ping_pong_count
                ),
            };
        }

        // 4. Generic repeat: same tool+input appearing too many times
        let repeat_count = self.count_same_tool_total(pending_name, pending_hash);
        if repeat_count >= CRITICAL_THRESHOLD {
            return LoopDetectionResult {
                level: LoopLevel::Critical,
                detector: Some(LoopDetector::GenericRepeat),
                count: repeat_count,
                message: format!(
                    "工具 '{}' 以相同参数被调用了{}次，强制终止。请换一种方法解决问题。",
                    pending_name, repeat_count
                ),
            };
        }
        if repeat_count >= WARNING_THRESHOLD {
            return LoopDetectionResult {
                level: LoopLevel::Warning,
                detector: Some(LoopDetector::GenericRepeat),
                count: repeat_count,
                message: format!(
                    "工具 '{}' 以相同参数已被调用{}次。请检查是否需要换一种方法，避免无效重复。",
                    pending_name, repeat_count
                ),
            };
        }

        LoopDetectionResult::ok()
    }

    /// Count consecutive calls to the same tool+input at the tail of history
    /// where the result hash is also unchanged (no progress).
    fn count_no_progress_streak(&self, name: &str, input_hash: u64) -> usize {
        let mut count = 0usize;
        let mut last_result: Option<u64> = None;
        for rec in self.history.iter().rev() {
            if rec.name == name && rec.input_hash == input_hash {
                match last_result {
                    None => {
                        last_result = Some(rec.result_hash);
                        count += 1;
                    }
                    Some(lr) if lr == rec.result_hash => {
                        count += 1;
                    }
                    _ => break,
                }
            } else {
                break;
            }
        }
        count
    }

    /// Count consecutive calls to the same tool+input at the tail of history.
    fn count_same_tool_streak(&self, name: &str, input_hash: u64) -> usize {
        self.history
            .iter()
            .rev()
            .take_while(|r| r.name == name && r.input_hash == input_hash)
            .count()
    }

    /// Count total occurrences of the same tool+input in the history window.
    fn count_same_tool_total(&self, name: &str, input_hash: u64) -> usize {
        self.history
            .iter()
            .filter(|r| r.name == name && r.input_hash == input_hash)
            .count()
    }

    /// Detect A→B→A→B alternating pattern at the tail of history.
    /// Returns the number of alternating pairs found.
    fn detect_ping_pong(&self, pending_name: &str, pending_hash: u64) -> usize {
        if self.history.len() < 2 {
            return 0;
        }

        let last = self.history.last().unwrap();
        if last.name == pending_name && last.input_hash == pending_hash {
            return 0; // Same as last — not a ping-pong, it's a repeat
        }

        // Check if the pattern is: ...A, B, A, B where pending is A and last is B
        let a_name = pending_name;
        let a_hash = pending_hash;
        let b_name = &last.name;
        let b_hash = last.input_hash;

        let mut alternations = 0usize;
        let mut expect_b = true; // Walking backwards from last, first should be B
        for rec in self.history.iter().rev() {
            if expect_b && rec.name == *b_name && rec.input_hash == b_hash {
                alternations += 1;
                expect_b = false;
            } else if !expect_b && rec.name == a_name && rec.input_hash == a_hash {
                expect_b = true;
            } else {
                break;
            }
        }
        alternations
    }
}

/// Compute a stable hash for a tool name + normalized input.
fn stable_hash_input(name: &str, input: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    let mut normalized = input.clone();
    if let Some(obj) = normalized.as_object_mut() {
        obj.remove("_trace_id");
    }
    normalized.to_string().hash(&mut hasher);
    hasher.finish()
}

/// Compute a stable hash of a single tool result content string.
fn stable_hash_result(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

// ── Tool Result Guard ────────────────────────────────────────────────────────

/// Truncate a tool result string if it exceeds the limit, keeping head + tail.
/// The limit is the smaller of the hard max and a dynamic limit based on context window.
fn guard_tool_result_content(content: &str, max_chars: usize) -> String {
    let limit = max_chars.min(TOOL_RESULT_HARD_MAX_CHARS);
    let char_count = content.chars().count();
    if char_count <= limit {
        return content.to_string();
    }
    let head_size = (limit * 3) / 4;
    let tail_size = limit / 4;
    let head: String = content.chars().take(head_size).collect();
    let tail: String = content.chars().skip(char_count - tail_size).collect();
    format!(
        "{}\n\n[... truncated {} chars (limit: {}) ...]\n\n{}",
        head,
        char_count - head_size - tail_size,
        limit,
        tail
    )
}

/// Compute dynamic per-result char limit based on context window.
/// Inspired by OpenClaw's SINGLE_TOOL_RESULT_CONTEXT_SHARE.
fn dynamic_result_limit(context_window_tokens: usize) -> usize {
    let context_chars = context_window_tokens * 4; // ~4 chars per token
    let limit = (context_chars as f64 * CONTEXT_SINGLE_RESULT_SHARE) as usize;
    limit.clamp(4_000, TOOL_RESULT_HARD_MAX_CHARS)
}

// ── In-memory Message Compaction ─────────────────────────────────────────────

// ---------------------------------------------------------------------------
// Context compaction helpers
// ---------------------------------------------------------------------------

const CTX_TRIM_HEAD: usize = 1_000; // chars to keep from the start of a tool result
const CTX_TRIM_TAIL: usize = 300; // chars to keep from the end of a tool result
/// Minimum chars a tool result must exceed before it is eligible for trimming.
/// Prevents trimming results that are already small enough to be useful in full.
const CTX_TRIM_MIN_SIZE: usize = CTX_TRIM_HEAD + CTX_TRIM_TAIL + 100;
const SUMMARY_KEEP_RECENT_RATIO: f64 = 0.60; // keep newest 60% of budget intact

/// Level-1 compaction: trim oversized individual tool results (head + tail).
///
/// A result is trimmed when it exceeds BOTH `single_limit` (the per-result share
/// of the context budget) AND `CTX_TRIM_MIN_SIZE` (the absolute minimum worth
/// trimming). Using `min` ensures we never trim a result that is already within
/// the budget share, and never trim one that is too small to benefit from it.
fn compact_trim_tool_results(messages: &mut [LlmMessage], single_limit: usize) -> bool {
    // Effective threshold: trim only if the result exceeds the budget share AND
    // is large enough that trimming makes sense (> head + tail + 100 chars).
    let trim_threshold = single_limit.max(CTX_TRIM_MIN_SIZE);
    let mut changed = false;
    for msg in messages.iter_mut() {
        if msg.role != "user" {
            continue;
        }
        if let MessageContent::Blocks(ref mut blocks) = msg.content {
            for block in blocks.iter_mut() {
                if let ContentBlock::ToolResult { content, .. } = block {
                    // Collect chars once to avoid O(n) traversal three times.
                    let chars: Vec<char> = content.chars().collect();
                    let len = chars.len();
                    if len > trim_threshold {
                        let head: String = chars[..CTX_TRIM_HEAD].iter().collect();
                        let tail_start = len.saturating_sub(CTX_TRIM_TAIL);
                        let tail: String = chars[tail_start..].iter().collect();
                        let removed = len - CTX_TRIM_HEAD - CTX_TRIM_TAIL;
                        *content =
                            format!("{}\n... [{} chars removed] ...\n{}", head, removed, tail);
                        changed = true;
                    }
                }
            }
        }
    }
    changed
}

/// Level-2 compaction: call LLM to summarise old messages.
///
/// Keeps the newest `keep_chars` worth of messages intact and summarises
/// everything older into a single user message prepended to the list.
/// Returns the new message list, or None if there was nothing to summarise.
async fn compact_summarise(
    messages: Vec<LlmMessage>,
    keep_chars: usize,
    client: &dyn crate::llm::LlmClient,
    model: &str,
    max_tokens: u32,
) -> Option<Vec<LlmMessage>> {
    if messages.len() < 2 {
        // Nothing meaningful to summarise if there are fewer than 2 messages.
        return None;
    }

    // Walk from the end, accumulating estimated chars until we exceed keep_chars.
    // Everything before the boundary index gets summarised.
    // We always keep at least the last 2 messages intact so the LLM has immediate
    // context regardless of how large they are.
    let mut acc = 0usize;
    // Default: summarise everything except the last 6 messages (3 tool call rounds).
    let mut split_idx = messages.len().saturating_sub(6);
    for (i, msg) in messages.iter().enumerate().rev() {
        // For tool-call messages (Blocks), estimate size from the serialised JSON
        // since as_text() returns empty string for ToolUse/ToolResult blocks.
        let chars = match &msg.content {
            crate::llm::MessageContent::Text(t) => crate::llm::estimate_tokens(t) * 4,
            crate::llm::MessageContent::Blocks(blocks) => {
                blocks.iter().map(|b| match b {
                    crate::llm::ContentBlock::Text { text } => text.len(),
                    crate::llm::ContentBlock::ToolUse { name, input, .. } => {
                        name.len() + input.to_string().len()
                    }
                    crate::llm::ContentBlock::ToolResult { content, .. } => content.len(),
                    _ => 0,
                }).sum::<usize>()
            }
        };
        acc += chars;
        // Once the tail accumulation exceeds keep_chars, everything before
        // index i belongs to the "old" region to be summarised.
        if acc >= keep_chars && i > 0 {
            split_idx = i;
            break;
        }
    }

    if split_idx == 0 {
        // All messages fit within keep_chars — nothing to summarise.
        return None;
    }

    let old_msgs = &messages[..split_idx];
    if old_msgs.is_empty() {
        return None;
    }

    let history_text: String = old_msgs
        .iter()
        .map(|m| {
            let role = if m.role == "user" {
                "用户/工具结果"
            } else {
                "智能体"
            };
            // as_text() returns empty for Blocks(ToolUse/ToolResult) — extract manually.
            let text = match &m.content {
                crate::llm::MessageContent::Text(t) => t.clone(),
                crate::llm::MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| match b {
                        crate::llm::ContentBlock::Text { text } => {
                            if text.is_empty() { None } else { Some(text.clone()) }
                        }
                        crate::llm::ContentBlock::ToolUse { name, input, .. } => {
                            let input_str = input.to_string();
                            let preview: String = input_str.chars().take(200).collect();
                            Some(format!("调用工具 {}: {}", name, preview))
                        }
                        crate::llm::ContentBlock::ToolResult { content, .. } => {
                            let preview: String = content.chars().take(200).collect();
                            Some(format!("工具结果: {}", preview))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            if text.is_empty() {
                return String::new();
            }
            let snippet = if text.chars().count() > 500 {
                format!("{}...", text.chars().take(500).collect::<String>())
            } else {
                text
            };
            format!("[{}]: {}", role, snippet)
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    let summary_prompt = format!(
        "请将以下对话历史压缩为一条简洁的摘要消息。\n\
         格式：用户要求[用户请求摘要]，智能体已完成[已完成的工作摘要]，当前状态[未完成事项或中间状态]。\n\
         保留关键文件路径、数据和结论，省略中间步骤细节。\n\n\
         对话历史：\n{}",
        history_text
    );

    let req = crate::llm::LlmRequest {
        messages: vec![crate::llm::LlmMessage {
            role: "user".into(),
            content: crate::llm::MessageContent::text(&summary_prompt),
        }],
        system: None,
        tools: vec![],
        model: model.to_string(),
        // Use at least 512 tokens for the summary regardless of the main model's
        // max_tokens setting, capped at 1024 to avoid wasting quota on a summary.
        max_tokens: max_tokens.clamp(512, 1024),
        stream: false,
        vision_override: Some(false),
    };

    match client.complete(req).await {
        Ok(resp) if !resp.content.is_empty() => {
            let summary_msg = crate::llm::LlmMessage {
                role: "user".into(),
                content: crate::llm::MessageContent::text(format!(
                    "[对话摘要] {}",
                    resp.content.trim()
                )),
            };
            let mut new_messages = vec![summary_msg];
            new_messages.extend_from_slice(&messages[split_idx..]);
            // After compaction, inject a continuation reminder so the LLM doesn't
            // mistake the summary for a completed task and stop prematurely.
            new_messages.push(crate::llm::LlmMessage {
                role: "user".into(),
                content: crate::llm::MessageContent::text(
                    "[系统提示] 以上是对话历史的压缩摘要。请根据摘要中的「当前状态」继续完成尚未完成的任务，不要重复已完成的工作。"
                        .to_string(),
                ),
            });
            Some(new_messages)
        }
        Ok(_) | Err(_) => None,
    }
}

/// Returns true if the error message indicates a context overflow.
fn is_context_overflow_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("context length exceeded")
        || lower.contains("maximum context length")
        || lower.contains("prompt is too long")
        || lower.contains("exceeds model context window")
        || lower.contains("context_window_exceeded")
        || lower.contains("request_too_large")
        || lower.contains("上下文过长")
        || lower.contains("input is too long")
        || lower.contains("reduce the length")
}

/// Returns true if the error indicates the model is permanently unavailable
/// and a fallback model should be tried instead.
/// Note: "overloaded" is intentionally excluded — it is transient and should
/// be retried with exponential backoff on the same model, not switched away from.
fn is_fallback_eligible_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("rate_limit")
        || lower.contains("rate limit")
        || lower.contains("model_not_found")
        || lower.contains("model not found")
        || lower.contains("does not exist")
}

pub struct AgentLoop {
    pub client: Box<dyn LlmClient>,
    pub registry: Arc<ToolRegistry>,
    pub policy: Arc<PolicyGate>,
    pub system_prompt: String,
    pub model: String,
    pub max_tokens: u32,
    /// Input context window size in tokens (0 = auto, derived from max_tokens).
    /// Used for dynamic compaction budget calculation.
    pub context_window: u32,
    /// Fallback models tried in order when the primary model fails with
    /// rate_limit / overloaded / model_not_found errors.
    pub fallback_models: Vec<String>,
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
    /// Receives runtime notifications (e.g. @mention alerts) injected into the
    /// message stream so the agent can react mid-execution.
    pub notification_rx: Option<Mutex<mpsc::Receiver<String>>>,
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
        cancel: &Arc<AtomicBool>,
    ) -> Vec<ContentBlock> {
        let span = tracing::info_span!("tool_exec", tool = %name, session_id = %ctx.session_id);
        info!(parent: &span, "executing tool");
        let trace_id = uuid::Uuid::new_v4().simple().to_string();
        let mut blocks = Vec::new();

        if let Some(wait_reason) = self.check_tool_rate_limit(ctx).await {
            let _ = event_tx
                .send(AgentEvent::ToolEnd {
                    id: id.to_string(),
                    name: name.to_string(),
                    result: wait_reason.clone(),
                    is_error: true,
                })
                .await;
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
                let _ = event_tx
                    .send(AgentEvent::ToolEnd {
                        id: id.to_string(),
                        name: name.to_string(),
                        result: format!("Denied by policy: {}", reason),
                        is_error: true,
                    })
                    .await;
                blocks.push(ContentBlock::ToolResult {
                    tool_use_id: id.to_string(),
                    content: format!("Error: {}", reason),
                    is_error: true,
                });
                return blocks;
            }
            PolicyDecision::Warn(msg) => {
                let tool_wants_confirm = self
                    .registry
                    .get(name)
                    .map(|t| t.needs_confirmation(input))
                    .unwrap_or(false);
                let user_disabled = match name {
                    "shell" | "bash" | "powershell" | "powershell_query" => {
                        !self.confirm_flags.confirm_shell
                    }
                    "file_write" | "file_edit" => !self.confirm_flags.confirm_file_write,
                    _ => false,
                };
                if tool_wants_confirm && !user_disabled {
                    if let (Some(_app), Some(confirms)) =
                        (&self.app_handle, &self.confirmation_responses)
                    {
                        let request_id = uuid::Uuid::new_v4().to_string();
                        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                        {
                            confirms.lock().await.insert(request_id.clone(), resp_tx);
                        }
                        let _ = event_tx
                            .send(AgentEvent::PermissionRequest {
                                request_id,
                                tool_name: name.to_string(),
                                tool_input: input.clone(),
                                description: msg.clone(),
                            })
                            .await;
                        match tokio::time::timeout(std::time::Duration::from_secs(60), resp_rx)
                            .await
                        {
                            Ok(Ok(true)) => {
                                debug!("User approved tool '{}' execution", name);
                            }
                            _ => {
                                warn!("Tool '{}' denied by user or timed out", name);
                                let _ = event_tx
                                    .send(AgentEvent::ToolEnd {
                                        id: id.to_string(),
                                        name: name.to_string(),
                                        result: "Denied by user".into(),
                                        is_error: true,
                                    })
                                    .await;
                                blocks.push(ContentBlock::ToolResult {
                                    tool_use_id: id.to_string(),
                                    content: "User denied this operation".into(),
                                    is_error: true,
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
            obj.insert(
                "_trace_id".into(),
                serde_json::Value::String(trace_id.clone()),
            );
        }
        let _ = event_tx
            .send(AgentEvent::ToolStart {
                id: id.to_string(),
                name: name.to_string(),
                input: input_with_trace,
            })
            .await;

        let result = match self.registry.get(name) {
            Some(tool) => {
                // Log key input fields to aid debugging (path, command, query, etc.)
                let input_hint = match name {
                    "file_read" | "file_write" => input["path"].as_str().unwrap_or("?").to_string(),
                    "shell" => format!(
                        "[{}] {}",
                        input["interpreter"].as_str().unwrap_or("powershell"),
                        input["command"]
                            .as_str()
                            .unwrap_or("?")
                            .chars()
                            .take(100)
                            .collect::<String>()
                    ),
                    "powershell_query" => format!(
                        "query={} arch={}",
                        input["query"].as_str().unwrap_or("?"),
                        input["arch"].as_str().unwrap_or("x64")
                    ),
                    "web_search" => input["query"]
                        .as_str()
                        .unwrap_or("?")
                        .chars()
                        .take(80)
                        .collect(),
                    "browser" => format!(
                        "action={} url={}",
                        input["action"].as_str().unwrap_or("?"),
                        input["url"].as_str().unwrap_or("")
                    ),
                    "com_invoke" => format!(
                        "action={} prog_id={} arch={}",
                        input["action"].as_str().unwrap_or("?"),
                        input["prog_id"].as_str().unwrap_or("?"),
                        input["arch"].as_str().unwrap_or("x64")
                    ),
                    "wmi" => format!(
                        "preset={} query={}",
                        input["preset"].as_str().unwrap_or(""),
                        input["query"]
                            .as_str()
                            .unwrap_or("?")
                            .chars()
                            .take(80)
                            .collect::<String>()
                    ),
                    "uia" => format!(
                        "action={} name={} window={}",
                        input["action"].as_str().unwrap_or("?"),
                        input["name"].as_str().unwrap_or(""),
                        input["window_title"].as_str().unwrap_or("")
                    ),
                    _ => input.to_string().chars().take(100).collect(),
                };
                // Check cancel before starting the tool
                if cancel.load(Ordering::Relaxed) {
                    let _ = event_tx
                        .send(AgentEvent::ToolEnd {
                            id: id.to_string(),
                            name: name.to_string(),
                            result: "已取消".into(),
                            is_error: true,
                        })
                        .await;
                    blocks.push(ContentBlock::ToolResult {
                        tool_use_id: id.to_string(),
                        content: "已取消".into(),
                        is_error: true,
                    });
                    return blocks;
                }

                debug!("Executing tool: {} | input: {}", name, input_hint);
                let cancel_clone = Arc::clone(cancel);
                // Poll cancel flag every 200 ms while the tool runs
                let cancel_watcher = async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        if cancel_clone.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                };
                tokio::select! {
                    biased;
                    res = tokio::time::timeout(
                        std::time::Duration::from_secs(TOOL_TIMEOUT_SECS),
                        tool.call(input.clone(), ctx),
                    ) => {
                        match res {
                            Ok(Ok(r)) => r,
                            Ok(Err(e)) => {
                                let err_msg = e.to_string();
                                warn!("Tool '{}' error: {} | input: {}", name, err_msg, input_hint);
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
                    _ = cancel_watcher => {
                        warn!("Tool '{}' interrupted by user cancel", name);
                        super::tool::ToolResult::err("已被用户取消".to_string())
                    }
                }
            }
            None => {
                warn!("Tool '{}' not found in registry", name);
                let available: Vec<String> = self
                    .registry
                    .all()
                    .iter()
                    .map(|t| t.name().to_string())
                    .collect();
                super::tool::ToolResult::err(format!(
                    "工具 '{}' 未找到。当前可用工具：{}。请检查工具名称是否正确，或在设置中启用该工具。",
                    name,
                    available.join(", ")
                ))
            }
        };

        let end_result = format!("[trace_id:{}] {}", trace_id, result.content);
        let _ = event_tx
            .send(AgentEvent::ToolEnd {
                id: id.to_string(),
                name: name.to_string(),
                result: end_result,
                is_error: result.is_error,
            })
            .await;

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
                let _ = db.append_audit(
                    &session_id_clone,
                    &tool_name_clone,
                    &action,
                    input_summary.as_deref(),
                    result_summary.as_deref(),
                    is_err,
                );
            });
        }

        let mut guarded_content = guard_tool_result_content(
            &result.content,
            dynamic_result_limit(self.max_tokens as usize * 4),
        );
        if let Some(img) = result.image.as_ref() {
            let artifact = vision::store_tool_image(&ctx.session_id, name, None, img).await;
            guarded_content.push_str(&format!(
                "\n\n[vision_artifact] id={} label=\"{}\" media_type={}\nUse vision_context to list/select reusable images for a later reasoning step.",
                artifact.id, artifact.label, artifact.media_type
            ));
        }
        blocks.push(ContentBlock::ToolResult {
            tool_use_id: id.to_string(),
            content: guarded_content,
            is_error: result.is_error,
        });
        if let Some(img) = result.image {
            blocks.push(ContentBlock::Image {
                source: ImageSource {
                    source_type: "base64".into(),
                    media_type: img.media_type,
                    data: img.base64,
                },
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
        let span =
            tracing::info_span!("agent_loop", session_id = %ctx.session_id, model = %self.model);
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
                    info!(
                        "Resuming from checkpoint at iteration {} for session {}",
                        iter, ctx.session_id
                    );
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
        let mut loop_detector = LoopDetectorState::new();

        for _iteration in 0..max_iterations {
            if cancel.load(Ordering::Relaxed) {
                break;
            }

            // Drain pending notifications (e.g. @mention alerts from other Koi)
            if let Some(ref rx_mutex) = self.notification_rx {
                let mut rx = rx_mutex.lock().await;
                while let Ok(msg) = rx.try_recv() {
                    let preview = if msg.chars().count() > 80 {
                        format!("{}...", msg.chars().take(80).collect::<String>())
                    } else {
                        msg.clone()
                    };
                    info!("Injecting notification into agent loop: {}", preview);
                    messages.push(LlmMessage {
                        role: "user".into(),
                        content: MessageContent::text(&msg),
                    });
                }
            }

            // Dynamic compaction (Level-1): trim oversized individual tool results.
            // single_limit is in chars (not tokens): budget tokens × share × ~4 chars/token.
            // Bug fix: previously multiplied by 4 again, making the limit 4× too large.
            {
                let budget =
                    crate::llm::compute_context_budget(self.context_window, self.max_tokens);
                // single_limit in chars: budget_tokens × share × 4 chars/token
                let single_limit = (budget as f64 * CONTEXT_SINGLE_RESULT_SHARE * 4.0) as usize;
                compact_trim_tool_results(&mut messages, single_limit);

                // Dynamic compaction (Level-2 proactive): if estimated token count exceeds
                // 80% of the budget, summarise old messages now — before the LLM call —
                // rather than waiting for a context overflow error (which some models never
                // emit, instead silently truncating or producing near-empty responses).
                let estimated: usize = messages
                    .iter()
                    .map(crate::llm::estimate_message_tokens)
                    .sum();
                if estimated > (budget as f64 * 0.80) as usize {
                    let keep_chars =
                        (budget as f64 * SUMMARY_KEEP_RECENT_RATIO * 4.0) as usize;
                    warn!(
                        "proactive compaction: estimated_tokens={} > 80% of budget={}, keep_chars={}",
                        estimated, budget, keep_chars
                    );
                    if let Some(compacted) = compact_summarise(
                        messages.clone(),
                        keep_chars,
                        self.client.as_ref(),
                        &self.model,
                        self.max_tokens,
                    )
                    .await
                    {
                        info!(
                            "proactive summarisation complete: {} → {} messages",
                            messages.len(),
                            compacted.len()
                        );
                        messages = compacted;
                    }
                }
            }

            info!(
                "agent loop iteration={} messages={}",
                _iteration,
                messages.len()
            );

            // Signal frontend that a new LLM call is starting — it should replace the
            // current streaming bubble with a fresh one (slide old out, slide new in).
            let _ = event_tx
                .send(AgentEvent::TextSegmentStart {
                    iteration: _iteration as u32 + 1,
                })
                .await;

            // Call LLM with exponential-backoff retry for transient failures,
            // model fallback for rate_limit/model_not_found errors,
            // and level-2 LLM summarisation for context overflow errors.
            //
            // req_messages is rebuilt inside the loop so that after compact_summarise
            // updates `messages`, the next attempt uses the compacted context.
            info!("calling LLM: model={}", self.model);
            let response = {
                let models_to_try: Vec<String> = std::iter::once(self.model.clone())
                    .chain(self.fallback_models.iter().cloned())
                    .collect();
                let mut last_err: Option<anyhow::Error> = None;
                let mut resp: Option<crate::llm::LlmResponse> = None;
                let mut context_overflow_attempted = false;

                'model_loop: for model_candidate in &models_to_try {
                    // Build req_messages inside the model loop so that after
                    // compact_summarise updates `messages`, we use the fresh context.
                    let req_messages =
                        vision::inject_selected_context(&messages, &ctx.session_id).await;
                    let req = LlmRequest {
                        messages: req_messages,
                        system: Some(self.system_prompt.clone()),
                        tools: self.registry.to_tool_defs(),
                        model: model_candidate.clone(),
                        max_tokens: self.max_tokens,
                        stream: true,
                        vision_override: self.vision_override,
                    };

                    for attempt in 0..LLM_MAX_RETRIES {
                        match self.client.complete(req.clone()).await {
                            Ok(r) => {
                                resp = Some(r);
                                break 'model_loop;
                            }
                            Err(e) => {
                                let msg = e.to_string();
                                warn!(
                                    "LLM call attempt {}/{} model={} failed: {}",
                                    attempt + 1,
                                    LLM_MAX_RETRIES,
                                    model_candidate,
                                    msg
                                );

                                if is_context_overflow_error(&msg) && !context_overflow_attempted {
                                    context_overflow_attempted = true;
                                    let budget = crate::llm::compute_context_budget(
                                        self.context_window,
                                        self.max_tokens,
                                    );
                                    // keep_chars: newest 60% of budget in chars
                                    let keep_chars =
                                        (budget as f64 * SUMMARY_KEEP_RECENT_RATIO * 4.0) as usize;
                                    warn!("context overflow — attempting LLM summarisation (keep_chars={})", keep_chars);
                                    let compacted = compact_summarise(
                                        messages.clone(),
                                        keep_chars,
                                        self.client.as_ref(),
                                        model_candidate,
                                        self.max_tokens,
                                    )
                                    .await;
                                    if let Some(c) = compacted {
                                        messages = c;
                                        info!(
                                            "summarisation complete, messages={}",
                                            messages.len()
                                        );
                                        // Restart model_loop with the compacted context.
                                        last_err = Some(e);
                                        continue 'model_loop;
                                    } else {
                                        // Summarisation failed — cannot recover from overflow.
                                        warn!("summarisation failed, cannot recover from context overflow");
                                        last_err = Some(e);
                                        break 'model_loop;
                                    }
                                } else if is_fallback_eligible_error(&msg) {
                                    // rate_limit / model_not_found: try next fallback model.
                                    // overloaded is intentionally excluded — it should be
                                    // retried with backoff on the same model.
                                    last_err = Some(e);
                                    break;
                                } else {
                                    let is_transient = msg.contains("timeout")
                                        || msg.contains("connection")
                                        || msg.contains("overloaded")
                                        || msg.contains("502")
                                        || msg.contains("503")
                                        || msg.contains("529");
                                    if !is_transient || attempt + 1 == LLM_MAX_RETRIES {
                                        last_err = Some(e);
                                        break 'model_loop;
                                    }
                                    let backoff = std::time::Duration::from_secs(1 << attempt);
                                    tokio::time::sleep(backoff).await;
                                    last_err = Some(e);
                                }
                            }
                        }
                    }
                }
                match resp {
                    Some(r) => r,
                    None => {
                        return Err(last_err.unwrap_or_else(|| anyhow::anyhow!("LLM call failed")))
                    }
                }
            };
            info!(
                "LLM response: input_tokens={} output_tokens={} tool_calls={} text_len={}",
                response.input_tokens,
                response.output_tokens,
                response.tool_calls.len(),
                response.content.len()
            );
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
                let _ = event_tx
                    .send(AgentEvent::TextDelta {
                        delta: text_buf.clone(),
                    })
                    .await;
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

            // ── Per-tool loop detection (before execution) ──────────────────
            // Check each tool call against the sliding window history.
            // Critical = block the tool call; Warning = inject hint but continue.
            let mut blocked_tool_ids: Vec<String> = Vec::new();
            let mut warning_messages: Vec<String> = Vec::new();
            for (id, name, input) in &tool_calls {
                let detection = loop_detector.detect(name, input);
                match detection.level {
                    LoopLevel::Critical => {
                        warn!(
                            "Loop CRITICAL [{}]: tool='{}' count={} detector={:?}",
                            ctx.session_id, name, detection.count, detection.detector
                        );
                        blocked_tool_ids.push(id.clone());
                        warning_messages.push(detection.message);
                    }
                    LoopLevel::Warning => {
                        warn!(
                            "Loop WARNING [{}]: tool='{}' count={} detector={:?}",
                            ctx.session_id, name, detection.count, detection.detector
                        );
                        warning_messages.push(detection.message);
                    }
                    LoopLevel::Ok => {}
                }
            }

            // If ALL tool calls in this iteration are blocked, break the loop.
            if !blocked_tool_ids.is_empty() && blocked_tool_ids.len() == tool_calls.len() {
                let combined_msg = warning_messages.join("\n");
                let _ = event_tx
                    .send(AgentEvent::TextDelta {
                        delta: format!("\n\n[系统] {}\n", combined_msg),
                    })
                    .await;
                messages.push(LlmMessage {
                    role: "assistant".into(),
                    content: MessageContent::text(&text_buf),
                });
                break;
            }

            // Build assistant message with tool calls
            let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
            if !text_buf.is_empty() {
                assistant_blocks.push(ContentBlock::Text {
                    text: text_buf.clone(),
                });
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

            // Execute tools — read-only concurrently, write serially.
            // Blocked tools (by loop detector) get a synthetic error result instead.
            let mut tool_result_blocks: Vec<ContentBlock> = Vec::new();

            if cancel.load(Ordering::Relaxed) {
                break;
            }

            // Separate blocked, read-only, and write calls
            let active_calls: Vec<_> = tool_calls
                .iter()
                .filter(|(id, _, _)| !blocked_tool_ids.contains(id))
                .cloned()
                .collect();
            let read_only_calls: Vec<_> = active_calls
                .iter()
                .filter(|(_, name, _)| {
                    self.registry
                        .get(name)
                        .map(|t| t.is_read_only())
                        .unwrap_or(false)
                })
                .cloned()
                .collect();
            let write_calls: Vec<_> = active_calls
                .iter()
                .filter(|(_, name, _)| {
                    !self
                        .registry
                        .get(name)
                        .map(|t| t.is_read_only())
                        .unwrap_or(false)
                })
                .cloned()
                .collect();

            // Inject synthetic error results for blocked tools
            for (id, name, _) in &tool_calls {
                if blocked_tool_ids.contains(id) {
                    let msg = warning_messages
                        .iter()
                        .find(|m| m.contains(name.as_str()))
                        .cloned()
                        .unwrap_or_else(|| format!("工具 '{}' 被循环检测器阻断。", name));
                    tool_result_blocks.push(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: format!("[循环检测] {}", msg),
                        is_error: true,
                    });
                    let _ = event_tx
                        .send(AgentEvent::ToolEnd {
                            id: id.clone(),
                            name: name.clone(),
                            result: format!("[循环检测] {}", msg),
                            is_error: true,
                        })
                        .await;
                }
            }

            // Execute read-only tools concurrently
            if !read_only_calls.is_empty() {
                let mut start = 0usize;
                while start < read_only_calls.len() {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
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
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let blocks = self
                    .execute_single_tool(id, name, input, &ctx, &event_tx, &cancel)
                    .await;
                tool_result_blocks.extend(blocks);
            }

            // ── Record results into loop detector + inject warnings ──────────
            for block in &tool_result_blocks {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } = block
                {
                    if let Some((_, name, input)) =
                        tool_calls.iter().find(|(id, _, _)| id == tool_use_id)
                    {
                        let rh = stable_hash_result(content);
                        loop_detector.record(name, input, rh);
                    }
                }
            }

            // Inject any warning messages (non-blocking) into the tool results
            if !warning_messages.is_empty() && blocked_tool_ids.is_empty() {
                let combined = warning_messages.join("\n");
                tool_result_blocks.push(ContentBlock::ToolResult {
                    tool_use_id: "system_loop_warning".to_string(),
                    content: format!("[循环检测警告] {}", combined),
                    is_error: true,
                });
            }

            // Add tool results as user message
            messages.push(LlmMessage {
                role: "user".into(),
                content: MessageContent::Blocks(tool_result_blocks),
            });

            // Write checkpoint after each iteration (with size guard)
            if let Some(ref db_arc) = self.db {
                let db = db_arc.lock().await;
                match serde_json::to_string(&messages) {
                    Ok(json) => {
                        if json.len() > CHECKPOINT_MAX_BYTES {
                            warn!(
                                "Checkpoint too large ({} bytes > {} limit), skipping write",
                                json.len(),
                                CHECKPOINT_MAX_BYTES
                            );
                        } else if let Err(e) =
                            db.upsert_checkpoint(&ctx.session_id, _iteration, &json)
                        {
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
    if raw_lower.contains("no such file")
        || raw_lower.contains("not found")
        || raw_lower.contains("cannot find")
    {
        return format!(
            "[{}] 文件或路径不存在。请确认路径正确，或先用 file_write 创建文件。\n详情：{}",
            tool_name, raw_error
        );
    }
    if raw_lower.contains("permission denied")
        || raw_lower.contains("access is denied")
        || raw_lower.contains("拒绝访问")
        || raw_lower.contains("0x80070005")
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
    if raw_lower.contains("dns") || raw_lower.contains("resolve") || raw_lower.contains("no route")
    {
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
        if raw_lower.contains("chrome")
            || raw_lower.contains("browser")
            || raw_lower.contains("cdp")
        {
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
    if (tool_name == "wmi" || tool_name == "com")
        && (raw_lower.contains("wmi")
            || raw_lower.contains("com")
            || raw_lower.contains("dispatch"))
    {
        return format!(
            "[{}] Windows 系统接口调用失败。请确认以管理员权限运行，或该功能在当前系统版本可用。\n详情：{}",
            tool_name, raw_error
        );
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
            let cmd = input["command"]
                .as_str()
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
        "screen_capture" => input["mode"].as_str().unwrap_or("fullscreen").to_string(),
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

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::{compact_summarise, compact_trim_tool_results, CTX_TRIM_HEAD, CTX_TRIM_TAIL};
    use crate::llm::{
        ContentBlock, LlmChunk, LlmMessage, LlmRequest, LlmResponse, MessageContent,
    };
    use anyhow::Result;
    use async_trait::async_trait;

    // ── Mock LLM clients ──────────────────────────────────────────────────────

    /// Returns a fixed summary string — simulates a successful LLM summarisation call.
    struct MockLlmClient {
        response: String,
    }

    impl MockLlmClient {
        fn new(response: impl Into<String>) -> Self {
            Self { response: response.into() }
        }
    }

    #[async_trait]
    impl crate::llm::LlmClient for MockLlmClient {
        async fn stream(
            &self,
            _req: LlmRequest,
            _tx: tokio::sync::mpsc::Sender<LlmChunk>,
        ) -> Result<()> {
            Ok(())
        }

        async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse> {
            Ok(LlmResponse {
                content: self.response.clone(),
                tool_calls: vec![],
                input_tokens: 10,
                output_tokens: 10,
            })
        }
    }

    /// Always returns an error — simulates a failed LLM call.
    struct FailingLlmClient;

    #[async_trait]
    impl crate::llm::LlmClient for FailingLlmClient {
        async fn stream(
            &self,
            _req: LlmRequest,
            _tx: tokio::sync::mpsc::Sender<LlmChunk>,
        ) -> Result<()> {
            Ok(())
        }

        async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse> {
            Err(anyhow::anyhow!("simulated LLM failure"))
        }
    }

    // ── Test data helpers ─────────────────────────────────────────────────────

    fn make_text_msg(role: &str, text: &str) -> LlmMessage {
        LlmMessage {
            role: role.to_string(),
            content: MessageContent::Text(text.to_string()),
        }
    }

    /// Assistant message with a ToolUse block (mirrors real DB tool_calls_json).
    fn make_tool_call_msg(tool_name: &str, input_json: &str) -> LlmMessage {
        LlmMessage {
            role: "assistant".to_string(),
            content: MessageContent::Blocks(vec![
                ContentBlock::Text {
                    text: format!("正在调用 {}...", tool_name),
                },
                ContentBlock::ToolUse {
                    id: format!("call_{}", tool_name),
                    name: tool_name.to_string(),
                    input: serde_json::from_str(input_json)
                        .unwrap_or(serde_json::Value::Null),
                },
            ]),
        }
    }

    /// User message with a ToolResult block (mirrors real DB tool_results_json).
    fn make_tool_result_msg(tool_use_id: &str, content: &str) -> LlmMessage {
        LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: content.to_string(),
                is_error: false,
            }]),
        }
    }

    /// User message with a large ToolResult of exactly `size_chars` characters.
    fn make_large_tool_result(size_chars: usize) -> LlmMessage {
        let content = "x".repeat(size_chars);
        make_tool_result_msg("call_large", &content)
    }

    /// Simulate a realistic agent session with `n_tool_rounds` tool call rounds.
    /// Structure mirrors what the real AgentLoop produces in memory:
    ///   user text → [assistant(ToolUse) → user(ToolResult)] × n_rounds → assistant text
    fn make_realistic_session(n_tool_rounds: usize) -> Vec<LlmMessage> {
        let mut msgs = vec![make_text_msg(
            "user",
            "请帮我在Tribon中移动配件位置，将管道支撑从坐标(100,200,300)移动到(150,250,350)",
        )];
        for i in 0..n_tool_rounds {
            // assistant calls a tool (shell, file_read, com_invoke, etc.)
            let tool_names = ["shell", "file_read", "com_invoke", "plan_todo", "file_write"];
            let tool_name = tool_names[i % tool_names.len()];
            let input = match tool_name {
                "shell" => format!(
                    r#"{{"command":"python tribon_move.py --id {} --x 150 --y 250 --z 350"}}"#,
                    i
                ),
                "file_read" => format!(r#"{{"path":"C:\\Tribon\\project\\part_{}.xml"}}"#, i),
                "com_invoke" => format!(
                    r#"{{"prog_id":"Tribon.Application","method":"MoveComponent","args":[{},150,250,350]}}"#,
                    i
                ),
                "plan_todo" => format!(
                    r#"{{"merge":true,"todos":[{{"id":"step-{}","content":"移动配件{}","status":"completed"}}]}}"#,
                    i, i
                ),
                _ => format!(r#"{{"path":"C:\\output\\result_{}.txt","content":"done"}}"#, i),
            };
            msgs.push(make_tool_call_msg(tool_name, &input));

            // tool result (realistic size: 200-2000 chars)
            let result_size = 200 + (i * 37) % 1800;
            let result_content = format!(
                "工具 {} 执行结果 (迭代 {}):\n{}\n退出码: 0",
                tool_name,
                i,
                "a".repeat(result_size)
            );
            msgs.push(make_tool_result_msg(&format!("call_{}", tool_name), &result_content));
        }
        msgs.push(make_text_msg(
            "assistant",
            "配件移动完成。已将管道支撑从(100,200,300)成功移动到(150,250,350)。",
        ));
        msgs
    }

    // ── T1: Level-1 — small result not trimmed ────────────────────────────────

    #[test]
    fn t1_small_tool_result_not_trimmed() {
        let original = "x".repeat(1_000);
        let mut msgs = vec![make_tool_result_msg("call_1", &original)];
        let changed = compact_trim_tool_results(&mut msgs, 50_000);
        assert!(!changed, "should not trim a 1000-char result with limit=50000");
        if let MessageContent::Blocks(ref blocks) = msgs[0].content {
            if let ContentBlock::ToolResult { content, .. } = &blocks[0] {
                assert_eq!(*content, original, "content should be unchanged");
            }
        }
    }

    // ── T2: Level-1 — oversized result trimmed ────────────────────────────────

    #[test]
    fn t2_large_tool_result_trimmed() {
        let mut msgs = vec![make_large_tool_result(100_000)];
        let changed = compact_trim_tool_results(&mut msgs, 10_000);
        assert!(changed, "should trim a 100000-char result with limit=10000");
        if let MessageContent::Blocks(ref blocks) = msgs[0].content {
            if let ContentBlock::ToolResult { content, .. } = &blocks[0] {
                assert!(
                    content.contains("chars removed"),
                    "trimmed content should contain 'chars removed' marker"
                );
                // Verify head and tail are preserved
                let head_check: String = "x".repeat(CTX_TRIM_HEAD);
                assert!(content.starts_with(&head_check), "head should be preserved");
                let tail_check: String = "x".repeat(CTX_TRIM_TAIL);
                assert!(content.ends_with(&tail_check), "tail should be preserved");
            }
        }
    }

    // ── T3: Level-1 — assistant messages not trimmed ─────────────────────────

    #[test]
    fn t3_assistant_tool_use_not_trimmed() {
        // assistant ToolUse messages should never be touched by Level-1
        let large_input = serde_json::json!({"command": "x".repeat(50_000)});
        let mut msgs = vec![LlmMessage {
            role: "assistant".to_string(),
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: "call_big".to_string(),
                name: "shell".to_string(),
                input: large_input.clone(),
            }]),
        }];
        let changed = compact_trim_tool_results(&mut msgs, 1_000);
        assert!(!changed, "assistant ToolUse should never be trimmed by Level-1");
        if let MessageContent::Blocks(ref blocks) = msgs[0].content {
            if let ContentBlock::ToolUse { input, .. } = &blocks[0] {
                assert_eq!(*input, large_input, "ToolUse input should be unchanged");
            }
        }
    }

    // ── T4: Level-1 — mixed messages, only oversized user results trimmed ─────

    #[test]
    fn t4_mixed_messages_only_oversized_trimmed() {
        let limit = 5_000;
        let threshold = limit.max(CTX_TRIM_HEAD + CTX_TRIM_TAIL + 100);

        let small = make_tool_result_msg("c1", &"a".repeat(500));
        let medium = make_tool_result_msg("c2", &"b".repeat(threshold - 1)); // just under threshold
        let large = make_large_tool_result(threshold + 10_000); // well over threshold
        let assistant = make_tool_call_msg("shell", r#"{"command":"ls"}"#);

        let mut msgs = vec![small, medium, large, assistant];
        let original_small = "a".repeat(500);
        let original_medium = "b".repeat(threshold - 1);

        let changed = compact_trim_tool_results(&mut msgs, limit);
        assert!(changed, "should report change because large result was trimmed");

        // small: unchanged
        if let MessageContent::Blocks(ref b) = msgs[0].content {
            if let ContentBlock::ToolResult { content, .. } = &b[0] {
                assert_eq!(*content, original_small);
            }
        }
        // medium: unchanged (just under threshold)
        if let MessageContent::Blocks(ref b) = msgs[1].content {
            if let ContentBlock::ToolResult { content, .. } = &b[0] {
                assert_eq!(*content, original_medium);
            }
        }
        // large: trimmed
        if let MessageContent::Blocks(ref b) = msgs[2].content {
            if let ContentBlock::ToolResult { content, .. } = &b[0] {
                assert!(content.contains("chars removed"), "large result should be trimmed");
            }
        }
        // assistant: unchanged
        assert_eq!(msgs[3].role, "assistant");
    }

    // ── T5: Level-2 — too few messages returns None ───────────────────────────

    #[tokio::test]
    async fn t5_too_few_messages_returns_none() {
        let client = MockLlmClient::new("摘要内容");
        let msgs = vec![make_text_msg("user", "只有一条消息")];
        let result = compact_summarise(msgs, 100_000, &client, "test-model", 1024).await;
        assert!(result.is_none(), "single message should return None");
    }

    // ── T6: Level-2 — all messages fit in keep_chars, returns None ────────────

    #[tokio::test]
    async fn t6_all_fit_in_budget_returns_none() {
        let client = MockLlmClient::new("摘要内容");
        let msgs = vec![
            make_text_msg("user", "短消息1"),
            make_text_msg("assistant", "短回复1"),
            make_text_msg("user", "短消息2"),
        ];
        // keep_chars=100000 >> total size of 3 short messages
        let result = compact_summarise(msgs, 100_000, &client, "test-model", 1024).await;
        assert!(result.is_none(), "all messages fit in budget, should return None");
    }

    // ── T7: Level-2 — plain text messages compacted correctly ────────────────

    #[tokio::test]
    async fn t7_plain_text_messages_compacted() {
        let client = MockLlmClient::new("用户要求[移动配件]，智能体已完成[查询位置]，当前状态[待执行移动]");
        // 20 messages × ~500 chars each ≈ 10000 chars total
        let mut msgs: Vec<LlmMessage> = (0..20)
            .map(|i| {
                let role = if i % 2 == 0 { "user" } else { "assistant" };
                make_text_msg(role, &format!("消息内容 {}: {}", i, "x".repeat(500)))
            })
            .collect();
        // Ensure alternating roles start with user
        msgs[0] = make_text_msg("user", &format!("用户请求: {}", "x".repeat(500)));

        // keep_chars=2000 forces compaction of older messages
        let result = compact_summarise(msgs.clone(), 2_000, &client, "test-model", 1024).await;
        assert!(result.is_some(), "should compact when messages exceed keep_chars");

        let compacted = result.unwrap();
        assert!(
            compacted.len() < msgs.len(),
            "compacted messages ({}) should be fewer than original ({})",
            compacted.len(),
            msgs.len()
        );

        // First message should be the summary
        let first_content = compacted[0].content.as_text();
        assert!(
            first_content.contains("[对话摘要]"),
            "first message should contain [对话摘要], got: {}",
            &first_content[..first_content.len().min(100)]
        );

        // Last message should be the continuation reminder
        let last_content = compacted.last().unwrap().content.as_text();
        assert!(
            last_content.contains("[系统提示]"),
            "last message should contain [系统提示]"
        );
    }

    // ── T8: Level-2 — realistic session with tool calls compacted ────────────

    #[tokio::test]
    async fn t8_realistic_tool_call_session_compacted() {
        let summary_text =
            "用户要求[移动Tribon配件]，智能体已完成[调用shell脚本、读取XML文件、调用COM接口]，当前状态[验证移动结果]";
        let client = MockLlmClient::new(summary_text);

        // 30 rounds of tool calls — mirrors the real crash scenario
        let msgs = make_realistic_session(30);
        let original_len = msgs.len();

        // keep_chars=5000 forces compaction of the bulk of the history
        let result = compact_summarise(msgs, 5_000, &client, "deepseek-chat", 4096).await;
        assert!(result.is_some(), "30-round session should be compacted");

        let compacted = result.unwrap();
        assert!(
            compacted.len() < original_len,
            "compacted ({}) should be fewer than original ({})",
            compacted.len(),
            original_len
        );

        // Summary message should contain tool names (not empty)
        let summary_msg = &compacted[0];
        let summary_content = summary_msg.content.as_text();
        assert!(
            summary_content.contains("[对话摘要]"),
            "first message should be summary"
        );
        assert!(
            !summary_content.trim().is_empty(),
            "summary content should not be empty"
        );

        // The history_text sent to LLM should have contained tool call info.
        // We verify this indirectly: the mock returns our fixed summary, which
        // means compact_summarise successfully called the LLM (didn't short-circuit
        // due to empty history_text).
        assert!(
            summary_content.contains(summary_text),
            "summary should contain the mock LLM response"
        );
    }

    // ── T9: Level-2 — LLM failure returns None ───────────────────────────────

    #[tokio::test]
    async fn t9_llm_failure_returns_none() {
        let client = FailingLlmClient;
        let msgs = make_realistic_session(20);
        let result = compact_summarise(msgs, 1_000, &client, "test-model", 1024).await;
        assert!(result.is_none(), "LLM failure should return None from compact_summarise");
    }

    // ── T10: estimate_message_tokens handles all content types ───────────────

    #[test]
    fn t10_estimate_message_tokens_all_types() {
        use crate::llm::estimate_message_tokens;

        // Plain text
        let text_msg = make_text_msg("user", &"a".repeat(400));
        let text_tokens = estimate_message_tokens(&text_msg);
        assert!(text_tokens > 0, "plain text should have non-zero token estimate");
        // 400 ASCII chars ÷ 4 = 100 tokens (max(1) applies)
        assert!(
            text_tokens >= 90 && text_tokens <= 110,
            "400 ASCII chars should estimate ~100 tokens, got {}",
            text_tokens
        );

        // ToolUse (assistant) — previously returned 0 with as_text()
        let tool_call = make_tool_call_msg("shell", r#"{"command":"python move.py --x 150"}"#);
        let tool_call_tokens = estimate_message_tokens(&tool_call);
        assert!(
            tool_call_tokens > 0,
            "ToolUse message should have non-zero token estimate, got {}",
            tool_call_tokens
        );

        // ToolResult (user) — previously returned 0 with as_text()
        let tool_result = make_tool_result_msg("call_1", &"result content ".repeat(50));
        let tool_result_tokens = estimate_message_tokens(&tool_result);
        assert!(
            tool_result_tokens > 0,
            "ToolResult message should have non-zero token estimate, got {}",
            tool_result_tokens
        );

        // Large ToolResult should estimate more tokens than small one
        let small_result = make_tool_result_msg("c1", "short");
        let large_result = make_large_tool_result(10_000);
        assert!(
            estimate_message_tokens(&large_result) > estimate_message_tokens(&small_result),
            "larger tool result should estimate more tokens"
        );
    }

    // ── T11: 154-round crash scenario — split_idx keeps enough tail messages ──

    #[tokio::test]
    async fn t11_154_round_crash_scenario_split_idx() {
        let client = MockLlmClient::new(
            "用户要求[监控路径合规性]，智能体已完成[检查前端和后端代码]，当前状态[待完成报告生成]",
        );

        // Reproduce the exact crash scenario from the logs:
        // deepseek-chat, max_tokens=4096 → budget=49000, keep_chars=117600
        let msgs = make_realistic_session(76); // 76 rounds ≈ 154 messages
        let original_len = msgs.len();

        // keep_chars matching the real crash: budget(49000) × 0.60 × 4 = 117600
        let keep_chars = 117_600usize;
        let result = compact_summarise(msgs, keep_chars, &client, "deepseek-chat", 4096).await;

        assert!(result.is_some(), "154-message session should be compacted");
        let compacted = result.unwrap();

        // Key regression check: must keep more than just 2 tail messages.
        // Before the fix, split_idx defaulted to len-2, leaving only 3 messages total.
        // After the fix, split_idx is computed from actual content sizes.
        let tail_count = compacted.len() - 1; // subtract the summary message
        assert!(
            tail_count >= 6,
            "should keep at least 6 tail messages (3 tool rounds), got {} tail + 1 summary = {} total (original={})",
            tail_count,
            compacted.len(),
            original_len
        );

        // Summary should be first, continuation reminder last
        assert!(
            compacted[0].content.as_text().contains("[对话摘要]"),
            "first message should be summary"
        );
        assert!(
            compacted.last().unwrap().content.as_text().contains("[系统提示]"),
            "last message should be continuation reminder"
        );
    }
}
