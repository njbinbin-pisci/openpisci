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
use crate::llm::{ContentBlock, ImageSource, LlmClient, LlmMessage, MessageContent};
use crate::policy::{PolicyDecision, PolicyGate};
use crate::store::Database;
use anyhow::Result;
use futures::future::join_all;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
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
const RESEARCH_WARNING_THRESHOLD: usize = 4;
const RESEARCH_CRITICAL_THRESHOLD: usize = 6;
const RESEARCH_RECENT_WINDOW: usize = 10;
const PING_PONG_WARNING: usize = 8;
const PING_PONG_CRITICAL: usize = 16;
const TOOL_RESULT_HARD_MAX_CHARS: usize = 48_000;
const CONTEXT_SINGLE_RESULT_SHARE: f64 = 0.5;
const CHECKPOINT_MAX_BYTES: usize = 8_000_000;

/// Tools that are known polling/status-checking tools. These get stricter
/// no-progress detection (inspired by OpenClaw's known_poll_no_progress).
const KNOWN_POLL_TOOLS: &[&str] = &["process_control", "shell", "powershell_query"];
const KNOWLEDGE_GATHERING_TOOLS: &[&str] = &["web_search", "browser"];

static TOOL_RATE_STATE: Lazy<Mutex<HashMap<String, Vec<std::time::Instant>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// User-controlled confirmation flags from Settings.
#[derive(Debug, Clone)]
pub struct ConfirmFlags {
    pub confirm_shell: bool,
    pub confirm_file_write: bool,
}

pub type ConfirmationResponseMap = Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>>;

/// Shared plan-state map (session_id → plan todo items).
///
/// The agent loop reads this when the model wants to exit to detect
/// unfinished todos and inject a reminder. Desktop hosts wire up their
/// `AppState.plan_state` here; headless hosts typically leave `AgentLoop::
/// plan_state` as `None`.
pub type PlanStateHandle = Arc<Mutex<HashMap<String, Vec<crate::agent::plan::PlanTodoItem>>>>;

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
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ToolCallRecord {
    name: String,
    input_hash: u64,
    result_hash: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AgentCheckpointPayload {
    base_context_hash: u64,
    base_message_count: usize,
    messages: Vec<LlmMessage>,
    loop_history: Vec<ToolCallRecord>,
    seen_notifications: Vec<String>,
}

/// Per-session tool call history for loop detection.
/// Maintains a sliding window of recent tool calls (like OpenClaw's toolCallHistory).
struct LoopDetectorState {
    history: Vec<ToolCallRecord>,
}

impl LoopDetectorState {
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

        // 2.5. Research tools: allow query refinement, but stop endless "one more search"
        let is_research = KNOWLEDGE_GATHERING_TOOLS
            .iter()
            .any(|t| pending_name.contains(t));
        if is_research {
            // Count only calls with the same tool+input — different operations
            // like browser launch/navigate/type/press are not repetitions.
            let same_input_count =
                self.count_recent_tool_family_same_input(pending_name, pending_hash, RESEARCH_RECENT_WINDOW) + 1;
            if same_input_count >= RESEARCH_CRITICAL_THRESHOLD {
                return LoopDetectionResult {
                    level: LoopLevel::Critical,
                    detector: Some(LoopDetector::GenericRepeat),
                    count: same_input_count,
                    message: format!(
                        "调研工具 '{}' 在最近步骤中已累计调用{}次。请停止继续搜集，先基于现有证据总结结论、明确不确定性，再决定是否还需要补充一轮查询。",
                        pending_name, same_input_count
                    ),
                };
            }
            if same_input_count >= RESEARCH_WARNING_THRESHOLD {
                return LoopDetectionResult {
                    level: LoopLevel::Warning,
                    detector: Some(LoopDetector::GenericRepeat),
                    count: same_input_count,
                    message: format!(
                        "调研工具 '{}' 在最近步骤中已累计调用{}次。请优先收束：总结已有发现、列出分歧点，只在确有信息缺口时再补充搜索。",
                        pending_name, same_input_count
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

    /// Count recent occurrences in the same tool family **with the same input hash**.
    /// Different tool operations (e.g. browser launch vs navigate vs type) count
    /// separately because they have different inputs — only truly identical calls
    /// are flagged as research repetition.
    fn count_recent_tool_family_same_input(
        &self,
        name: &str,
        input_hash: u64,
        window: usize,
    ) -> usize {
        self.history
            .iter()
            .rev()
            .take(window)
            .filter(|r| same_tool_family(&r.name, name) && r.input_hash == input_hash)
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

fn stable_hash_messages(messages: &[LlmMessage]) -> u64 {
    let mut hasher = DefaultHasher::new();
    serde_json::to_string(messages)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}

fn same_tool_family(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let a_research = KNOWLEDGE_GATHERING_TOOLS.iter().any(|t| a.contains(t));
    let b_research = KNOWLEDGE_GATHERING_TOOLS.iter().any(|t| b.contains(t));
    a_research && b_research
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

pub use super::compaction::{
    CTX_COMPACT_AFTER, CTX_FULL_TURNS, CTX_KEEP_RECENT_TOOL_CARRIERS, CTX_PRESERVE_RECENT_TURNS,
    CTX_TRIM_HEAD, CTX_TRIM_TAIL,
};
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
pub fn compact_trim_tool_results(messages: &mut [LlmMessage], single_limit: usize) -> bool {
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

pub struct CompactionOutcome {
    pub messages: Vec<LlmMessage>,
    pub summary: String,
    /// Prompt tokens billed for the summarisation call. Accumulated into the
    /// session's cumulative_input_tokens so ring indicators reflect reality.
    pub input_tokens: u32,
    /// Completion tokens billed for the summarisation call. Accumulated into
    /// cumulative_output_tokens.
    pub output_tokens: u32,
    /// p7: structured fields extracted from the summariser output (empty
    /// when the model falls back to plain prose). These feed the p6
    /// `StateFrame` so a resumed session knows the latest plan / hint.
    structured_plan_items: Vec<String>,
    structured_next_step_hint: Option<String>,
    /// Phase 2b: rich structured rolling summary (facts / decisions /
    /// open items / evidence / errors learned). `None` when the legacy
    /// prose-summary path was used and no structure was recovered.
    pub structured_rolling: Option<crate::agent::summary_worker::StructuredRollingSummary>,
}

/// Serialize a batch of `ContentBlock::ToolResult` blocks into the DB's
/// `tool_results_json` column.
///
/// The base shape is the existing `ContentBlock` JSON (so legacy readers still
/// work). When `tool_minimals` / `tool_names` are supplied, each entry is
/// augmented with a `content_minimal` and/or `tool_name` field keyed by
/// `tool_use_id`. The middle-tier read path in `commands/chat.rs` picks those
/// up to swap in the minimal receipt for older turns.
fn serialize_tool_results_with_receipts(
    tool_results: &[&ContentBlock],
    tool_minimals: Option<&HashMap<String, String>>,
    tool_names: Option<&HashMap<String, String>>,
) -> String {
    let mut value = match serde_json::to_value(tool_results) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    if let serde_json::Value::Array(ref mut arr) = value {
        for entry in arr.iter_mut() {
            let tool_use_id = entry
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if let Some(id) = tool_use_id {
                if let Some(map) = tool_minimals {
                    if let Some(min) = map.get(&id) {
                        entry["content_minimal"] = serde_json::Value::String(min.clone());
                    }
                }
                if let Some(map) = tool_names {
                    if let Some(name) = map.get(&id) {
                        entry["tool_name"] = serde_json::Value::String(name.clone());
                    }
                }
            }
        }
    }
    serde_json::to_string(&value).unwrap_or_default()
}

fn is_compaction_summary_text(text: &str) -> bool {
    text.starts_with("[会话滚动摘要]") || text.starts_with("[对话摘要]")
}

/// Build the outgoing LLM request messages from the in-memory full `messages`,
/// swapping in rule-based minimal receipts for tool-results belonging to turns
/// older than `recent_full_turns`.
///
/// Invariants:
/// - `messages` is never mutated. The in-memory log is the authoritative
///   full-fidelity source for Level-2 summarisation.
/// - A "turn boundary" is a `user` message whose content is plain `Text` (not a
///   tool-result carrier), walking from newest to oldest. The final iteration
///   of a run always counts as one full turn even before the next user message
///   arrives.
/// - p5 **two-boundary scheme**: two *independent* cutoffs are computed —
///   one counting `recent_full_turns` user-text turns (the classic one), one
///   counting the last [`CTX_KEEP_RECENT_TOOL_CARRIERS`] messages that carry
///   tool-result blocks. The effective cutoff is `min(turn_cutoff,
///   tool_cutoff)` so whichever boundary preserves *more* history wins. The
///   cutoff is then snapped **backwards** so it never lands between an
///   assistant `tool_use` and its matching `tool_result` — demoting only the
///   result (or only the call) would break provider pairing invariants.
/// - If `tool_minimals` lacks an entry for a given `tool_use_id`, the original
///   full content is kept. Callers that need a hard ceiling on tokens should
///   follow up with `compact_trim_tool_results` on the returned vector.
pub fn build_request_messages(
    messages: &[LlmMessage],
    tool_minimals: &HashMap<String, String>,
    recent_full_turns: usize,
    recent_tool_carriers: usize,
) -> Vec<LlmMessage> {
    let turn_cutoff = turn_based_recent_start(messages, recent_full_turns);
    let tool_cutoff = tool_carrier_recent_start(messages, recent_tool_carriers);
    // `min` = whichever boundary is *further back* (lower index) in the
    // message vector, i.e. preserves more messages at full fidelity.
    let mut recent_start = turn_cutoff.min(tool_cutoff);
    // Snap so we never split a tool_use / tool_result pair across the
    // boundary. The rule: walk back over any assistant message that is
    // purely a `ToolUse` carrier (its matching `ToolResult` will appear in
    // the preserved region). Equivalently, if `recent_start` currently
    // points at a user message containing only `ToolResult` blocks, step
    // back one more so the preceding assistant `ToolUse` comes with it.
    recent_start = snap_to_pair_boundary(messages, recent_start);

    // Second pass: materialise the request vector. For indices below
    // `recent_start`, substitute `content` of each `ToolResult` block with its
    // minimal receipt from the side-map.
    let mut out: Vec<LlmMessage> = Vec::with_capacity(messages.len());
    for (i, msg) in messages.iter().enumerate() {
        if i >= recent_start {
            out.push(msg.clone());
            continue;
        }
        match &msg.content {
            MessageContent::Blocks(blocks) => {
                let swapped: Vec<ContentBlock> = blocks
                    .iter()
                    .map(|b| match b {
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content: _,
                            is_error,
                        } => {
                            if let Some(min) = tool_minimals.get(tool_use_id) {
                                ContentBlock::ToolResult {
                                    tool_use_id: tool_use_id.clone(),
                                    content: crate::agent::tool_receipt::with_recall_hint(
                                        min,
                                        tool_use_id,
                                    ),
                                    is_error: *is_error,
                                }
                            } else {
                                b.clone()
                            }
                        }
                        _ => b.clone(),
                    })
                    .collect();
                out.push(LlmMessage {
                    role: msg.role.clone(),
                    content: MessageContent::Blocks(swapped),
                });
            }
            MessageContent::Text(_) => out.push(msg.clone()),
        }
    }
    out
}

/// Build the same request-view message slice the live agent uses before an
/// LLM call: demote older tool results to receipts, then hard-trim oversized
/// single results against the current message budget.
pub fn build_request_view_messages(
    messages: &[LlmMessage],
    tool_minimals: &HashMap<String, String>,
    recent_full_turns: usize,
    recent_tool_carriers: usize,
    message_budget_tokens: usize,
) -> Vec<LlmMessage> {
    let mut out = build_request_messages(
        messages,
        tool_minimals,
        recent_full_turns,
        recent_tool_carriers,
    );
    let single_limit = (message_budget_tokens as f64 * CONTEXT_SINGLE_RESULT_SHARE * 4.0) as usize;
    compact_trim_tool_results(&mut out, single_limit);
    out
}

/// Turn-based boundary: index of the oldest message kept full when the
/// policy is "keep the last N user-text turns". Returns 0 when there are
/// fewer than N qualifying user-text boundaries (i.e. keep everything).
fn turn_based_recent_start(messages: &[LlmMessage], recent_full_turns: usize) -> usize {
    if recent_full_turns == 0 {
        return messages.len();
    }
    let mut recent_start: usize = 0;
    let mut turns_seen: usize = 0;
    for (i, msg) in messages.iter().enumerate().rev() {
        let is_user_text_boundary = msg.role == "user"
            && matches!(&msg.content, MessageContent::Text(t) if !t.is_empty() && !is_compaction_summary_text(t));
        if is_user_text_boundary {
            turns_seen += 1;
            if turns_seen == recent_full_turns {
                recent_start = i;
            } else if turns_seen > recent_full_turns {
                break;
            }
        }
    }
    recent_start
}

/// Tool-carrier boundary: index of the oldest message that still falls
/// within the most recent `keep` messages carrying `ToolResult` blocks.
/// Returns 0 when there are fewer than `keep` such carriers (i.e. keep
/// everything full).
fn tool_carrier_recent_start(messages: &[LlmMessage], keep: usize) -> usize {
    if keep == 0 {
        return messages.len();
    }
    let mut carriers_seen: usize = 0;
    for (i, msg) in messages.iter().enumerate().rev() {
        let has_tool_result = matches!(
            &msg.content,
            MessageContent::Blocks(blocks) if blocks.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. }))
        );
        if has_tool_result {
            carriers_seen += 1;
            if carriers_seen >= keep {
                return i;
            }
        }
    }
    // Fewer than `keep` carriers exist anywhere — keep everything full.
    0
}

/// Snap a candidate cutoff *backwards* so it never sits between an
/// assistant `ToolUse` and its matching `ToolResult`. Called after the
/// two boundaries have been minned; safe no-op when the cutoff is
/// already at a pair boundary or at 0.
fn snap_to_pair_boundary(messages: &[LlmMessage], mut start: usize) -> usize {
    // Cheap short-circuits.
    if start == 0 || start >= messages.len() {
        return start;
    }
    // Step backwards while:
    //   (a) the message at `start` is a user message containing ONLY
    //       ToolResult blocks (i.e. it's a tool-result carrier, meaning
    //       the assistant's ToolUse lives at start-1), OR
    //   (b) the message at start-1 is an assistant message containing
    //       ToolUse blocks — keeping the pair together requires
    //       back-stepping across it.
    // Bound the walk to avoid pathological all-tool-result sequences.
    let mut guard = 0;
    while start > 0 && guard < 16 {
        let here = &messages[start];
        let starts_with_tool_result = matches!(
            &here.content,
            MessageContent::Blocks(blocks)
                if blocks.iter().all(|b| matches!(b, ContentBlock::ToolResult { .. }))
                && !blocks.is_empty()
        );
        let prev_has_tool_use = matches!(
            &messages[start - 1].content,
            MessageContent::Blocks(blocks) if blocks.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }))
        );
        if starts_with_tool_result || prev_has_tool_use {
            start -= 1;
            guard += 1;
            continue;
        }
        break;
    }
    start
}

/// Level-2 compaction: call LLM to summarise old messages, optionally merging
/// an existing rolling summary with the newly compacted history.
///
/// `keep_tokens` is the approximate token budget for the "recent tail" that
/// stays verbatim; everything older is fed to the summariser. Using tokens
/// (rather than the old char-based heuristic) prevents CJK-heavy sessions from
/// systematically under-keeping because 1 char is worth ~1 token in CJK but
/// ~0.25 tokens in English.
pub async fn compact_summarise(
    messages: Vec<LlmMessage>,
    keep_tokens: usize,
    client: &dyn crate::llm::LlmClient,
    model: &str,
    max_tokens: u32,
    existing_summary: Option<&str>,
) -> Option<CompactionOutcome> {
    if messages.len() < 2 {
        // Nothing meaningful to summarise if there are fewer than 2 messages.
        return None;
    }

    // Walk from the end, accumulating estimated tokens until we exceed
    // keep_tokens. Everything before the boundary index gets summarised.
    // We always keep at least the last 2 messages intact so the LLM has
    // immediate context regardless of how large they are.
    let mut acc = 0usize;
    // Default: summarise everything except the last 6 messages (3 tool call rounds).
    let mut split_idx = messages.len().saturating_sub(6);
    for (i, msg) in messages.iter().enumerate().rev() {
        // Use the shared token estimator so every byte we count here matches
        // the budget math in build_request_messages / estimate_request_input_tokens.
        acc += crate::llm::estimate_message_tokens(msg);
        if acc >= keep_tokens && i > 0 {
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
                            if text.is_empty() {
                                None
                            } else {
                                Some(text.clone())
                            }
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
            if text.is_empty() || is_compaction_summary_text(&text) {
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
    if history_text.trim().is_empty() && existing_summary.unwrap_or("").trim().is_empty() {
        return None;
    }

    let existing_summary_block = existing_summary
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .map(|summary| format!("已有滚动摘要：\n{}\n\n", summary))
        .unwrap_or_default();
    let summary_prompt = format!(
        "请将以下内容合并为一条新的滚动摘要。\n\
         摘要必须覆盖五部分：当前任务契约/用户目标、已完成工作、当前状态、未完成或待交接事项、关键文件/命令/结果。\n\
         必须保留仍然有效的任务目标、todo id、显式 handoff 目标（如 @Reviewer）、`[ProjectStatus]` 信号、阻塞原因、关键路径、错误和结论，省略重复中间步骤。\n\
         如果历史里出现了明确的下一位执行者、完成条件或待验证项，除非后续内容已经明确覆盖，否则不要丢失这些信息。\n\n\
         {}新近待压缩的对话历史：\n{}{}",
        existing_summary_block,
        history_text,
        crate::agent::summary_worker::STRUCTURED_SUMMARY_PROMPT_SUFFIX
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
            // p7: attempt structured JSON parse first; fall back to prose.
            let structured = crate::agent::summary_worker::parse_structured_summary(&resp.content);
            let merged_summary = if structured.summary.is_empty() {
                resp.content.trim().to_string()
            } else {
                structured.summary.clone()
            };
            let summary_msg = crate::agent::message_utils::rolling_summary_message(&merged_summary);
            let mut new_messages = vec![summary_msg];
            new_messages.extend_from_slice(&messages[split_idx..]);
            Some(CompactionOutcome {
                messages: new_messages,
                summary: merged_summary,
                input_tokens: resp.input_tokens,
                output_tokens: resp.output_tokens,
                structured_plan_items: structured.active_plan_items,
                structured_next_step_hint: structured.next_step_hint,
                structured_rolling: None,
            })
        }
        Ok(_) | Err(_) => None,
    }
}

/// Phase 2a: **Incremental / predictive-coding** Level-2 compaction.
///
/// Instead of re-summarising the entire old-message slab, this flow
/// feeds the LLM only
///
/// 1. the previous [`StructuredRollingSummary`] (prior),
/// 2. the "delta" = messages whose index is > `prev.last_msg_idx_covered`
///    (plus older uncovered messages on first run), and
/// 3. an optional `memory_snapshot` acting as a personalised codebook
///    (Phase 4b) — facts/decisions already in long-term memory don't
///    need to be re-summarised.
///
/// The LLM returns a list of [`MergeInstruction`]s and
/// [`apply_merge_instructions`] produces the new summary atomically —
/// if any step fails, the caller receives `None` and should keep the
/// previous summary unchanged (atomic rollback, FEC-style).
///
/// Benefits vs. the whole-history path:
/// - **O(|delta|)** input size instead of O(|history|) — up to 5–10×
///   latency reduction for long sessions.
/// - **Predictive coding**: only the residual is coded, satisfying
///   rate-distortion R(D) more tightly.
/// - **Memory-conditioned**: H(X | M) < H(X) reduces LLM work in
///   proportion to how much the agent already "knows".
///
/// Phase 6: The delta is first passed through `rule_preprocess` at
/// L2 aggressiveness to strip low-entropy noise before the LLM sees
/// it.
///
/// Returns `None` when:
/// - there is nothing new to summarise (delta empty), or
/// - the LLM call fails, or
/// - the returned merge instructions cannot be parsed / applied.
pub async fn compact_summarise_incremental(
    messages: Vec<LlmMessage>,
    keep_tokens: usize,
    client: &dyn crate::llm::LlmClient,
    model: &str,
    max_tokens: u32,
    prev: Option<&crate::agent::summary_worker::StructuredRollingSummary>,
    memory_snapshot: &[String],
) -> Option<CompactionOutcome> {
    use crate::agent::summary_worker as sw;

    if messages.len() < 2 {
        return None;
    }

    // Reuse the same tail-split math as the legacy path.
    let mut acc = 0usize;
    let mut split_idx = messages.len().saturating_sub(6);
    for (i, msg) in messages.iter().enumerate().rev() {
        acc += crate::llm::estimate_message_tokens(msg);
        if acc >= keep_tokens && i > 0 {
            split_idx = i;
            break;
        }
    }
    if split_idx == 0 {
        return None;
    }

    // Delta = messages [last_covered .. split_idx].
    let last_covered = prev.map(|p| p.last_msg_idx_covered).unwrap_or(0);
    let delta_start = last_covered.min(split_idx);
    let delta = &messages[delta_start..split_idx];
    if delta.is_empty() {
        return None;
    }

    // Phase 6: aggressive rule preprocessing on the LLM input only.
    // Never touches the real conversation vector.
    let pre_delta: Vec<LlmMessage> = crate::agent::rule_preprocess::preprocess_messages(
        delta,
        crate::agent::rule_preprocess::Level::L2,
    );

    let delta_text: String = pre_delta
        .iter()
        .map(format_message_for_summariser)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if delta_text.trim().is_empty() {
        return None;
    }

    // Render the prior summary + memory snapshot as a "known" block
    // that the LLM must not re-emit. Dictionary coding.
    let prior_block = match prev {
        Some(p) if !p.is_empty() => format!(
            "【先验摘要（已知事实/决策/待办，不要重复）】\n{}\n\n",
            p.render_for_prompt(4_096)
        ),
        _ => String::new(),
    };
    let memory_block = if memory_snapshot.is_empty() {
        String::new()
    } else {
        let lines: String = memory_snapshot
            .iter()
            .take(40)
            .map(|s| format!("- {}", s))
            .collect::<Vec<_>>()
            .join("\n");
        format!("【长期记忆码本（已固化的事实，不要重复）】\n{}\n\n", lines)
    };

    let prompt = format!(
        "你正在维护一份结构化的会话滚动摘要。请阅读下面的先验 + 新增片段，\
         仅针对**新增信息**给出 merge 指令（JSON 数组）。\n\n\
         {}{}【新增对话片段】\n{}\n{}",
        prior_block,
        memory_block,
        delta_text,
        sw::INCREMENTAL_MERGE_PROMPT_SUFFIX
    );

    let req = crate::llm::LlmRequest {
        messages: vec![crate::llm::LlmMessage {
            role: "user".into(),
            content: crate::llm::MessageContent::text(&prompt),
        }],
        system: None,
        tools: vec![],
        model: model.to_string(),
        max_tokens: max_tokens.clamp(512, 1024),
        stream: false,
        vision_override: Some(false),
    };

    let resp = match client.complete(req).await {
        Ok(r) if !r.content.is_empty() => r,
        _ => return None,
    };

    let instructions = match sw::parse_merge_instructions(&resp.content) {
        Ok(v) => v,
        Err(_) => return None,
    };

    let prev_snapshot = prev.cloned().unwrap_or_default();
    let new_summary = match sw::apply_merge_instructions(&prev_snapshot, &instructions, split_idx) {
        Ok(s) => s,
        Err(_) => return None,
    };

    let rendered = new_summary.render_for_prompt(4_096);
    let summary_msg = crate::agent::message_utils::rolling_summary_message(&rendered);
    let mut new_messages = vec![summary_msg];
    new_messages.extend_from_slice(&messages[split_idx..]);

    let plan_items: Vec<String> = new_summary
        .open_items
        .iter()
        .map(|o| o.text.clone())
        .collect();

    Some(CompactionOutcome {
        messages: new_messages,
        summary: new_summary.to_prose(),
        input_tokens: resp.input_tokens,
        output_tokens: resp.output_tokens,
        structured_plan_items: plan_items,
        structured_next_step_hint: None,
        structured_rolling: Some(new_summary),
    })
}

/// Render a single message as a compact snippet for the summariser
/// prompt. Extracted so both legacy and incremental paths share
/// identical formatting.
fn format_message_for_summariser(m: &LlmMessage) -> String {
    let role = if m.role == "user" {
        "用户/工具结果"
    } else {
        "智能体"
    };
    let text = match &m.content {
        crate::llm::MessageContent::Text(t) => t.clone(),
        crate::llm::MessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                crate::llm::ContentBlock::Text { text } => (!text.is_empty()).then(|| text.clone()),
                crate::llm::ContentBlock::ToolUse { name, input, .. } => {
                    let input_str = input.to_string();
                    let preview: String = input_str.chars().take(200).collect();
                    Some(format!("调用工具 {}: {}", name, preview))
                }
                crate::llm::ContentBlock::ToolResult {
                    content,
                    tool_use_id,
                    ..
                } => {
                    let preview: String = content.chars().take(200).collect();
                    Some(format!("[{}] 工具结果: {}", tool_use_id, preview))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    if text.is_empty() || is_compaction_summary_text(&text) {
        return String::new();
    }
    let snippet = if text.chars().count() > 500 {
        format!("{}...", text.chars().take(500).collect::<String>())
    } else {
        text
    };
    format!("[{}]: {}", role, snippet)
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

/// Unified entry point for the per-iteration LLM call. When
/// `enable_streaming` is false, delegates to `LlmClient::complete` (the
/// historical single-response path). When true, drives `LlmClient::stream`,
/// forwards each text delta as an `AgentEvent::TextDelta`, and folds the
/// emitted chunks back into an `LlmResponse` so the caller's downstream
/// bookkeeping (token counters, tool-call extraction, persistence) stays
/// unchanged.
async fn llm_call_unified(
    client: &dyn LlmClient,
    req: crate::llm::LlmRequest,
    enable_streaming: bool,
    event_tx: &mpsc::Sender<AgentEvent>,
) -> Result<crate::llm::LlmResponse> {
    if !enable_streaming {
        return client.complete(req).await;
    }

    let (tx, mut rx) = mpsc::channel::<crate::llm::LlmChunk>(32);
    let mut text = String::new();
    let mut tool_calls: Vec<crate::llm::ToolCall> = Vec::new();
    let mut input_tokens = 0u32;
    let mut output_tokens = 0u32;
    let mut stream_error: Option<String> = None;

    let stream_fut = client.stream(req, tx);
    let recv_fut = async {
        while let Some(chunk) = rx.recv().await {
            match chunk {
                crate::llm::LlmChunk::TextDelta(delta) => {
                    text.push_str(&delta);
                    let _ = event_tx.send(AgentEvent::TextDelta { delta }).await;
                }
                crate::llm::LlmChunk::ToolUse { id, name, input } => {
                    tool_calls.push(crate::llm::ToolCall { id, name, input });
                }
                crate::llm::LlmChunk::Done {
                    input_tokens: it,
                    output_tokens: ot,
                } => {
                    input_tokens = it;
                    output_tokens = ot;
                }
                crate::llm::LlmChunk::Error(e) => {
                    stream_error = Some(e);
                }
            }
        }
    };

    let (stream_res, _) = tokio::join!(stream_fut, recv_fut);
    stream_res?;
    if let Some(err) = stream_error {
        return Err(anyhow::anyhow!(err));
    }

    Ok(crate::llm::LlmResponse {
        content: text,
        tool_calls,
        input_tokens,
        output_tokens,
    })
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
    /// Shared plan-state map (session_id -> plan todo items). Set when the
    /// agent is driven by a host that tracks execution plans (desktop).
    /// Replaces the previous `app_handle: Option<tauri::AppHandle>` coupling
    /// so the agent loop is portable to headless / CLI hosts.
    pub plan_state: Option<PlanStateHandle>,
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
    /// Automatically trigger rolling-summary compaction once cumulative input
    /// tokens reach this threshold. `0` disables threshold-driven compaction.
    pub auto_compact_input_tokens_threshold: u32,
    /// When true, main-loop LLM calls go through `LlmClient::stream` and
    /// text deltas are forwarded as they arrive. When false, calls go
    /// through `LlmClient::complete` and the full text is emitted once per
    /// turn.
    pub enable_streaming: bool,
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
                    if let Some(confirms) = &self.confirmation_responses {
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
                        let cancel_for_perm = Arc::clone(cancel);
                        let approved = tokio::select! {
                            biased;
                            // User cancelled the whole run while waiting for permission
                            _ = async {
                                loop {
                                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                                    if cancel_for_perm.load(Ordering::Relaxed) { break; }
                                }
                            } => false,
                            // 60-second timeout waiting for the user to click approve/deny
                            result = tokio::time::timeout(
                                std::time::Duration::from_secs(60),
                                resp_rx,
                            ) => matches!(result, Ok(Ok(true))),
                        };
                        if approved {
                            debug!("User approved tool '{}' execution", name);
                        } else {
                            let reason = if cancel.load(Ordering::Relaxed) {
                                "已被用户取消"
                            } else {
                                "User denied this operation"
                            };
                            warn!("Tool '{}' denied/cancelled: {}", name, reason);
                            let _ = event_tx
                                .send(AgentEvent::ToolEnd {
                                    id: id.to_string(),
                                    name: name.to_string(),
                                    result: reason.into(),
                                    is_error: true,
                                })
                                .await;
                            blocks.push(ContentBlock::ToolResult {
                                tool_use_id: id.to_string(),
                                content: reason.into(),
                                is_error: true,
                            });
                            return blocks;
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

        let mut schema_correction_envelope: Option<String> = None;
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
                                schema_correction_envelope = maybe_schema_correction_envelope(
                                    &self.registry,
                                    name,
                                    &err_msg,
                                );
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
                    "Tool '{}' does not exist. Available tools: {}.",
                    name,
                    available.join(", ")
                ))
            }
        };

        let mut final_result_content =
            decorate_tool_failure_for_agent(name, input, &result.content, result.is_error);
        if let Some(envelope) = schema_correction_envelope {
            if !final_result_content.is_empty() {
                final_result_content.push_str("\n\n");
            }
            final_result_content.push_str(&envelope);
        }
        let end_result = format!("[trace_id:{}] {}", trace_id, final_result_content);
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
            let redacted_result = self.policy.redact_text(&final_result_content);
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
            &final_result_content,
            dynamic_result_limit(
                crate::llm::compute_total_input_budget(self.context_window, self.max_tokens)
                    .saturating_sub(crate::llm::estimate_request_overhead_tokens(
                        Some(&self.system_prompt),
                        &self
                            .registry
                            .to_tool_defs(crate::agent::tool::ToolDefMode::Minimal),
                    )),
            ),
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
    /// Write a single LlmMessage to the database immediately (real-time persistence).
    /// Called after every new assistant/tool message is appended during the agent loop,
    /// so messages survive even if the process is killed mid-run.
    async fn persist_message(&self, session_id: &str, msg: &LlmMessage, turn_index: Option<i64>) {
        self.persist_message_with_receipts(session_id, msg, turn_index, None, None)
            .await;
    }

    /// Variant of `persist_message` that also writes dual-version tool results.
    ///
    /// `tool_minimals` maps `tool_use_id → minimal receipt` and `tool_names` maps
    /// `tool_use_id → tool name`. When both are provided (typically only for the
    /// tool-result-carrier message), the serialized `tool_results_json` gains a
    /// `content_minimal` and `tool_name` field per entry, which the read path
    /// consumes to swap in the minimal form for older turns.
    async fn persist_message_with_receipts(
        &self,
        session_id: &str,
        msg: &LlmMessage,
        turn_index: Option<i64>,
        tool_minimals: Option<&HashMap<String, String>>,
        tool_names: Option<&HashMap<String, String>>,
    ) {
        let Some(ref db_arc) = self.db else { return };
        let db = db_arc.lock().await;
        use crate::llm::{ContentBlock, MessageContent};
        match &msg.content {
            MessageContent::Blocks(blocks) => {
                let tool_uses: Vec<&ContentBlock> = blocks
                    .iter()
                    .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                    .collect();
                let tool_results: Vec<&ContentBlock> = blocks
                    .iter()
                    .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
                    .collect();
                if !tool_uses.is_empty() {
                    let raw_text = msg.content.as_text();
                    let text = crate::agent::message_utils::strip_send_markers(&raw_text);
                    let calls_json = serde_json::to_string(&tool_uses).unwrap_or_default();
                    let _ = db.append_message_full(
                        session_id,
                        "assistant",
                        &text,
                        Some(&calls_json),
                        None,
                        turn_index,
                    );
                } else if !tool_results.is_empty() {
                    let results_json = serialize_tool_results_with_receipts(
                        &tool_results,
                        tool_minimals,
                        tool_names,
                    );
                    let _ = db.append_message_full(
                        session_id,
                        "user",
                        "",
                        None,
                        Some(&results_json),
                        turn_index,
                    );
                } else {
                    let text = msg.content.as_text();
                    if !text.is_empty() {
                        let _ = db.append_message_full(
                            session_id, &msg.role, &text, None, None, turn_index,
                        );
                    }
                }
            }
            MessageContent::Text(text) => {
                if !text.is_empty() {
                    let clean = if msg.role == "assistant" {
                        crate::agent::message_utils::strip_send_markers(text).into_owned()
                    } else {
                        text.clone()
                    };
                    if !clean.is_empty() {
                        let _ = db.append_message_full(
                            session_id, &msg.role, &clean, None, None, turn_index,
                        );
                    }
                }
            }
        }
    }

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
        // Accumulate new messages produced during this run in a separate buffer.
        // This is immune to compaction: compaction only modifies `messages` (the LLM context
        // window), but new_messages always grows monotonically with every new assistant/tool
        // message. The caller persists new_messages to the DB.
        let mut new_messages: Vec<LlmMessage> = Vec::new();
        // Dual-version tool-result side-maps (Phase C). Keyed by `tool_use_id`.
        // `tool_minimals` carries rule-based minimal receipts; `tool_names_by_id`
        // is used by `build_request_messages` to run the receipt generator on
        // legacy messages whose DB row did not yet carry `content_minimal`.
        //
        // Scope: lives for the duration of a single `run`. Messages re-hydrated
        // from the DB backfill on demand in the read path (commands/chat.rs).
        let mut tool_minimals: HashMap<String, String> = HashMap::new();
        let mut tool_names_by_id: HashMap<String, String> = HashMap::new();

        // Determine the turn_index for this run once, so all messages share the same index.
        // This must be computed before any messages are written.
        let turn_index: Option<i64> = if let Some(ref db_arc) = self.db {
            let db = db_arc.lock().await;
            let idx = db
                .get_messages_latest(&ctx.session_id, 2000)
                .map(|msgs| {
                    let max_turn = msgs.iter().filter_map(|m| m.turn_index).max().unwrap_or(0);
                    max_turn + 1
                })
                .unwrap_or(1);
            Some(idx)
        } else {
            None
        };
        let base_context_hash = stable_hash_messages(&messages);
        let base_message_count = messages.len();
        let mut restored_loop_history: Option<Vec<ToolCallRecord>> = None;
        let mut restored_seen_notifications: Option<HashSet<String>> = None;

        // Check for a resumable checkpoint from a previous (crashed) run
        if let Some(ref db_arc) = self.db {
            let db = db_arc.lock().await;
            match db.load_checkpoint(&ctx.session_id) {
                Ok(Some((iter, json))) => {
                    match serde_json::from_str::<AgentCheckpointPayload>(&json) {
                        Ok(payload)
                            if !payload.messages.is_empty()
                                && payload.base_context_hash == base_context_hash
                                && payload.base_message_count == base_message_count =>
                        {
                            info!(
                                "Resuming from checkpoint at iteration {} for session {}",
                                iter, ctx.session_id
                            );
                            restored_loop_history = Some(payload.loop_history.clone());
                            restored_seen_notifications =
                                Some(payload.seen_notifications.into_iter().collect());
                            messages = payload.messages;
                            info!("Checkpoint restored: {} messages", messages.len());
                            let _ = db.finish_checkpoint(&ctx.session_id, "resumed");
                        }
                        Ok(payload) => {
                            warn!(
                                "Checkpoint stale for session {} (base hash/count mismatch: {}:{}, current {}:{}); ignoring",
                                ctx.session_id,
                                payload.base_context_hash,
                                payload.base_message_count,
                                base_context_hash,
                                base_message_count
                            );
                            let _ = db.finish_checkpoint(&ctx.session_id, "stale");
                        }
                        Err(_) => {
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
        let mut loop_detector = LoopDetectorState {
            history: restored_loop_history.unwrap_or_default(),
        };
        let mut rolling_summary = String::new();
        let mut seen_notifications: HashSet<String> =
            restored_seen_notifications.unwrap_or_default();
        let mut rolling_summary_version = 0i64;
        let mut cumulative_input_tokens = 0i64;
        let mut cumulative_output_tokens = 0i64;
        if let Some(ref db_arc) = self.db {
            let db = db_arc.lock().await;
            match db.get_session_context_state(&ctx.session_id) {
                Ok(Some(state)) => {
                    rolling_summary = state.rolling_summary;
                    rolling_summary_version = state.rolling_summary_version;
                    cumulative_input_tokens = state.total_input_tokens;
                    cumulative_output_tokens = state.total_output_tokens;
                }
                Ok(None) => {}
                Err(error) => warn!("Failed to load session context state: {}", error),
            }
        }
        let threshold_step = i64::from(self.auto_compact_input_tokens_threshold);
        let mut next_auto_compact_threshold = if threshold_step > 0 {
            ((cumulative_input_tokens / threshold_step) + 1) * threshold_step
        } else {
            i64::MAX
        };

        for _iteration in 0..max_iterations {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let tool_defs = self
                .registry
                .to_tool_defs(crate::agent::tool::ToolDefMode::Minimal);

            // Drain pending notifications (e.g. @mention alerts from other Koi)
            if let Some(ref rx_mutex) = self.notification_rx {
                let mut rx = rx_mutex.lock().await;
                while let Ok(msg) = rx.try_recv() {
                    if !seen_notifications.insert(msg.clone()) {
                        info!("Skipping duplicate notification already seen in this run");
                        continue;
                    }
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
                let total_budget =
                    crate::llm::compute_total_input_budget(self.context_window, self.max_tokens);
                let static_overhead_tokens = crate::llm::estimate_request_overhead_tokens(
                    Some(&self.system_prompt),
                    &tool_defs,
                );
                let message_budget = total_budget.saturating_sub(static_overhead_tokens);
                // single_limit in chars: message_budget_tokens × share × 4 chars/token
                let single_limit =
                    (message_budget as f64 * CONTEXT_SINGLE_RESULT_SHARE * 4.0) as usize;

                // Build the request-view first. For the middle tier, tool-result
                // blocks are swapped to rule-based minimal receipts; this is
                // expected to bring most sessions well under the budget, after
                // which the legacy char-trim is only a safety net.
                let mut demoted = build_request_messages(
                    &messages,
                    &tool_minimals,
                    CTX_PRESERVE_RECENT_TURNS,
                    CTX_KEEP_RECENT_TOOL_CARRIERS,
                );
                compact_trim_tool_results(&mut demoted, single_limit);

                // Dynamic compaction (Level-2 proactive): if estimated token count exceeds
                // 80% of the budget, summarise old messages now — before the LLM call —
                // rather than waiting for a context overflow error (which some models never
                // emit, instead silently truncating or producing near-empty responses).
                //
                // We retry with a smaller keep_tokens if the first pass still leaves
                // too many tokens (can happen when the budget estimate is conservative).
                let req_messages = vision::inject_selected_context(&demoted, &ctx.session_id).await;
                let mut estimated = crate::llm::estimate_request_input_tokens(
                    &req_messages,
                    Some(&self.system_prompt),
                    &tool_defs,
                );
                info!(
                    "context check: {} messages, ~{} estimated request tokens (total_budget={}, message_budget={}, threshold={})",
                    messages.len(),
                    estimated,
                    total_budget,
                    message_budget,
                    (total_budget as f64 * 0.60) as usize,
                );
                // Up to 2 compaction passes: first at 60% keep, then at 30% keep
                let keep_ratios: &[f64] =
                    &[SUMMARY_KEEP_RECENT_RATIO, SUMMARY_KEEP_RECENT_RATIO * 0.5];
                let min_threshold_estimate = (total_budget as f64 * 0.35) as usize;
                for (pass, &ratio) in keep_ratios.iter().enumerate() {
                    let threshold_reached = threshold_step > 0
                        && cumulative_input_tokens >= next_auto_compact_threshold
                        && estimated >= min_threshold_estimate;
                    // Bug fix 🔴1: once the per-request estimate is below the
                    // 60% safety line, stop — regardless of whether cumulative
                    // threshold_reached is still true. Otherwise a session
                    // repeatedly pays for Level-2 summarisation every iteration
                    // even though nothing is actually over budget.
                    if estimated <= (total_budget as f64 * 0.60) as usize {
                        break;
                    }
                    let keep_tokens = (message_budget as f64 * ratio) as usize;
                    warn!(
                        "proactive compaction pass={} estimated_tokens={} total_budget={} message_budget={} keep_tokens={} cumulative_input_tokens={} threshold_reached={} min_threshold_estimate={}",
                        pass + 1, estimated, total_budget, message_budget, keep_tokens, cumulative_input_tokens, threshold_reached, min_threshold_estimate
                    );
                    // Level-2 summarisation receives the FULL in-memory messages
                    // (never the demoted/minimal view) so the summariser has
                    // enough signal to produce a faithful rolling summary.
                    if let Some(compacted) = compact_summarise(
                        messages.clone(),
                        keep_tokens,
                        self.client.as_ref(),
                        &self.model,
                        self.max_tokens,
                        (!rolling_summary.trim().is_empty()).then_some(rolling_summary.as_str()),
                    )
                    .await
                    {
                        // Account for prompt/completion tokens billed to the summariser.
                        total_input = total_input.saturating_add(compacted.input_tokens);
                        total_output = total_output.saturating_add(compacted.output_tokens);
                        cumulative_input_tokens = cumulative_input_tokens
                            .saturating_add(i64::from(compacted.input_tokens));
                        cumulative_output_tokens = cumulative_output_tokens
                            .saturating_add(i64::from(compacted.output_tokens));
                        let compacted_demoted = build_request_messages(
                            &compacted.messages,
                            &tool_minimals,
                            CTX_PRESERVE_RECENT_TURNS,
                            CTX_KEEP_RECENT_TOOL_CARRIERS,
                        );
                        let compacted_req_messages =
                            vision::inject_selected_context(&compacted_demoted, &ctx.session_id)
                                .await;
                        let new_estimated = crate::llm::estimate_request_input_tokens(
                            &compacted_req_messages,
                            Some(&self.system_prompt),
                            &tool_defs,
                        );
                        info!(
                            "proactive summarisation pass={} complete: {} → {} messages, tokens {} → {}",
                            pass + 1,
                            messages.len(),
                            compacted.messages.len(),
                            estimated,
                            new_estimated,
                        );
                        rolling_summary = compacted.summary;
                        rolling_summary_version += 1;
                        messages = compacted.messages;
                        estimated = new_estimated;
                        // p7: structured fields from summariser output; forwarded
                        // to the state_frame persist block a few lines below.
                        let structured_plan_items_latest: Vec<String> =
                            compacted.structured_plan_items.clone();
                        let structured_next_step_hint_latest: Option<String> =
                            compacted.structured_next_step_hint.clone();
                        if threshold_reached {
                            // Bug fix 🔴2: bump relative to the CURRENT cumulative
                            // so that one oversized response cannot push cumulative
                            // far past the new threshold and cause the next
                            // iteration to re-trigger immediately.
                            let step = threshold_step.max(1);
                            let from_old = next_auto_compact_threshold.saturating_add(step);
                            let from_now = cumulative_input_tokens.saturating_add(step);
                            next_auto_compact_threshold = from_old.max(from_now);
                        }
                        if let Some(ref db_arc) = self.db {
                            let db = db_arc.lock().await;
                            if let Err(error) = db.update_session_rolling_summary(
                                &ctx.session_id,
                                &rolling_summary,
                                rolling_summary_version,
                            ) {
                                warn!("Failed to persist rolling summary: {}", error);
                            }
                            // p6 + p7: refresh the state frame snapshot so a
                            // resume right after this compaction picks up the
                            // most recent tool call / error signals. Merge in
                            // the structured active_plan_items / next_step_hint
                            // if the summariser returned them.
                            let mut frame =
                                crate::agent::state_frame::derive_frame_from_tail(&messages, 24);
                            if !structured_plan_items_latest.is_empty() {
                                frame.active_plan_items = structured_plan_items_latest.clone();
                            }
                            if structured_next_step_hint_latest.is_some() {
                                frame.next_step_hint = structured_next_step_hint_latest.clone();
                            }
                            let frame_json = frame.to_json();
                            if let Err(error) = db.update_session_state_frame_json(
                                &ctx.session_id,
                                frame_json.as_deref(),
                            ) {
                                warn!("Failed to persist state frame: {}", error);
                            }
                        }
                    } else {
                        // Summarisation failed — stop trying
                        warn!("proactive summarisation pass={} failed, proceeding with current context", pass + 1);
                        break;
                    }
                }

                // Emit a context-usage snapshot for the UI ring indicator. This fires on
                // every iteration regardless of whether compaction ran, so the ring
                // reflects the true state of the request we're about to send.
                let trigger_threshold = (total_budget as f64 * 0.60) as usize;

                // p8 — compute a best-effort per-layer breakdown so the
                // ring indicator can show which slot (system / tools /
                // history / tool-results / vision) is dominating context.
                // This uses the already-built demoted + vision-injected
                // request view so it matches the numbers sent to the LLM.
                let breakdown_snapshot = {
                    let req_view = vision::inject_selected_context(&demoted, &ctx.session_id).await;
                    let rolling_tokens = if rolling_summary.trim().is_empty() {
                        0
                    } else {
                        crate::llm::estimate_tokens(&rolling_summary) as u32
                    };
                    let bd = crate::agent::harness::context_builder::compute_layered_breakdown(
                        &req_view,
                        Some(&self.system_prompt),
                        &tool_defs,
                        &tool_minimals,
                        rolling_tokens,
                        0,
                    );
                    crate::agent::messages::LayeredTokenBreakdownSnapshot {
                        persona: bd.prompt.persona,
                        scene: bd.prompt.scene,
                        memory: bd.prompt.memory,
                        project: bd.prompt.project,
                        platform_hint: bd.prompt.platform_hint,
                        tool_defs: bd.tool_def_tokens,
                        history_text: bd.history_text_tokens,
                        history_tool_result_full: bd.history_tool_result_full_tokens,
                        history_tool_result_receipt: bd.history_tool_result_receipt_tokens,
                        rolling_summary: bd.rolling_summary_tokens,
                        state_frame: bd.state_frame_tokens,
                        vision: bd.vision_tokens,
                        request_overhead: bd.request_overhead_tokens,
                    }
                };

                let _ = event_tx
                    .send(AgentEvent::ContextUsage {
                        estimated_input_tokens: estimated.min(u32::MAX as usize) as u32,
                        total_input_budget: total_budget.min(u32::MAX as usize) as u32,
                        trigger_threshold: trigger_threshold.min(u32::MAX as usize) as u32,
                        cumulative_input_tokens: cumulative_input_tokens.clamp(0, u32::MAX as i64)
                            as u32,
                        cumulative_output_tokens: cumulative_output_tokens.clamp(0, u32::MAX as i64)
                            as u32,
                        rolling_summary_version: rolling_summary_version.clamp(0, u32::MAX as i64)
                            as u32,
                        auto_compact_threshold: self.auto_compact_input_tokens_threshold,
                        layered_breakdown: Some(breakdown_snapshot),
                    })
                    .await;
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
                    // Tool-result blocks from older turns are swapped to their
                    // minimal receipts before vision-context injection.
                    let demoted_messages = build_request_messages(
                        &messages,
                        &tool_minimals,
                        CTX_PRESERVE_RECENT_TURNS,
                        CTX_KEEP_RECENT_TOOL_CARRIERS,
                    );
                    let req_messages =
                        vision::inject_selected_context(&demoted_messages, &ctx.session_id).await;
                    // Route through RequestBuilder so provider-specific
                    // ceilings (e.g. Anthropic's 8192 max_tokens cap) are
                    // applied in one place instead of leaking into every
                    // call site. The builder is cheap to construct and we
                    // rebuild per-iteration so fallback to another model —
                    // possibly a different provider — picks up the right
                    // limits automatically.
                    let provider_kind =
                        crate::agent::harness::ProviderKind::from_model_id(model_candidate);
                    let req = crate::agent::harness::RequestBuilder::new(
                        req_messages,
                        Some(self.system_prompt.clone()),
                        tool_defs.clone(),
                        model_candidate.clone(),
                        self.max_tokens,
                    )
                    .with_stream(self.enable_streaming)
                    .with_vision_override(self.vision_override)
                    .build_for(provider_kind);

                    for attempt in 0..LLM_MAX_RETRIES {
                        // Check cancel before each LLM attempt
                        if cancel.load(Ordering::Relaxed) {
                            break 'model_loop;
                        }

                        // Race the LLM call against the cancel flag (200ms poll)
                        let cancel_for_llm = Arc::clone(&cancel);
                        let llm_result = tokio::select! {
                            biased;
                            _ = async {
                                loop {
                                    tokio::time::sleep(
                                        std::time::Duration::from_millis(200),
                                    ).await;
                                    if cancel_for_llm.load(Ordering::Relaxed) { break; }
                                }
                            } => {
                                info!("LLM call cancelled by user");
                                break 'model_loop;
                            }
                            r = llm_call_unified(
                                self.client.as_ref(),
                                req.clone(),
                                self.enable_streaming,
                                &event_tx,
                            ) => r,
                        };

                        match llm_result {
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
                                    let total_budget = crate::llm::compute_total_input_budget(
                                        self.context_window,
                                        self.max_tokens,
                                    );
                                    let static_overhead_tokens =
                                        crate::llm::estimate_request_overhead_tokens(
                                            Some(&self.system_prompt),
                                            &tool_defs,
                                        );
                                    let message_budget =
                                        total_budget.saturating_sub(static_overhead_tokens);
                                    // keep_tokens: newest 60% of the budget in tokens
                                    let keep_tokens = (message_budget as f64
                                        * SUMMARY_KEEP_RECENT_RATIO)
                                        as usize;
                                    warn!("context overflow — attempting LLM summarisation (keep_tokens={})", keep_tokens);
                                    let compacted = compact_summarise(
                                        messages.clone(),
                                        keep_tokens,
                                        self.client.as_ref(),
                                        model_candidate,
                                        self.max_tokens,
                                        (!rolling_summary.trim().is_empty())
                                            .then_some(rolling_summary.as_str()),
                                    )
                                    .await;
                                    if let Some(c) = compacted {
                                        total_input = total_input.saturating_add(c.input_tokens);
                                        total_output = total_output.saturating_add(c.output_tokens);
                                        cumulative_input_tokens = cumulative_input_tokens
                                            .saturating_add(i64::from(c.input_tokens));
                                        cumulative_output_tokens = cumulative_output_tokens
                                            .saturating_add(i64::from(c.output_tokens));
                                        rolling_summary = c.summary;
                                        rolling_summary_version += 1;
                                        messages = c.messages;
                                        if let Some(ref db_arc) = self.db {
                                            let db = db_arc.lock().await;
                                            if let Err(error) = db.update_session_rolling_summary(
                                                &ctx.session_id,
                                                &rolling_summary,
                                                rolling_summary_version,
                                            ) {
                                                warn!(
                                                    "Failed to persist rolling summary after overflow: {}",
                                                    error
                                                );
                                            }
                                            // p6 + p7: refresh state frame on overflow-triggered
                                            // compaction as well so the UI sees a fresh snapshot.
                                            let mut frame =
                                                crate::agent::state_frame::derive_frame_from_tail(
                                                    &messages, 24,
                                                );
                                            if !c.structured_plan_items.is_empty() {
                                                frame.active_plan_items =
                                                    c.structured_plan_items.clone();
                                            }
                                            if c.structured_next_step_hint.is_some() {
                                                frame.next_step_hint =
                                                    c.structured_next_step_hint.clone();
                                            }
                                            let frame_json = frame.to_json();
                                            if let Err(error) = db.update_session_state_frame_json(
                                                &ctx.session_id,
                                                frame_json.as_deref(),
                                            ) {
                                                warn!(
                                                    "Failed to persist state frame after overflow: {}",
                                                    error
                                                );
                                            }
                                        }
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
                                        || msg.contains("529")
                                        // Network-level decode errors (server closed connection
                                        // mid-stream, incomplete chunk, etc.) are transient
                                        || msg.contains("error decoding response body")
                                        || msg.contains("incomplete message")
                                        || msg.contains("unexpected eof")
                                        || msg.contains("broken pipe");
                                    if !is_transient || attempt + 1 == LLM_MAX_RETRIES {
                                        last_err = Some(e);
                                        break 'model_loop;
                                    }
                                    // Interruptible backoff sleep — cancel exits immediately
                                    let backoff = std::time::Duration::from_secs(1 << attempt);
                                    let cancel_for_sleep = Arc::clone(&cancel);
                                    tokio::select! {
                                        biased;
                                        _ = async {
                                            loop {
                                                tokio::time::sleep(
                                                    std::time::Duration::from_millis(200),
                                                ).await;
                                                if cancel_for_sleep.load(Ordering::Relaxed) {
                                                    break;
                                                }
                                            }
                                        } => { break 'model_loop; }
                                        _ = tokio::time::sleep(backoff) => {}
                                    }
                                    last_err = Some(e);
                                }
                            }
                        }
                    }
                }
                match resp {
                    Some(r) => r,
                    None => {
                        // If cancelled, break the outer iteration loop cleanly
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        return Err(last_err.unwrap_or_else(|| anyhow::anyhow!("LLM call failed")));
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
            cumulative_input_tokens += i64::from(response.input_tokens);
            cumulative_output_tokens += i64::from(response.output_tokens);
            if let Some(ref db_arc) = self.db {
                let db = db_arc.lock().await;
                if let Err(error) = db.update_session_usage_totals(
                    &ctx.session_id,
                    response.input_tokens,
                    response.output_tokens,
                ) {
                    warn!("Failed to persist usage totals: {}", error);
                }
            }

            let text_buf = response.content.clone();
            let tool_calls: Vec<(String, String, serde_json::Value)> = response
                .tool_calls
                .iter()
                .map(|tc| (tc.id.clone(), tc.name.clone(), tc.input.clone()))
                .collect();

            // In non-streaming mode we emit the whole response as a single
            // `TextDelta`. Streaming mode already forwarded per-chunk deltas
            // inside `llm_call_unified`, so re-emitting here would duplicate
            // the text in the UI.
            if !self.enable_streaming && !text_buf.is_empty() {
                let _ = event_tx
                    .send(AgentEvent::TextDelta {
                        delta: text_buf.clone(),
                    })
                    .await;
            }

            // If no tool calls, check for unfinished plan_todo items before exiting.
            // If any todo is still in_progress or pending, inject a reminder and continue.
            if tool_calls.is_empty() {
                // Check if there are any in_progress or pending todos that haven't been resolved
                let unfinished_todos = if let Some(ref plan_state_arc) = self.plan_state {
                    let plan_state = plan_state_arc.lock().await;
                    plan_state
                        .get(&ctx.session_id)
                        .map(|todos| {
                            todos
                                .iter()
                                .filter(|t| t.status == "in_progress" || t.status == "pending")
                                .map(|t| format!("- [{}] {}", t.status, t.content))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                } else {
                    vec![]
                };

                if !unfinished_todos.is_empty() {
                    // Inject a reminder and continue the loop instead of breaking
                    warn!(
                        "LLM tried to exit with {} unfinished todo(s), injecting reminder",
                        unfinished_todos.len()
                    );
                    let asst_msg = LlmMessage {
                        role: "assistant".into(),
                        content: MessageContent::text(&text_buf),
                    };
                    new_messages.push(asst_msg.clone());
                    messages.push(asst_msg.clone());
                    self.persist_message(&ctx.session_id, &asst_msg, turn_index)
                        .await;
                    let reminder = format!(
                        "⚠️ 你的计划中还有未完成的步骤，请继续执行或将其标记为 cancelled：\n{}\n\n\
                         `plan_todo` 只更新计划板，本身不算实际进展。请继续使用能真正推进任务的工具，或直接产出可交付结果；如果这些步骤无法完成，再用 `plan_todo` 标记为 cancelled 并说明原因。",
                        unfinished_todos.join("\n")
                    );
                    let reminder_msg = LlmMessage {
                        role: "user".into(),
                        content: MessageContent::text(&reminder),
                    };
                    new_messages.push(reminder_msg.clone());
                    messages.push(reminder_msg.clone());
                    self.persist_message(&ctx.session_id, &reminder_msg, turn_index)
                        .await;
                    // Continue the loop (don't break)
                } else {
                    // All todos are done (or no plan exists) — normal exit
                    let asst_msg = LlmMessage {
                        role: "assistant".into(),
                        content: MessageContent::text(&text_buf),
                    };
                    new_messages.push(asst_msg.clone());
                    messages.push(asst_msg.clone());
                    self.persist_message(&ctx.session_id, &asst_msg, turn_index)
                        .await;
                    break;
                }
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

            let all_tools_blocked =
                !blocked_tool_ids.is_empty() && blocked_tool_ids.len() == tool_calls.len();

            // If all tool calls are blocked, surface a stronger reminder but still
            // let the agent see synthetic tool failures and produce a final answer
            // from the evidence it already has.
            if all_tools_blocked {
                let combined_msg = warning_messages.join("\n");
                let _ = event_tx
                    .send(AgentEvent::TextDelta {
                        delta: format!("\n\n[系统] {}\n", combined_msg),
                    })
                    .await;
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
            let asst_tool_msg = LlmMessage {
                role: "assistant".into(),
                content: MessageContent::Blocks(assistant_blocks),
            };
            new_messages.push(asst_tool_msg.clone());
            messages.push(asst_tool_msg.clone());
            self.persist_message(&ctx.session_id, &asst_tool_msg, turn_index)
                .await;

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

            // ── Record results into loop detector + compute minimal receipts ─
            for block in &tool_result_blocks {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    ..
                } = block
                {
                    if let Some((_, name, input)) =
                        tool_calls.iter().find(|(id, _, _)| id == tool_use_id)
                    {
                        let rh = stable_hash_result(content);
                        loop_detector.record(name, input, rh);
                        let receipt = super::tool_receipt::render_receipt(
                            name, input, content, *is_error, None,
                        );
                        tool_minimals.insert(tool_use_id.clone(), receipt);
                        tool_names_by_id.insert(tool_use_id.clone(), name.clone());
                    } else {
                        // No matching ToolUse (shouldn't happen) — still emit a
                        // generic receipt so the middle-tier read path doesn't
                        // have to backfill from an unknown tool name.
                        let receipt = super::tool_receipt::render_receipt(
                            "unknown",
                            &serde_json::Value::Null,
                            content,
                            *is_error,
                            None,
                        );
                        tool_minimals.insert(tool_use_id.clone(), receipt);
                    }
                }
            }

            let warning_reminder = if all_tools_blocked {
                Some(format!(
                    "[系统提醒]\n{}\n本轮所有工具调用都已被循环检测器阻断。现在必须停止继续沿用刚才的工具路径，直接基于已有证据给出当前最佳答复；如果信息仍有缺口，请明确列出不确定项、现有判断依据与后续建议，不要继续调用工具。",
                    warning_messages.join("\n")
                ))
            } else if !warning_messages.is_empty() && blocked_tool_ids.is_empty() {
                Some(format!(
                    "[系统提醒]\n{}\n请先基于现有结果收束、总结或切换方法，不要机械重复刚才的工具路径。",
                    warning_messages.join("\n")
                ))
            } else {
                None
            };

            // Add tool results as user message
            let tool_result_msg = LlmMessage {
                role: "user".into(),
                content: MessageContent::Blocks(tool_result_blocks),
            };
            new_messages.push(tool_result_msg.clone());
            messages.push(tool_result_msg.clone());
            self.persist_message_with_receipts(
                &ctx.session_id,
                &tool_result_msg,
                turn_index,
                Some(&tool_minimals),
                Some(&tool_names_by_id),
            )
            .await;
            messages = crate::agent::message_utils::collapse_superseded_tool_failures(messages);

            if let Some(reminder) = warning_reminder {
                let reminder_msg = LlmMessage {
                    role: "user".into(),
                    content: MessageContent::text(&reminder),
                };
                let _ = event_tx
                    .send(AgentEvent::TextDelta {
                        delta: format!("\n\n{}\n", reminder),
                    })
                    .await;
                new_messages.push(reminder_msg.clone());
                messages.push(reminder_msg.clone());
                self.persist_message(&ctx.session_id, &reminder_msg, turn_index)
                    .await;
            }

            // Write checkpoint after each iteration (with size guard)
            if let Some(ref db_arc) = self.db {
                let db = db_arc.lock().await;
                let payload = AgentCheckpointPayload {
                    base_context_hash,
                    base_message_count,
                    messages: messages.clone(),
                    loop_history: loop_detector.history.clone(),
                    seen_notifications: seen_notifications.iter().cloned().collect(),
                };
                match serde_json::to_string(&payload) {
                    Ok(json) => {
                        if json.len() > CHECKPOINT_MAX_BYTES {
                            warn!(
                                "Checkpoint too large ({} bytes > {} limit), skipping write",
                                json.len(),
                                CHECKPOINT_MAX_BYTES
                            );
                            let _ = db.finish_checkpoint(&ctx.session_id, "oversized");
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

        // If this run was cancelled, scrub any partially-written agent messages
        // for this turn so they don't pollute the conversation history.
        let was_cancelled = cancel.load(Ordering::Relaxed);
        if was_cancelled {
            if let (Some(db_arc), Some(idx)) = (self.db.as_ref(), turn_index) {
                let db = db_arc.lock().await;
                match db.delete_messages_by_turn_index(&ctx.session_id, idx) {
                    Ok(deleted) if deleted > 0 => {
                        info!(
                            "Cancelled run cleaned up {} incomplete messages for turn {} session {}",
                            deleted, idx, ctx.session_id
                        );
                        let _ = db.recompute_session_message_count(&ctx.session_id);
                    }
                    Ok(_) => {}
                    Err(e) => warn!("Failed to clean up cancelled turn messages: {}", e),
                }
            }
        }

        // Return only the new messages produced during this run (not the full context).
        // new_messages is immune to compaction: it accumulates every assistant/tool message
        // appended during the run, regardless of how many times the context was compacted.
        // The caller (persist_agent_turn) saves exactly these messages to the DB.
        Ok((new_messages, total_input, total_output))
    }
}

/// Convert low-level tool errors into actionable, user-friendly messages.
fn friendly_tool_error(tool_name: &str, raw_error: &str) -> String {
    let raw_lower = raw_error.to_lowercase();

    if is_structural_schema_error(raw_error) {
        return format!(
            "[{}] 工具输入与 schema 不匹配。请根据下方 schema_correction 修正参数，仅重试这个工具一次。\n详情：{}",
            tool_name, raw_error
        );
    }

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

fn is_structural_schema_error(raw_error: &str) -> bool {
    let lower = raw_error.to_lowercase();
    lower.contains("missing field")
        || lower.contains("missing required")
        || lower.contains("invalid type")
        || lower.contains("invalid value")
        || lower.contains("unknown field")
        || lower.contains("unknown variant")
        || lower.contains("did not match any variant")
        || lower.contains("no variant of enum")
        || lower.contains("additional properties are not allowed")
        || lower.contains("additionalproperties")
        || lower.contains("expected u")
        || lower.contains("expected i")
        || lower.contains("expected a string")
        || lower.contains("expected a boolean")
        || lower.contains("expected an array")
        || lower.contains("expected a map")
        || lower.contains("expected struct")
}

fn maybe_schema_correction_envelope(
    registry: &crate::agent::tool::ToolRegistry,
    tool_name: &str,
    raw_error: &str,
) -> Option<String> {
    if !is_structural_schema_error(raw_error) {
        return None;
    }
    let tool_def = registry.to_tool_defs_for(tool_name, crate::agent::tool::ToolDefMode::Full)?;
    let full_schema_json = serde_json::to_string(&tool_def.input_schema).ok()?;
    Some(format!(
        "[schema_correction tool={}]\n{}\n[/schema_correction]",
        tool_name, full_schema_json
    ))
}

fn decorate_tool_failure_for_agent(
    tool_name: &str,
    input: &serde_json::Value,
    content: &str,
    is_error: bool,
) -> String {
    if !is_error || content.contains("[ConstraintViolation]") {
        return content.to_string();
    }

    let lower = content.to_lowercase();
    let looks_like_constraint = lower.contains("missing required parameter")
        || lower.contains(" requires ")
        || lower.contains("requires '")
        || lower.contains("requires \"")
        || lower.contains("unknown action")
        || lower.contains("not configured")
        || lower.contains("tool is disabled")
        || lower.contains("working directory does not exist")
        || lower.contains("file not found")
        || lower.contains("path_a not found")
        || lower.contains("path_b not found")
        || lower.contains("too large")
        || lower.contains("permission denied")
        || lower.contains("access is denied")
        || lower.contains("denied by policy");

    if !looks_like_constraint {
        return content.to_string();
    }

    let input_preview = serde_json::to_string(input).unwrap_or_default();
    let mut suggestions: Vec<&str> = Vec::new();
    if lower.contains("missing required parameter")
        || lower.contains(" requires ")
        || lower.contains("requires '")
        || lower.contains("requires \"")
    {
        suggestions.push("补齐工具要求的必填参数后重试");
    }
    if lower.contains("not configured") || lower.contains("tool is disabled") {
        suggestions.push("先在 Settings 中启用或配置该工具");
    }
    if lower.contains("working directory does not exist")
        || lower.contains("file not found")
        || lower.contains("path_a not found")
        || lower.contains("path_b not found")
    {
        suggestions.push("先确认路径存在，必要时先列目录或创建目标");
    }
    if lower.contains("too large") {
        suggestions.push("改用 offset/limit、分页、分块或更小范围参数");
    }
    if lower.contains("permission denied")
        || lower.contains("access is denied")
        || lower.contains("denied by policy")
    {
        suggestions.push("改用允许的路径/命令，或请求用户确认后再重试");
    }
    if suggestions.is_empty() {
        suggestions.push("修正输入或先满足前置条件后再重试");
    }

    let mut deduped = HashSet::new();
    let suggestion_text = suggestions
        .into_iter()
        .filter(|item| deduped.insert(*item))
        .collect::<Vec<_>>()
        .join("；");

    format!(
        "[ConstraintViolation] 工具 `{}` 本次调用未生效。请不要重复相同调用，先按提示调整后再试。\n建议：{}\n输入：{}\n原始结果：{}",
        tool_name, suggestion_text, input_preview, content
    )
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
    use super::{
        build_request_messages, compact_summarise, compact_trim_tool_results,
        is_structural_schema_error, maybe_schema_correction_envelope,
        serialize_tool_results_with_receipts, CTX_KEEP_RECENT_TOOL_CARRIERS,
        CTX_PRESERVE_RECENT_TURNS, CTX_TRIM_HEAD, CTX_TRIM_TAIL,
    };
    use crate::agent::tool::{Tool, ToolRegistry};
    use crate::llm::{ContentBlock, LlmChunk, LlmMessage, LlmRequest, LlmResponse, MessageContent};
    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::{json, Value};
    use std::{borrow::Cow, collections::HashMap};

    // ── Mock LLM clients ──────────────────────────────────────────────────────

    /// Returns a fixed summary string — simulates a successful LLM summarisation call.
    struct MockLlmClient {
        response: String,
    }

    impl MockLlmClient {
        fn new(response: impl Into<String>) -> Self {
            Self {
                response: response.into(),
            }
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

    struct SchemaTool;

    #[async_trait]
    impl Tool for SchemaTool {
        fn name(&self) -> &str {
            "schema_tool"
        }

        fn description(&self) -> &str {
            "A tool used to validate schema-correction envelopes."
        }

        fn description_minimal(&self) -> Cow<'_, str> {
            Cow::Borrowed("validate schema correction")
        }

        fn input_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["fast", "safe"],
                        "description": "Run mode."
                    }
                },
                "required": ["path", "mode"],
                "additionalProperties": false
            })
        }

        async fn call(
            &self,
            _input: Value,
            _ctx: &crate::agent::tool::ToolContext,
        ) -> Result<crate::agent::tool::ToolResult> {
            unreachable!("schema helper tool should not be executed in unit tests");
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
                    input: serde_json::from_str(input_json).unwrap_or(serde_json::Value::Null),
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
            let tool_names = [
                "shell",
                "file_read",
                "com_invoke",
                "plan_todo",
                "file_write",
            ];
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
                _ => format!(
                    r#"{{"path":"C:\\output\\result_{}.txt","content":"done"}}"#,
                    i
                ),
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
            msgs.push(make_tool_result_msg(
                &format!("call_{}", tool_name),
                &result_content,
            ));
        }
        msgs.push(make_text_msg(
            "assistant",
            "配件移动完成。已将管道支撑从(100,200,300)成功移动到(150,250,350)。",
        ));
        msgs
    }

    #[test]
    fn schema_error_classifier_matches_structural_errors_only() {
        assert!(is_structural_schema_error("missing field `path`"));
        assert!(is_structural_schema_error(
            "invalid type: integer `1`, expected a string"
        ));
        assert!(is_structural_schema_error(
            "unknown field `extra`, expected one of `path`, `mode`"
        ));
        assert!(!is_structural_schema_error("permission denied"));
        assert!(!is_structural_schema_error("exit code 1"));
    }

    #[test]
    fn schema_correction_envelope_includes_full_schema_json() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(SchemaTool));

        let envelope =
            maybe_schema_correction_envelope(&registry, "schema_tool", "missing field `mode`")
                .expect("structural schema error should produce envelope");

        assert!(envelope.starts_with("[schema_correction tool=schema_tool]\n"));
        assert!(envelope.contains("\"required\":[\"path\",\"mode\"]"));
        assert!(envelope.contains("\"additionalProperties\":false"));
        assert!(envelope.ends_with("\n[/schema_correction]"));
    }

    #[test]
    fn schema_correction_envelope_skips_non_structural_and_unknown_tool() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(SchemaTool));

        assert!(
            maybe_schema_correction_envelope(&registry, "schema_tool", "permission denied")
                .is_none()
        );
        assert!(maybe_schema_correction_envelope(
            &registry,
            "missing_tool",
            "missing field `path`"
        )
        .is_none());
    }

    #[test]
    fn schema_correction_envelope_survives_tool_result_roundtrip() {
        let content = "[schema_correction tool=schema_tool]\n{\"type\":\"object\",\"required\":[\"path\"]}\n[/schema_correction]";
        let block = ContentBlock::ToolResult {
            tool_use_id: "call_schema_tool".to_string(),
            content: content.to_string(),
            is_error: true,
        };
        let json = serde_json::to_string(&block).expect("tool_result should serialize");
        let decoded: ContentBlock =
            serde_json::from_str(&json).expect("tool_result should deserialize");
        match decoded {
            ContentBlock::ToolResult {
                content: restored, ..
            } => assert_eq!(restored, content),
            other => panic!("unexpected block: {other:?}"),
        }
    }

    // ── T1: Level-1 — small result not trimmed ────────────────────────────────

    #[test]
    fn t1_small_tool_result_not_trimmed() {
        let original = "x".repeat(1_000);
        let mut msgs = vec![make_tool_result_msg("call_1", &original)];
        let changed = compact_trim_tool_results(&mut msgs, 50_000);
        assert!(
            !changed,
            "should not trim a 1000-char result with limit=50000"
        );
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
        assert!(
            !changed,
            "assistant ToolUse should never be trimmed by Level-1"
        );
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
        assert!(
            changed,
            "should report change because large result was trimmed"
        );

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
                assert!(
                    content.contains("chars removed"),
                    "large result should be trimmed"
                );
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
        let result = compact_summarise(msgs, 100_000, &client, "test-model", 1024, None).await;
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
        let result = compact_summarise(msgs, 100_000, &client, "test-model", 1024, None).await;
        assert!(
            result.is_none(),
            "all messages fit in budget, should return None"
        );
    }

    // ── T7: Level-2 — plain text messages compacted correctly ────────────────

    #[tokio::test]
    async fn t7_plain_text_messages_compacted() {
        let client =
            MockLlmClient::new("用户要求[移动配件]，智能体已完成[查询位置]，当前状态[待执行移动]");
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
        let result =
            compact_summarise(msgs.clone(), 2_000, &client, "test-model", 1024, None).await;
        assert!(
            result.is_some(),
            "should compact when messages exceed keep_chars"
        );

        let compacted = result.unwrap();
        assert!(
            compacted.messages.len() < msgs.len(),
            "compacted messages ({}) should be fewer than original ({})",
            compacted.messages.len(),
            msgs.len()
        );

        // First message should be the summary
        let first_content = compacted.messages[0].content.as_text();
        assert!(
            first_content.contains("[会话滚动摘要]"),
            "first message should contain [会话滚动摘要], got: {}",
            &first_content[..first_content.len().min(100)]
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
        let result = compact_summarise(msgs, 5_000, &client, "deepseek-chat", 4096, None).await;
        assert!(result.is_some(), "30-round session should be compacted");

        let compacted = result.unwrap();
        assert!(
            compacted.messages.len() < original_len,
            "compacted ({}) should be fewer than original ({})",
            compacted.messages.len(),
            original_len
        );

        // Summary message should contain tool names (not empty)
        let summary_msg = &compacted.messages[0];
        let summary_content = summary_msg.content.as_text();
        assert!(
            summary_content.contains("[会话滚动摘要]"),
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
        let result = compact_summarise(msgs, 1_000, &client, "test-model", 1024, None).await;
        assert!(
            result.is_none(),
            "LLM failure should return None from compact_summarise"
        );
    }

    #[tokio::test]
    async fn t9b_merges_existing_rolling_summary() {
        let client = MockLlmClient::new(
            "用户目标[整理上下文]；已完成工作[合并旧摘要与新历史]；当前状态[继续执行]；关键结果[src-tauri/src/agent/loop_.rs]",
        );
        let msgs = make_realistic_session(12);
        let result = compact_summarise(
            msgs,
            1_500,
            &client,
            "test-model",
            1024,
            Some("用户目标[旧目标]；已完成工作[旧工作]；当前状态[旧状态]"),
        )
        .await
        .expect("merged compaction");

        assert!(result.summary.contains("合并旧摘要与新历史"));
        assert!(result.messages[0]
            .content
            .as_text()
            .contains("[会话滚动摘要]"));
    }

    // ── T10: estimate_message_tokens handles all content types ───────────────

    #[test]
    fn t10_estimate_message_tokens_all_types() {
        use crate::llm::estimate_message_tokens;

        // Plain text
        let text_msg = make_text_msg("user", &"a".repeat(400));
        let text_tokens = estimate_message_tokens(&text_msg);
        assert!(
            text_tokens > 0,
            "plain text should have non-zero token estimate"
        );
        // 400 ASCII chars ÷ 4 = 100 tokens (max(1) applies)
        assert!(
            (90..=110).contains(&text_tokens),
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
        // deepseek-chat, max_tokens=4096 → budget=49000, keep_tokens=29400
        let msgs = make_realistic_session(76); // 76 rounds ≈ 154 messages
        let original_len = msgs.len();

        // keep_tokens matching the real crash: budget(49000) × 0.60 = 29400
        let keep_tokens = 29_400usize;
        let result =
            compact_summarise(msgs, keep_tokens, &client, "deepseek-chat", 4096, None).await;

        assert!(result.is_some(), "154-message session should be compacted");
        let compacted = result.unwrap();

        // Key regression check: must keep more than just 2 tail messages.
        // Before the fix, split_idx defaulted to len-2, leaving only 3 messages total.
        // After the fix, split_idx is computed from actual content sizes.
        let tail_count = compacted.messages.len() - 1; // subtract the summary message
        assert!(
            tail_count >= 6,
            "should keep at least 6 tail messages (3 tool rounds), got {} tail + 1 summary = {} total (original={})",
            tail_count,
            compacted.messages.len(),
            original_len
        );

        // Summary should be first
        assert!(
            compacted.messages[0]
                .content
                .as_text()
                .contains("[会话滚动摘要]"),
            "first message should be summary"
        );
    }

    // ── Phase C: build_request_messages ───────────────────────────────────────

    fn user_text(text: &str) -> LlmMessage {
        LlmMessage {
            role: "user".into(),
            content: MessageContent::text(text),
        }
    }

    fn tool_result_carrier(id: &str, content: &str) -> LlmMessage {
        LlmMessage {
            role: "user".into(),
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: content.to_string(),
                is_error: false,
            }]),
        }
    }

    fn assistant_text(text: &str) -> LlmMessage {
        LlmMessage {
            role: "assistant".into(),
            content: MessageContent::text(text),
        }
    }

    #[test]
    fn build_request_messages_keeps_recent_turns_full() {
        // p5 two-boundary contract: with 10 short turns (one tool_result per
        // turn) both the turn-count boundary and the tool-carrier boundary
        // kick in. The effective cutoff is `min(turn, tool)`:
        //   * turn cutoff (keep 3 turns)          → index of turn 8's user
        //   * tool cutoff (keep 8 carriers)       → index of turn 3's carrier
        // so `min` = tool cutoff, and turns 1..=2's tool results get demoted
        // while turns 3..=10 stay full (tool-carrier boundary wins, as the
        // in-flight long-running work should).
        let big = "X".repeat(5_000);
        let mut messages = Vec::new();
        for turn in 1..=10 {
            messages.push(user_text(&format!("请求 {}", turn)));
            messages.push(assistant_text(&format!("assistant {}", turn)));
            messages.push(tool_result_carrier(&format!("call-{}", turn), &big));
        }
        let mut minimals = HashMap::new();
        for turn in 1..=10 {
            minimals.insert(format!("call-{}", turn), format!("receipt {}", turn));
        }
        let req = build_request_messages(
            &messages,
            &minimals,
            CTX_PRESERVE_RECENT_TURNS,
            CTX_KEEP_RECENT_TOOL_CARRIERS,
        );
        assert_eq!(req.len(), messages.len());

        // Turns 1 and 2 (idx 2 and 5) demoted — outside both boundaries.
        // p11: demoted receipts now carry a `[recall:<tool_use_id>]` suffix
        // so the agent can re-fetch the full content via recall_tool_result.
        for (idx, turn) in [(2usize, 1), (5, 2)] {
            if let MessageContent::Blocks(ref b) = req[idx].content {
                if let ContentBlock::ToolResult { content, .. } = &b[0] {
                    let expected = format!("receipt {} [recall:call-{}]", turn, turn);
                    assert_eq!(
                        content, &expected,
                        "turn {} (idx {}) should be demoted with recall hint",
                        turn, idx
                    );
                    continue;
                }
            }
            panic!("expected ToolResult at idx {}", idx);
        }

        // Turns 3..=10 preserved (within tool-carrier window of 8).
        for (idx, _turn) in (8..30).step_by(3).zip(3..=10) {
            if let MessageContent::Blocks(ref b) = req[idx].content {
                if let ContentBlock::ToolResult { content, .. } = &b[0] {
                    assert_eq!(
                        content.chars().count(),
                        5_000,
                        "tool at idx {} should stay full",
                        idx
                    );
                }
            }
        }
    }

    #[test]
    fn build_request_messages_tool_carrier_boundary_protects_single_long_turn() {
        // Single user turn, 12 tool-call iterations. CTX_KEEP_RECENT_TOOL_CARRIERS
        // is 8, but CTX_PRESERVE_RECENT_TURNS boundary alone would be 0
        // (only 1 user text boundary exists), so `min(0, tool_cutoff)` = 0
        // → nothing is demoted. This protects long autonomous workflows.
        let big = "Y".repeat(3_000);
        let mut messages = Vec::new();
        messages.push(user_text("开始长任务"));
        for i in 1..=12 {
            messages.push(assistant_text(&format!("iter {}", i)));
            messages.push(tool_result_carrier(&format!("call-{}", i), &big));
        }
        let mut minimals = HashMap::new();
        for i in 1..=12 {
            minimals.insert(format!("call-{}", i), format!("receipt {}", i));
        }
        let req = build_request_messages(
            &messages,
            &minimals,
            CTX_PRESERVE_RECENT_TURNS,
            CTX_KEEP_RECENT_TOOL_CARRIERS,
        );
        for i in 1..=12 {
            let idx = i * 2; // tool_result of iter i
            if let MessageContent::Blocks(ref b) = req[idx].content {
                if let ContentBlock::ToolResult { content, .. } = &b[0] {
                    assert_eq!(
                        content.chars().count(),
                        3_000,
                        "iter {} must stay full (single-turn protection)",
                        i
                    );
                }
            }
        }
    }

    #[test]
    fn build_request_messages_snaps_boundary_off_tool_use_result_pair() {
        // Construct: [user_text, assistant_with_tool_use, tool_result] × 10.
        // The raw boundary could fall on a `tool_result` carrier (a user
        // message whose only blocks are ToolResult). `snap_to_pair_boundary`
        // must step back over the preceding assistant `ToolUse` so the pair
        // is kept intact when it crosses the boundary.
        let big = "Z".repeat(2_000);
        let mut messages = Vec::new();
        for turn in 1..=10 {
            messages.push(user_text(&format!("Q{}", turn)));
            let tool_use_block = ContentBlock::ToolUse {
                id: format!("call-{}", turn),
                name: "shell".into(),
                input: serde_json::json!({"command": "echo"}),
            };
            messages.push(LlmMessage {
                role: "assistant".into(),
                content: MessageContent::Blocks(vec![
                    ContentBlock::Text {
                        text: format!("thinking {}", turn),
                    },
                    tool_use_block,
                ]),
            });
            messages.push(tool_result_carrier(&format!("call-{}", turn), &big));
        }
        let mut minimals = HashMap::new();
        for turn in 1..=10 {
            minimals.insert(format!("call-{}", turn), format!("r{}", turn));
        }
        let req = build_request_messages(
            &messages,
            &minimals,
            CTX_PRESERVE_RECENT_TURNS,
            CTX_KEEP_RECENT_TOOL_CARRIERS,
        );
        // For every assistant ToolUse that ended up retained with full
        // content, the matching ToolResult must also be retained. The
        // pair-boundary snap is what guarantees this.
        for i in 0..req.len() {
            if let MessageContent::Blocks(blocks) = &req[i].content {
                for b in blocks {
                    if let ContentBlock::ToolUse { id, .. } = b {
                        // Find matching tool_result in req.
                        let found = req.iter().any(|m| {
                            if let MessageContent::Blocks(bs) = &m.content {
                                bs.iter().any(|bb| {
                                    matches!(bb, ContentBlock::ToolResult { tool_use_id, .. }
                                             if tool_use_id == id)
                                })
                            } else {
                                false
                            }
                        });
                        assert!(found, "ToolUse {id} at idx {i} lost its ToolResult pair");
                    }
                }
            }
        }
    }

    #[test]
    fn build_request_messages_keeps_all_when_fewer_turns_than_window() {
        // With only 3 user turns and recent_full_turns=3, NOTHING should be
        // demoted — the whole session fits inside the recent window.
        let big = "X".repeat(5_000);
        let mut messages = Vec::new();
        for turn in 1..=3 {
            messages.push(user_text(&format!("请求 {}", turn)));
            messages.push(assistant_text(&format!("assistant {}", turn)));
            messages.push(tool_result_carrier(&format!("call-{}", turn), &big));
        }
        let mut minimals = HashMap::new();
        for turn in 1..=3 {
            minimals.insert(format!("call-{}", turn), format!("receipt {}", turn));
        }
        let req = build_request_messages(
            &messages,
            &minimals,
            CTX_PRESERVE_RECENT_TURNS,
            CTX_KEEP_RECENT_TOOL_CARRIERS,
        );
        for idx in [2usize, 5, 8] {
            if let MessageContent::Blocks(ref b) = req[idx].content {
                if let ContentBlock::ToolResult { content, .. } = &b[0] {
                    assert_eq!(
                        content.chars().count(),
                        5_000,
                        "turn at {} must stay full when total turns <= window",
                        idx
                    );
                }
            } else {
                panic!("expected Blocks at idx {}", idx);
            }
        }
    }

    #[test]
    fn build_request_messages_without_minimal_keeps_full() {
        // If the side-map has no entry for a tool_use_id, build_request_messages
        // must leave the full content intact rather than blanking it out.
        let msgs = vec![
            user_text("第 1 轮"),
            assistant_text("ok1"),
            tool_result_carrier("call-1", "LONG 1"),
            user_text("第 2 轮"),
            assistant_text("ok2"),
            tool_result_carrier("call-2", "LONG 2"),
            user_text("第 3 轮"),
            assistant_text("ok3"),
            tool_result_carrier("call-3", "LONG 3"),
            user_text("第 4 轮"),
            assistant_text("ok4"),
            tool_result_carrier("call-4", "LONG 4"),
        ];
        let minimals = HashMap::new(); // empty
        let req = build_request_messages(
            &msgs,
            &minimals,
            CTX_PRESERVE_RECENT_TURNS,
            CTX_KEEP_RECENT_TOOL_CARRIERS,
        );
        if let MessageContent::Blocks(ref b) = req[2].content {
            if let ContentBlock::ToolResult { content, .. } = &b[0] {
                assert_eq!(content, "LONG 1");
            }
        }
    }

    #[test]
    fn serialize_tool_results_with_receipts_injects_fields() {
        let blocks = [ContentBlock::ToolResult {
            tool_use_id: "call-1".into(),
            content: "full content".into(),
            is_error: false,
        }];
        let refs: Vec<&ContentBlock> = blocks.iter().collect();
        let mut minimals = HashMap::new();
        minimals.insert("call-1".to_string(), "receipt-1".to_string());
        let mut names = HashMap::new();
        names.insert("call-1".to_string(), "shell".to_string());
        let json = serialize_tool_results_with_receipts(&refs, Some(&minimals), Some(&names));
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v[0]["content_minimal"], "receipt-1");
        assert_eq!(v[0]["tool_name"], "shell");
        // Legacy fields preserved
        assert_eq!(v[0]["content"], "full content");
        assert_eq!(v[0]["tool_use_id"], "call-1");
    }

    /// Spy client that records the last prompt it saw, so we can assert that
    /// Level-2 summarisation receives the FULL content (not demoted minimal).
    struct SpyClient {
        last_prompt: std::sync::Mutex<String>,
        response: String,
    }

    #[async_trait]
    impl crate::llm::LlmClient for SpyClient {
        async fn stream(
            &self,
            _req: LlmRequest,
            _tx: tokio::sync::mpsc::Sender<LlmChunk>,
        ) -> Result<()> {
            Ok(())
        }

        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse> {
            let joined = req
                .messages
                .iter()
                .map(|m| m.content.as_text())
                .collect::<Vec<_>>()
                .join("\n---\n");
            *self.last_prompt.lock().unwrap() = joined;
            Ok(LlmResponse {
                content: self.response.clone(),
                tool_calls: vec![],
                input_tokens: 123,
                output_tokens: 45,
            })
        }
    }

    #[tokio::test]
    async fn compact_summarise_uses_full_tool_result_content() {
        // Build a history where tool results carry distinctive full content.
        // After summarisation we assert the prompt contained that full text
        // (proof that the caller passed FULL, not the minimal receipt).
        let unique_marker = "FULL_CONTENT_MARKER_q7z9_long_tool_output";
        let big_content = format!("{} {}", unique_marker, "y".repeat(2000));
        let mut msgs = Vec::new();
        for turn in 1..=8 {
            msgs.push(user_text(&format!("请求 {}", turn)));
            msgs.push(assistant_text(&format!("回答 {}", turn)));
            msgs.push(tool_result_carrier(&format!("c{}", turn), &big_content));
        }
        let client = SpyClient {
            last_prompt: std::sync::Mutex::new(String::new()),
            response: "rolling summary output".into(),
        };
        // keep_tokens small enough to force summarisation of older turns.
        let result = compact_summarise(msgs, 500, &client, "test-model", 4096, None).await;
        assert!(result.is_some(), "summarise should have run");
        let prompt = client.last_prompt.lock().unwrap().clone();
        assert!(
            prompt.contains(unique_marker),
            "summariser prompt should include the full tool-result content"
        );
    }
}
