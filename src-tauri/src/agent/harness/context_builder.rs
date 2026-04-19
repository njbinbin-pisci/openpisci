//! `ContextBuilder::finalize` — the single entry point that every agent
//! request must pass through before hitting the LLM.
//!
//! Execution order mirrors hermes-agent's `_preprocess_messages` and
//! claw-compactor's `ContextFinalizer`:
//!
//!   1. **Receipt demotion** — old tool results swap to their minimal
//!      version (delegates to [`crate::agent::loop_::build_request_messages`]).
//!   2. **Supersede filter** — collapse failed tool attempts whose retry
//!      succeeded (delegates to
//!      [`crate::commands::chat::collapse_superseded_tool_failures`]).
//!   3. **Tool pairing fixup** — trim trailing orphan tool_use blocks +
//!      mid-history orphan tool_use/tool_result pairs.
//!   4. **Layered token estimation** — fill [`LayeredTokenBreakdown`] and
//!      classify the tier via [`LayeredBudget`].
//!
//! p0 wires this together and proves the ordering via tests. p1 calls it
//! from the real request path; p5 / p5a / p6 / p11 add new transformation
//! stages *inside* this hook so the call sites never need to change again.

use std::collections::HashMap;

use crate::llm::{
    estimate_message_tokens, estimate_request_overhead_tokens, estimate_tool_def_tokens, ContentBlock,
    LlmMessage, MessageContent, ToolDef,
};

use super::{CompactionTier, LayeredBudget, LayeredPrompt, LayeredPromptTokens};

/// Per-layer token breakdown reported to telemetry (p8) and to the UI
/// context-ring (p8 frontend).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayeredTokenBreakdown {
    /// L0..Lhint — all system-prompt layers.
    pub prompt: LayeredPromptTokens,
    /// L4 — tool definitions (after minimal/full selection).
    pub tool_def_tokens: u32,
    /// L5a — history non-tool text and tool_use arguments.
    pub history_text_tokens: u32,
    /// L5b — history tool_result blocks currently serialised as-is
    /// (full version).
    pub history_tool_result_full_tokens: u32,
    /// L5c — history tool_result blocks serialised as minimal receipts.
    pub history_tool_result_receipt_tokens: u32,
    /// L5d — injected rolling summary (from p7); user-role synthetic
    /// message token weight.
    pub rolling_summary_tokens: u32,
    /// L5e — injected state-frame synthetic message (from p6).
    pub state_frame_tokens: u32,
    /// L6 — image / vision blocks.
    pub vision_tokens: u32,
    /// Request-level framing and provider metadata.
    pub request_overhead_tokens: u32,
}

impl LayeredTokenBreakdown {
    pub fn total(&self) -> u32 {
        self.prompt
            .total()
            .saturating_add(self.tool_def_tokens)
            .saturating_add(self.history_text_tokens)
            .saturating_add(self.history_tool_result_full_tokens)
            .saturating_add(self.history_tool_result_receipt_tokens)
            .saturating_add(self.rolling_summary_tokens)
            .saturating_add(self.state_frame_tokens)
            .saturating_add(self.vision_tokens)
            .saturating_add(self.request_overhead_tokens)
    }
}

/// Output of [`ContextBuilder::finalize`].
#[derive(Debug)]
pub struct FinalizedRequest {
    /// Ready-to-send messages (receipt-demoted + supersede-collapsed +
    /// tool-pairing-sanitised).
    pub messages: Vec<LlmMessage>,
    /// System prompt rendered from the layered prompt.
    pub system_prompt: String,
    /// Token breakdown across layers.
    pub breakdown: LayeredTokenBreakdown,
    /// Which tier this request falls into given the budget.
    pub tier: CompactionTier,
    /// Number of messages dropped by the sanitisers (for telemetry).
    pub messages_dropped: usize,
    /// Whether at least one tool result was demoted to its receipt form.
    pub demoted_any: bool,
    /// Phase 3 (schema-correction close-loop): concise error
    /// descriptions recovered from failure-then-success chains
    /// involving the **same tool** but different signatures (i.e. the
    /// full-schema nudge worked). Callers with access to the rolling
    /// summary should promote these into `errors_learned` so the
    /// knowledge survives the next compaction cycle.
    pub recovered_errors: Vec<String>,
}

/// Builder for finalising a request's in-memory message log.
pub struct ContextBuilder<'a> {
    messages: Vec<LlmMessage>,
    tool_minimals: &'a HashMap<String, String>,
    recent_full_turns: usize,
    recent_tool_carriers: usize,
    layered_prompt: &'a LayeredPrompt,
    tools: &'a [ToolDef],
    budget: LayeredBudget,
    rolling_summary_tokens_hint: u32,
    state_frame_tokens_hint: u32,
}

impl<'a> ContextBuilder<'a> {
    /// Start a builder. Take ownership of `messages` so we can mutate
    /// the vector in-place during sanitisation steps without cloning.
    pub fn new(
        messages: Vec<LlmMessage>,
        tool_minimals: &'a HashMap<String, String>,
        layered_prompt: &'a LayeredPrompt,
        tools: &'a [ToolDef],
        budget: LayeredBudget,
    ) -> Self {
        Self {
            messages,
            tool_minimals,
            recent_full_turns: crate::agent::compaction::CTX_PRESERVE_RECENT_TURNS,
            recent_tool_carriers: crate::agent::compaction::CTX_KEEP_RECENT_TOOL_CARRIERS,
            layered_prompt,
            tools,
            budget,
            rolling_summary_tokens_hint: 0,
            state_frame_tokens_hint: 0,
        }
    }

    pub fn with_recent_full_turns(mut self, n: usize) -> Self {
        self.recent_full_turns = n;
        self
    }

    /// Override the independent tool-carrier boundary (p5). Kept separate
    /// from `with_recent_full_turns` so callers / tests can exercise each
    /// boundary in isolation, reflecting the `min(turn, tool)` contract.
    pub fn with_recent_tool_carriers(mut self, n: usize) -> Self {
        self.recent_tool_carriers = n;
        self
    }

    /// Declare an already-injected rolling summary so its tokens are
    /// attributed to `rolling_summary_tokens` rather than
    /// `history_text_tokens`. p7 will call this.
    pub fn with_rolling_summary_tokens(mut self, n: u32) -> Self {
        self.rolling_summary_tokens_hint = n;
        self
    }

    /// Declare an already-injected state frame. p6 will call this.
    pub fn with_state_frame_tokens(mut self, n: u32) -> Self {
        self.state_frame_tokens_hint = n;
        self
    }

    /// Run the full pipeline. Consumes the builder.
    pub fn finalize(self) -> FinalizedRequest {
        let ContextBuilder {
            messages,
            tool_minimals,
            recent_full_turns,
            recent_tool_carriers,
            layered_prompt,
            tools,
            budget,
            rolling_summary_tokens_hint,
            state_frame_tokens_hint,
        } = self;

        let msgs_in = messages.len();

        // Step 1: receipt demotion (two-boundary scheme, see p5).
        let demoted = crate::agent::loop_::build_request_messages(
            &messages,
            tool_minimals,
            recent_full_turns,
            recent_tool_carriers,
        );
        let demoted_any = demoted_changed(&messages, &demoted);

        // Step 2: supersede filter.
        //
        // Run the schema-recovery collector BEFORE collapsing: we need
        // to see the failed + successful pair in the same vector so we
        // can attribute the failure to the right tool. After collapse
        // the failures are gone.
        let recovered_errors = collect_recovered_schema_errors(&demoted);
        let superseded_free = crate::commands::chat::collapse_superseded_tool_failures(demoted);

        // Step 3: tool pairing fixup.
        //
        // We intentionally run ONLY `sanitize_tool_use_result_pairing`
        // here, not the DB-reload `sanitize_tool_call_pairs`. The
        // in-memory agent loop ends each iteration with a user-role
        // tool_result (the output of the tool it just ran); the
        // trailing-orphan sanitiser is designed for a different shape
        // (DB-restored sessions that end with a user-text question) and
        // would incorrectly drop the last properly-paired tool_result
        // when the list ends with one. `sanitize_tool_use_result_pairing`
        // is safe for both cases because it walks forward and only
        // strips assistant-with-ToolUse that lacks a following
        // tool_result — which never happens mid-iteration.
        let sanitized = crate::commands::chat::sanitize_tool_use_result_pairing(superseded_free);

        let msgs_dropped = msgs_in.saturating_sub(sanitized.len());

        // Step 4: layered token estimation + tier classification.
        let system_prompt = layered_prompt.render();
        let prompt_breakdown = layered_prompt.token_breakdown();
        let tool_def_tokens = tools.iter().map(estimate_tool_def_tokens).sum::<usize>() as u32;
        let request_overhead =
            estimate_request_overhead_tokens(Some(&system_prompt), tools) as u32
                - prompt_breakdown.total()
                - tool_def_tokens;

        let history_breakdown =
            split_history_tokens(&sanitized, tool_minimals);

        let breakdown = LayeredTokenBreakdown {
            prompt: prompt_breakdown,
            tool_def_tokens,
            history_text_tokens: history_breakdown
                .text
                .saturating_sub(rolling_summary_tokens_hint.min(history_breakdown.text))
                .saturating_sub(state_frame_tokens_hint.min(history_breakdown.text)),
            history_tool_result_full_tokens: history_breakdown.tool_result_full,
            history_tool_result_receipt_tokens: history_breakdown.tool_result_receipt,
            rolling_summary_tokens: rolling_summary_tokens_hint,
            state_frame_tokens: state_frame_tokens_hint,
            vision_tokens: history_breakdown.vision,
            request_overhead_tokens: request_overhead,
        };

        let tier = budget.classify(breakdown.total());

        FinalizedRequest {
            messages: sanitized,
            system_prompt,
            breakdown,
            tier,
            messages_dropped: msgs_dropped,
            demoted_any,
            recovered_errors,
        }
    }
}

/// Phase 3 — scan a message vector for `tool_use_X (fail, is_error=true)`
/// followed by `tool_use_Y (same tool name, success)`, and emit a
/// short description of the error for each such pair.
///
/// Why this exists
/// ----------------
/// When a tool call fails because the LLM called it with a bad
/// signature, Pisci's harness swaps to the **full** schema for the
/// next turn. Once the LLM succeeds with the corrected signature, we
/// can safely drop the failed leg (see
/// [`crate::commands::chat::collapse_superseded_tool_failures`]). But
/// the *lesson* — "arg X needs Y" — should not vanish: we promote it
/// into `errors_learned` of the structured rolling summary so future
/// compaction cycles retain the knowledge.
///
/// This matches the FEC view: once a packet is successfully retransmitted,
/// the lost one is dropped, but the error correction metadata (what went
/// wrong) is retained in the dictionary.
///
/// Returns concise strings like `"tool=file_write: unknown key 'pth' —
/// use 'path' instead"`. Deduplicates identical lessons. Bounded.
fn collect_recovered_schema_errors(msgs: &[LlmMessage]) -> Vec<String> {
    use std::collections::HashMap;

    const MAX_ERRORS: usize = 10;
    const MAX_ERROR_LEN: usize = 200;

    // Index tool_use_id -> (tool_name, error_content, pos).
    let mut failures: HashMap<String, (String, String, usize)> = HashMap::new();
    let mut tool_use_name: HashMap<String, String> = HashMap::new();
    let mut successes_by_tool: HashMap<String, Vec<usize>> = HashMap::new();

    for (pos, m) in msgs.iter().enumerate() {
        if let MessageContent::Blocks(blocks) = &m.content {
            for b in blocks {
                match b {
                    ContentBlock::ToolUse { id, name, .. } => {
                        tool_use_name.insert(id.clone(), name.clone());
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        let Some(name) = tool_use_name.get(tool_use_id).cloned() else {
                            continue;
                        };
                        if *is_error {
                            failures.insert(
                                tool_use_id.clone(),
                                (name, content.clone(), pos),
                            );
                        } else {
                            successes_by_tool
                                .entry(name)
                                .or_default()
                                .push(pos);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (_id, (name, error, fail_pos)) in &failures {
        // Was there a later success using the same tool?
        let has_later_success = successes_by_tool
            .get(name)
            .map(|ps| ps.iter().any(|p| *p > *fail_pos))
            .unwrap_or(false);
        if !has_later_success {
            continue;
        }
        let snippet = condense_error_snippet(error, MAX_ERROR_LEN);
        let lesson = format!("tool={}: {}", name, snippet);
        if seen.insert(lesson.clone()) {
            out.push(lesson);
            if out.len() >= MAX_ERRORS {
                break;
            }
        }
    }
    out.sort();
    out
}

/// Extract the most informative leading part of an error message,
/// capped at `max_len` characters. Strips stack-trace tails.
fn condense_error_snippet(error: &str, max_len: usize) -> String {
    let trimmed = error.trim();
    // Prefer the first non-empty line — typically the human-readable
    // message before stack trace / backtrace details.
    let first_line = trimmed
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let truncated: String = first_line.chars().take(max_len).collect();
    if truncated.chars().count() < first_line.chars().count() {
        format!("{}…", truncated)
    } else {
        truncated
    }
}

fn demoted_changed(before: &[LlmMessage], after: &[LlmMessage]) -> bool {
    // Cheap structural diff: compare total serialised ToolResult character
    // counts. If we lost characters, something was demoted.
    let count_tr_chars = |msgs: &[LlmMessage]| -> usize {
        msgs.iter()
            .flat_map(|m| match &m.content {
                MessageContent::Blocks(b) => b.iter().collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .filter_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.len()),
                _ => None,
            })
            .sum()
    };
    count_tr_chars(after) < count_tr_chars(before)
}

struct HistoryTokenSplit {
    text: u32,
    tool_result_full: u32,
    tool_result_receipt: u32,
    vision: u32,
}

fn split_history_tokens(
    msgs: &[LlmMessage],
    tool_minimals: &HashMap<String, String>,
) -> HistoryTokenSplit {
    let mut text: u32 = 0;
    let mut tool_result_full: u32 = 0;
    let mut tool_result_receipt: u32 = 0;
    let mut vision: u32 = 0;

    for m in msgs {
        // Per-message framing overhead is attributed to text.
        const MSG_OVERHEAD: u32 = 8;
        text = text.saturating_add(MSG_OVERHEAD);
        match &m.content {
            MessageContent::Text(t) => {
                text = text.saturating_add(crate::llm::estimate_tokens(t) as u32);
            }
            MessageContent::Blocks(blocks) => {
                for b in blocks {
                    match b {
                        ContentBlock::Text { text: t } => {
                            text = text.saturating_add(crate::llm::estimate_tokens(t) as u32);
                        }
                        ContentBlock::ToolUse { name, input, .. } => {
                            text = text.saturating_add(
                                (8 + crate::llm::estimate_tokens(name)
                                    + crate::llm::estimate_tokens(&input.to_string()))
                                    as u32,
                            );
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            let tok = (4 + crate::llm::estimate_tokens(content)) as u32;
                            // p11: a demoted receipt may have a
                            // `[recall:<tool_use_id>]` suffix appended, so
                            // compare using the suffix-aware helper rather
                            // than plain equality.
                            let is_receipt = tool_minimals
                                .get(tool_use_id)
                                .map(|m| is_demoted_receipt_match(m, content, tool_use_id))
                                .unwrap_or(false);
                            if is_receipt {
                                tool_result_receipt = tool_result_receipt.saturating_add(tok);
                            } else {
                                tool_result_full = tool_result_full.saturating_add(tok);
                            }
                        }
                        ContentBlock::Image { .. } => {
                            vision = vision.saturating_add(256);
                        }
                    }
                }
            }
        }
    }

    HistoryTokenSplit {
        text,
        tool_result_full,
        tool_result_receipt,
        vision,
    }
}

/// Convenience for callers that want to know the estimated total without
/// re-running the full finalise pipeline.
pub fn estimate_total(breakdown: &LayeredTokenBreakdown) -> u32 {
    breakdown.total()
}

/// p8 — pure helper that computes a [`LayeredTokenBreakdown`] for an
/// already-built request view (messages, system prompt, tools) without
/// running the finalise pipeline. Used by the agent loop telemetry path
/// where demotion / supersede / sanitisation has already happened and we
/// only want the per-layer attribution for the UI ring.
///
/// `rolling_summary_tokens` and `state_frame_tokens` are attributed
/// separately to their own layers; when passed, those tokens are *also*
/// subtracted from the history_text bucket so the layers never
/// double-count.
pub fn compute_layered_breakdown(
    messages: &[LlmMessage],
    system_prompt: Option<&str>,
    tools: &[ToolDef],
    tool_minimals: &HashMap<String, String>,
    rolling_summary_tokens: u32,
    state_frame_tokens: u32,
) -> LayeredTokenBreakdown {
    // For a monolithic system prompt we attribute the whole weight to the
    // `persona` slot. Callers that already keep the prompt layered
    // should prefer `ContextBuilder::finalize` to get the accurate split.
    let persona = system_prompt
        .map(|s| crate::llm::estimate_tokens(s) as u32)
        .unwrap_or(0);
    let prompt = LayeredPromptTokens {
        persona,
        scene: 0,
        memory: 0,
        project: 0,
        platform_hint: 0,
    };

    let tool_def_tokens = tools.iter().map(estimate_tool_def_tokens).sum::<usize>() as u32;
    let request_overhead = estimate_request_overhead_tokens(system_prompt, tools) as u32;
    let request_overhead = request_overhead
        .saturating_sub(persona)
        .saturating_sub(tool_def_tokens);

    let history = split_history_tokens(messages, tool_minimals);
    let history_text = history
        .text
        .saturating_sub(rolling_summary_tokens.min(history.text))
        .saturating_sub(state_frame_tokens.min(history.text));

    LayeredTokenBreakdown {
        prompt,
        tool_def_tokens,
        history_text_tokens: history_text,
        history_tool_result_full_tokens: history.tool_result_full,
        history_tool_result_receipt_tokens: history.tool_result_receipt,
        rolling_summary_tokens,
        state_frame_tokens,
        vision_tokens: history.vision,
        request_overhead_tokens: request_overhead,
    }
}

/// True when `actual_content` equals the minimal receipt, optionally with a
/// `[recall:<tool_use_id>]` suffix appended by p11 `with_recall_hint`. This
/// keeps the telemetry split accurate after the recall hint is injected.
fn is_demoted_receipt_match(minimal: &str, actual_content: &str, tool_use_id: &str) -> bool {
    if minimal == actual_content {
        return true;
    }
    let with_hint = crate::agent::tool_receipt::with_recall_hint(minimal, tool_use_id);
    with_hint == actual_content
}

/// Re-export for parity with [`estimate_message_tokens`] inside this
/// module (kept private to avoid widening the public API surface).
#[allow(dead_code)]
fn estimate_message_tokens_u32(msg: &LlmMessage) -> u32 {
    estimate_message_tokens(msg) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ContentBlock, LlmMessage, MessageContent};
    use serde_json::json;
    use std::sync::Arc;

    fn user_text(s: &str) -> LlmMessage {
        LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Text(s.to_string()),
        }
    }

    fn assistant_tool_use(id: &str, name: &str, input: serde_json::Value) -> LlmMessage {
        LlmMessage {
            role: "assistant".to_string(),
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input,
            }]),
        }
    }

    fn user_tool_result(id: &str, content: &str, is_error: bool) -> LlmMessage {
        LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: content.to_string(),
                is_error,
            }]),
        }
    }

    #[test]
    fn finalize_is_a_no_op_for_simple_chat() {
        let msgs = vec![
            user_text("hello"),
            LlmMessage {
                role: "assistant".to_string(),
                content: MessageContent::Text("hi there".to_string()),
            },
        ];
        let minimals = HashMap::new();
        let lp = LayeredPrompt::from_monolithic("you are pisci");
        let tools: Vec<ToolDef> = vec![];
        let budget = LayeredBudget::with_total(100_000);

        let fin = ContextBuilder::new(msgs, &minimals, &lp, &tools, budget).finalize();
        assert_eq!(fin.messages.len(), 2);
        assert_eq!(fin.messages_dropped, 0);
        assert!(!fin.demoted_any);
        assert_eq!(fin.tier, CompactionTier::None);
        assert!(fin.breakdown.prompt.persona > 0);
        assert!(fin.breakdown.history_text_tokens > 0);
        assert_eq!(fin.breakdown.history_tool_result_full_tokens, 0);
        assert_eq!(fin.breakdown.vision_tokens, 0);
    }

    #[test]
    fn finalize_strips_trailing_orphan_tool_use() {
        let msgs = vec![
            user_text("please run a shell"),
            // Orphan: assistant with ToolUse but no following ToolResult.
            assistant_tool_use("call_1", "shell", json!({"cmd": "ls"})),
        ];
        let minimals = HashMap::new();
        let lp = LayeredPrompt::from_monolithic("system");
        let tools: Vec<ToolDef> = vec![];
        let budget = LayeredBudget::with_total(100_000);

        let fin = ContextBuilder::new(msgs, &minimals, &lp, &tools, budget).finalize();
        // The orphan assistant message should be dropped entirely.
        assert_eq!(fin.messages.len(), 1);
        assert_eq!(fin.messages_dropped, 1);
    }

    #[test]
    fn finalize_demotes_old_tool_results_to_receipts() {
        // 5 user turns with tool calls; only the last CTX_FULL_TURNS keep
        // full content.
        let mut msgs = Vec::new();
        let mut minimals = HashMap::new();
        let long_full_payload = "X".repeat(2_000);
        for i in 0..5 {
            msgs.push(user_text(&format!("turn {i}")));
            let id = format!("call_{i}");
            msgs.push(assistant_tool_use(&id, "shell", json!({"cmd": "pwd"})));
            msgs.push(user_tool_result(&id, &long_full_payload, false));
            minimals.insert(id, "[receipt]".to_string());
        }
        let lp = LayeredPrompt::from_monolithic("system");
        let tools: Vec<ToolDef> = vec![];
        let budget = LayeredBudget::with_total(100_000);

        let fin = ContextBuilder::new(msgs, &minimals, &lp, &tools, budget)
            .with_recent_full_turns(2)
            // Override the tool-carrier boundary as well, otherwise the
            // p5 `min(turn, tool)` rule would leave the whole small
            // session (5 carriers) untouched — that *is* the new
            // production behaviour, but this test exercises the turn
            // boundary specifically.
            .with_recent_tool_carriers(2)
            .finalize();
        assert!(fin.demoted_any, "expected at least one receipt demotion");
        // Oldest turns should have their tool_result content swapped.
        let receipts: Vec<_> = fin
            .messages
            .iter()
            .flat_map(|m| match &m.content {
                MessageContent::Blocks(b) => b.iter().collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .filter_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect();
        // p11: demoted receipts now carry a `[recall:<tool_use_id>]` suffix
        // (e.g. "[receipt] [recall:call_0]") so use `starts_with`.
        assert!(
            receipts.iter().any(|c| c.starts_with("[receipt]")),
            "expected at least one [receipt]: {:?}",
            receipts
        );
    }

    #[test]
    fn finalize_collapses_superseded_failed_retry() {
        // Same tool name + same input: first attempt fails, retry succeeds.
        // `collapse_superseded_tool_failures` must remove the failed pair.
        let msgs = vec![
            user_text("please list"),
            assistant_tool_use("call_1", "shell", json!({"cmd": "ls"})),
            user_tool_result("call_1", "permission denied", true),
            assistant_tool_use("call_2", "shell", json!({"cmd": "ls"})),
            user_tool_result("call_2", "file-a\nfile-b", false),
            LlmMessage {
                role: "assistant".to_string(),
                content: MessageContent::Text("done".to_string()),
            },
        ];
        let minimals = HashMap::new();
        let lp = LayeredPrompt::from_monolithic("system");
        let tools: Vec<ToolDef> = vec![];
        let budget = LayeredBudget::with_total(100_000);

        let fin = ContextBuilder::new(msgs, &minimals, &lp, &tools, budget).finalize();

        // We should see only one ToolUse/ToolResult pair left (the
        // successful retry) and no error blocks.
        let mut tool_uses = 0;
        let mut tool_results = 0;
        let mut error_results = 0;
        for m in &fin.messages {
            if let MessageContent::Blocks(blocks) = &m.content {
                for b in blocks {
                    match b {
                        ContentBlock::ToolUse { .. } => tool_uses += 1,
                        ContentBlock::ToolResult { is_error, .. } => {
                            tool_results += 1;
                            if *is_error {
                                error_results += 1;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        assert_eq!(tool_uses, 1);
        assert_eq!(tool_results, 1);
        assert_eq!(error_results, 0);
    }

    #[test]
    fn finalize_reports_tier_above_budget() {
        // Build enough text history to push us past 60% of a tiny budget.
        let mut msgs = Vec::new();
        let padding = "lorem ipsum ".repeat(400);
        for i in 0..10 {
            msgs.push(user_text(&format!("turn {i} {padding}")));
        }
        let minimals = HashMap::new();
        let lp = LayeredPrompt::from_monolithic("system");
        let tools: Vec<ToolDef> = vec![];
        let budget = LayeredBudget::with_total(5_000);

        let fin = ContextBuilder::new(msgs, &minimals, &lp, &tools, budget).finalize();
        assert!(
            matches!(
                fin.tier,
                CompactionTier::Micro | CompactionTier::Auto | CompactionTier::Full
            ),
            "expected non-None tier, got {:?}",
            fin.tier
        );
    }

    #[test]
    fn finalize_vision_tokens_counted_separately() {
        let msgs = vec![
            user_text("look at this"),
            LlmMessage {
                role: "assistant".to_string(),
                content: MessageContent::Blocks(vec![
                    ContentBlock::Text { text: "ok".to_string() },
                    ContentBlock::Image {
                        source: crate::llm::ImageSource {
                            source_type: "base64".to_string(),
                            media_type: "image/png".to_string(),
                            data: "DEADBEEF".to_string(),
                        },
                    },
                ]),
            },
        ];
        let minimals = HashMap::new();
        let lp = LayeredPrompt::from_monolithic("system");
        let tools: Vec<ToolDef> = vec![];
        let budget = LayeredBudget::with_total(100_000);

        let fin = ContextBuilder::new(msgs, &minimals, &lp, &tools, budget).finalize();
        assert_eq!(fin.breakdown.vision_tokens, 256);
    }

    #[test]
    fn finalize_receipt_bucket_split_is_accurate() {
        // With 1 full turn and 2 older turns, the 2 older results should
        // land in the receipt bucket.
        let mut msgs = Vec::new();
        let mut minimals = HashMap::new();
        let full_payload = "Y".repeat(500);
        for i in 0..3 {
            msgs.push(user_text(&format!("turn {i}")));
            let id = format!("call_{i}");
            msgs.push(assistant_tool_use(&id, "shell", json!({"i": i})));
            msgs.push(user_tool_result(&id, &full_payload, false));
            minimals.insert(id, "[R]".to_string());
        }
        let lp = LayeredPrompt::from_monolithic("system");
        let tools: Vec<ToolDef> = vec![];
        let budget = LayeredBudget::with_total(100_000);

        let fin = ContextBuilder::new(msgs, &minimals, &lp, &tools, budget)
            .with_recent_full_turns(1)
            // Same rationale as finalize_demotes_old_tool_results_to_receipts:
            // shrink the tool-carrier window too so at least one receipt
            // ends up in the demoted bucket even with only 3 carriers.
            .with_recent_tool_carriers(1)
            .finalize();
        assert!(fin.breakdown.history_tool_result_receipt_tokens > 0);
        assert!(fin.breakdown.history_tool_result_full_tokens > 0);
    }

    /// p8 — `compute_layered_breakdown` must attribute system / tools /
    /// history to their own buckets and never double-count layers that
    /// are passed explicitly via the rolling_summary / state_frame
    /// hints.
    #[test]
    fn compute_layered_breakdown_splits_layers() {
        let msgs = vec![
            user_text("hello there"),
            assistant_tool_use("call_a", "shell", json!({"cmd": "ls -la"})),
            user_tool_result("call_a", &"F".repeat(400), false),
        ];
        let tools = vec![ToolDef {
            name: "shell".into(),
            description: "run shell".into(),
            input_schema: json!({"type":"object","properties":{"cmd":{"type":"string"}}}),
        }];
        let minimals = HashMap::new();
        let bd = compute_layered_breakdown(
            &msgs,
            Some("you are pisci"),
            &tools,
            &minimals,
            0,
            0,
        );
        assert!(bd.prompt.persona > 0);
        assert!(bd.tool_def_tokens > 0);
        assert!(bd.history_text_tokens > 0);
        assert!(bd.history_tool_result_full_tokens > 0);
        assert_eq!(bd.rolling_summary_tokens, 0);
        assert_eq!(bd.state_frame_tokens, 0);
    }

    #[test]
    fn compute_layered_breakdown_honours_summary_hint() {
        let msgs = vec![user_text("hi")];
        let tools: Vec<ToolDef> = vec![];
        let minimals = HashMap::new();
        let bd = compute_layered_breakdown(&msgs, Some("sys"), &tools, &minimals, 120, 40);
        assert_eq!(bd.rolling_summary_tokens, 120);
        assert_eq!(bd.state_frame_tokens, 40);
    }

    #[test]
    fn finalize_recovers_schema_errors_after_successful_retry() {
        // file_write fails with an "unknown key 'pth'" error, then
        // succeeds with 'path'. Phase 3 should surface the lesson.
        let msgs = vec![
            user_text("write a file"),
            assistant_tool_use("c1", "file_write", json!({"pth": "a.txt", "content": "x"})),
            user_tool_result("c1", "Error: unknown key 'pth'", true),
            assistant_tool_use("c2", "file_write", json!({"path": "a.txt", "content": "x"})),
            user_tool_result("c2", "ok", false),
        ];
        let minimals = HashMap::new();
        let lp = LayeredPrompt::from_monolithic("system");
        let tools: Vec<ToolDef> = vec![];
        let budget = LayeredBudget::with_total(100_000);

        let fin = ContextBuilder::new(msgs, &minimals, &lp, &tools, budget).finalize();
        assert!(
            !fin.recovered_errors.is_empty(),
            "expected at least one recovered error, got {:?}",
            fin.recovered_errors
        );
        let msg = &fin.recovered_errors[0];
        assert!(msg.contains("file_write"));
        assert!(msg.contains("pth"));
    }

    #[test]
    fn finalize_does_not_recover_when_no_success_follows() {
        // Failure without a follow-up success must NOT be promoted —
        // the lesson hasn't been confirmed yet.
        let msgs = vec![
            user_text("write a file"),
            assistant_tool_use("c1", "file_write", json!({"pth": "a.txt"})),
            user_tool_result("c1", "Error: unknown key 'pth'", true),
        ];
        let minimals = HashMap::new();
        let lp = LayeredPrompt::from_monolithic("system");
        let tools: Vec<ToolDef> = vec![];
        let budget = LayeredBudget::with_total(100_000);

        let fin = ContextBuilder::new(msgs, &minimals, &lp, &tools, budget).finalize();
        assert!(fin.recovered_errors.is_empty());
    }

    #[test]
    fn finalize_recovers_across_same_tool_different_ids() {
        let msgs = vec![
            user_text("run it"),
            assistant_tool_use("c1", "shell", json!({"cmd": "ls /nonexistent"})),
            user_tool_result("c1", "No such file or directory", true),
            assistant_tool_use("c2", "shell", json!({"cmd": "ls /tmp"})),
            user_tool_result("c2", "ok", false),
        ];
        let minimals = HashMap::new();
        let lp = LayeredPrompt::from_monolithic("system");
        let tools: Vec<ToolDef> = vec![];
        let budget = LayeredBudget::with_total(100_000);

        let fin = ContextBuilder::new(msgs, &minimals, &lp, &tools, budget).finalize();
        assert!(!fin.recovered_errors.is_empty());
    }

    #[test]
    fn layered_prompt_tokens_survive_arc_sharing() {
        // The builder must not double-count tokens when the same Arc<str>
        // backs both persona and, say, memory (not realistic but guards
        // against future aliasing bugs).
        let shared: Arc<str> = Arc::from("shared layer");
        let lp = LayeredPrompt {
            persona: shared.clone(),
            memory: Some(shared),
            ..Default::default()
        };
        let bd = lp.token_breakdown();
        // Both should contribute independently to the total.
        assert!(bd.persona > 0);
        assert!(bd.memory > 0);
        assert_eq!(bd.total(), bd.persona + bd.memory);
    }
}
