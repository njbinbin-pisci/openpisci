//! Async-friendly summary worker primitives (p7 of the harness plan).
//!
//! What this module provides today:
//!
//! 1. [`CircuitBreaker`] — a small stateful guard that tracks consecutive
//!    failures and throttles retries so the agent loop cannot enter a
//!    compaction storm when the summariser model is flaky or context is
//!    incompressible.
//!
//! 2. [`StructuredSummary`] — the richer shape we want from the Level-2
//!    summariser: not just a prose blob but discrete fields (active plan
//!    items, next-step hint) that feed the p6 `StateFrame` directly.
//!
//! 3. [`parse_structured_summary`] — best-effort JSON parser with prose
//!    fallback so legacy summariser outputs continue to work.
//!
//! The module deliberately stays free of `tokio` runtime code so it
//! compiles clean in both the main agent loop and the offline evaluation
//! harness. True fire-and-forget background spawning is done at the call
//! site (currently inside `agent::loop_::compact_summarise`).

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

// ───────────────── Structured Rolling Summary (Phase 2b) ─────────────────

/// Evidence reference anchoring a fact/decision back to a concrete tool
/// exchange, so the rolling summary can point the LLM at the exact
/// `tool_use_id` when it needs the underlying detail via
/// `recall_tool_result`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    #[serde(default)]
    pub tool_use_id: String,
    #[serde(default)]
    pub note: String,
}

/// A discrete factual observation about the task/world extracted from
/// the conversation.
///
/// Information-theoretic role: a single typical-set codeword
/// corresponding to a stable sufficient statistic for downstream
/// decisions. `confidence` is a soft belief that decays when
/// contradicted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FactItem {
    #[serde(default)]
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub evidence: Option<EvidenceRef>,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

fn default_confidence() -> f32 {
    0.8
}

/// A decision the agent has committed to (architectural, scope, tool
/// choice). Kept separate from facts because decisions survive even
/// when the evidence that produced them is summarised away.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DecisionItem {
    #[serde(default)]
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub evidence: Option<EvidenceRef>,
}

/// An open item — a TODO, handoff, verification pending, or blocking
/// question. These are *never* evicted by LRU because they drive the
/// agent's next action.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenItem {
    #[serde(default)]
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub deadline: Option<String>,
}

/// Structured rolling summary (Phase 2b). Replaces the previous
/// free-text `summary` blob while remaining backward-compatible (a
/// plain prose dump is rendered into `task_contract` / `facts` on
/// read when no structured JSON is available).
///
/// Information-theoretic framing: this is the Information Bottleneck
/// representation T, minimising I(T; raw_history) subject to
/// preserving I(T; downstream_task_success). Each field is a shard
/// that serves a distinct downstream signal:
/// - `task_contract` — the original user contract (stable, rarely evicted)
/// - `facts` — typical-set observations (evictable by confidence)
/// - `decisions` — committed choices (non-evictable)
/// - `open_items` — pending work (non-evictable)
/// - `tool_evidence` — FEC index, enables `recall_tool_result`
/// - `errors_learned` — mistakes not to repeat (non-evictable, capped)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StructuredRollingSummary {
    #[serde(default)]
    pub task_contract: String,
    #[serde(default)]
    pub facts: Vec<FactItem>,
    #[serde(default)]
    pub decisions: Vec<DecisionItem>,
    #[serde(default)]
    pub open_items: Vec<OpenItem>,
    #[serde(default)]
    pub tool_evidence: Vec<EvidenceRef>,
    #[serde(default)]
    pub errors_learned: Vec<String>,
    /// Bumped on every successful merge. Persisted so downstream tools
    /// can detect regressions and refuse stale reads.
    #[serde(default)]
    pub version: u32,
    /// Index of the newest message already folded into this summary.
    /// Used by the incremental merge path (Phase 2a) to feed the LLM
    /// only the delta.
    #[serde(default)]
    pub last_msg_idx_covered: usize,
}

impl StructuredRollingSummary {
    /// Promote a plain-text rolling summary into the structured shape.
    /// Treats the whole body as `task_contract` so legacy sessions
    /// keep working when the new path first sees them.
    pub fn from_prose(s: &str) -> Self {
        let mut out = Self::default();
        let t = s.trim();
        if !t.is_empty() {
            out.task_contract = t.to_string();
        }
        out
    }

    /// Serialise to JSON for persistence in `sessions.rolling_summary`.
    /// Falls back to the prose body if serialisation fails (defensive
    /// — should never happen with the concrete types we use).
    pub fn to_prose(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| self.task_contract.clone())
    }

    /// Parse a stored JSON blob back into structure. Returns
    /// `from_prose` if the blob is not structured JSON.
    pub fn parse(s: &str) -> Self {
        let t = s.trim();
        if t.is_empty() {
            return Self::default();
        }
        if t.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<Self>(t) {
                return v;
            }
        }
        Self::from_prose(t)
    }

    /// Render the structured summary as a compact prompt-friendly
    /// string for injection into the LLM request. Preserves
    /// non-evictable shards (decisions, open_items, errors_learned)
    /// verbatim; LRU-evicts facts when over budget.
    ///
    /// `max_chars` is a *soft* cap — non-evictable shards are always
    /// emitted even when over budget. Returns empty string when the
    /// summary has no content.
    pub fn render_for_prompt(&self, max_chars: usize) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        if !self.task_contract.is_empty() {
            out.push_str("【任务契约】\n");
            out.push_str(&self.task_contract);
            out.push('\n');
        }
        if !self.decisions.is_empty() {
            out.push_str("【已决策】\n");
            for d in &self.decisions {
                out.push_str("- ");
                out.push_str(&d.text);
                if !d.rationale.is_empty() {
                    out.push_str("（理由：");
                    out.push_str(&d.rationale);
                    out.push('）');
                }
                out.push('\n');
            }
        }
        if !self.open_items.is_empty() {
            out.push_str("【待办】\n");
            for o in &self.open_items {
                out.push_str("- ");
                out.push_str(&o.text);
                if let Some(a) = &o.assignee {
                    out.push_str(" @");
                    out.push_str(a);
                }
                out.push('\n');
            }
        }
        if !self.errors_learned.is_empty() {
            out.push_str("【已知坑】\n");
            for e in &self.errors_learned {
                out.push_str("- ");
                out.push_str(e);
                out.push('\n');
            }
        }
        // Facts are the only evictable shard. Rank by confidence DESC
        // then emit until we hit the soft budget.
        if !self.facts.is_empty() {
            let mut ranked: Vec<&FactItem> = self.facts.iter().collect();
            ranked.sort_by(|a, b| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            out.push_str("【关键事实】\n");
            for f in ranked {
                if out.chars().count() >= max_chars {
                    break;
                }
                out.push_str("- ");
                out.push_str(&f.text);
                if let Some(ev) = &f.evidence {
                    if !ev.tool_use_id.is_empty() {
                        out.push_str(" [recall:");
                        out.push_str(&ev.tool_use_id);
                        out.push(']');
                    }
                }
                out.push('\n');
            }
        }
        out.trim_end().to_string()
    }

    pub fn is_empty(&self) -> bool {
        self.task_contract.is_empty()
            && self.facts.is_empty()
            && self.decisions.is_empty()
            && self.open_items.is_empty()
            && self.errors_learned.is_empty()
    }
}

// ───────────────── Merge instructions (Phase 2a) ─────────────────

/// Single merge operation emitted by the incremental L2 LLM.
///
/// The LLM returns a list of these; the pure-function
/// [`apply_merge_instructions`] mutates a `StructuredRollingSummary`
/// atomically. If any instruction fails to apply (e.g. malformed
/// JSON), the caller is expected to roll back to the previous
/// summary — hence the merge function never panics and never yields
/// a partially applied state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MergeInstruction {
    SetTaskContract {
        text: String,
    },
    AddFact {
        text: String,
        #[serde(default = "default_confidence")]
        confidence: f32,
        #[serde(default)]
        evidence: Option<EvidenceRef>,
    },
    UpdateFact {
        id: String,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        confidence: Option<f32>,
    },
    InvalidateFact {
        id: String,
    },
    AddDecision {
        text: String,
        #[serde(default)]
        rationale: String,
        #[serde(default)]
        evidence: Option<EvidenceRef>,
    },
    AddOpenItem {
        text: String,
        #[serde(default)]
        assignee: Option<String>,
    },
    CloseOpenItem {
        id: String,
    },
    AddError {
        text: String,
    },
    AddEvidence {
        tool_use_id: String,
        #[serde(default)]
        note: String,
    },
    /// No-op. Useful when the LLM wants to signal "nothing to add"
    /// without emitting an empty list (some models struggle with `[]`).
    Noop,
}

/// Apply a batch of merge instructions to a rolling summary atomically.
///
/// Returns a `Result`:
/// - `Ok(new_summary)` — all instructions applied cleanly. `version`
///   is bumped by 1.
/// - `Err(reason)` — one or more instructions were invalid. The
///   caller is expected to keep the previous summary (atomic rollback).
///
/// Pure function: no I/O, deterministic, easy to unit-test.
pub fn apply_merge_instructions(
    prev: &StructuredRollingSummary,
    instructions: &[MergeInstruction],
    last_msg_idx_covered: usize,
) -> Result<StructuredRollingSummary, String> {
    let mut out = prev.clone();
    for (i, ins) in instructions.iter().enumerate() {
        if let Err(e) = apply_single(&mut out, ins) {
            return Err(format!("instruction #{} failed: {}", i, e));
        }
    }
    out.version = prev.version.saturating_add(1);
    out.last_msg_idx_covered = last_msg_idx_covered.max(prev.last_msg_idx_covered);
    // Bounded-size guarantees: the summary must never grow without
    // bound regardless of how many instructions the LLM emits.
    const MAX_FACTS: usize = 40;
    const MAX_DECISIONS: usize = 20;
    const MAX_OPEN_ITEMS: usize = 30;
    const MAX_ERRORS: usize = 20;
    const MAX_EVIDENCE: usize = 60;
    if out.facts.len() > MAX_FACTS {
        // Keep the highest-confidence facts.
        out.facts.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out.facts.truncate(MAX_FACTS);
    }
    if out.decisions.len() > MAX_DECISIONS {
        out.decisions
            .drain(0..out.decisions.len() - MAX_DECISIONS);
    }
    if out.open_items.len() > MAX_OPEN_ITEMS {
        out.open_items
            .drain(0..out.open_items.len() - MAX_OPEN_ITEMS);
    }
    if out.errors_learned.len() > MAX_ERRORS {
        out.errors_learned
            .drain(0..out.errors_learned.len() - MAX_ERRORS);
    }
    if out.tool_evidence.len() > MAX_EVIDENCE {
        out.tool_evidence
            .drain(0..out.tool_evidence.len() - MAX_EVIDENCE);
    }
    Ok(out)
}

fn apply_single(
    out: &mut StructuredRollingSummary,
    ins: &MergeInstruction,
) -> Result<(), String> {
    match ins {
        MergeInstruction::SetTaskContract { text } => {
            if text.trim().is_empty() {
                return Err("empty task_contract".into());
            }
            out.task_contract = text.trim().to_string();
        }
        MergeInstruction::AddFact {
            text,
            confidence,
            evidence,
        } => {
            let text = text.trim();
            if text.is_empty() {
                return Err("empty fact text".into());
            }
            // Deduplicate by exact text match — re-observing an
            // existing fact bumps its confidence (up to 1.0) instead
            // of creating a duplicate (Dictionary coding / AEP).
            if let Some(existing) = out.facts.iter_mut().find(|f| f.text == text) {
                existing.confidence = (existing.confidence + 0.1).min(1.0);
                if existing.evidence.is_none() {
                    existing.evidence = evidence.clone();
                }
                return Ok(());
            }
            let id = format!("f{}", out.facts.len() + 1);
            out.facts.push(FactItem {
                id,
                text: text.to_string(),
                evidence: evidence.clone(),
                confidence: confidence.clamp(0.0, 1.0),
            });
        }
        MergeInstruction::UpdateFact {
            id,
            text,
            confidence,
        } => {
            let Some(target) = out.facts.iter_mut().find(|f| f.id == *id) else {
                // Unknown id is a soft failure — silently ignore so
                // the LLM cannot corrupt the summary with stale ids.
                return Ok(());
            };
            if let Some(t) = text {
                let t = t.trim();
                if !t.is_empty() {
                    target.text = t.to_string();
                }
            }
            if let Some(c) = confidence {
                target.confidence = c.clamp(0.0, 1.0);
            }
        }
        MergeInstruction::InvalidateFact { id } => {
            out.facts.retain(|f| f.id != *id);
        }
        MergeInstruction::AddDecision {
            text,
            rationale,
            evidence,
        } => {
            let text = text.trim();
            if text.is_empty() {
                return Err("empty decision text".into());
            }
            // Dedup by exact text: decisions don't have confidence.
            if out.decisions.iter().any(|d| d.text == text) {
                return Ok(());
            }
            let id = format!("d{}", out.decisions.len() + 1);
            out.decisions.push(DecisionItem {
                id,
                text: text.to_string(),
                rationale: rationale.trim().to_string(),
                evidence: evidence.clone(),
            });
        }
        MergeInstruction::AddOpenItem { text, assignee } => {
            let text = text.trim();
            if text.is_empty() {
                return Err("empty open_item text".into());
            }
            if out.open_items.iter().any(|o| o.text == text) {
                return Ok(());
            }
            let id = format!("o{}", out.open_items.len() + 1);
            out.open_items.push(OpenItem {
                id,
                text: text.to_string(),
                assignee: assignee.clone(),
                deadline: None,
            });
        }
        MergeInstruction::CloseOpenItem { id } => {
            out.open_items.retain(|o| o.id != *id);
        }
        MergeInstruction::AddError { text } => {
            let text = text.trim();
            if text.is_empty() {
                return Err("empty error text".into());
            }
            if !out.errors_learned.iter().any(|e| e == text) {
                out.errors_learned.push(text.to_string());
            }
        }
        MergeInstruction::AddEvidence { tool_use_id, note } => {
            if tool_use_id.trim().is_empty() {
                return Err("empty tool_use_id".into());
            }
            if out
                .tool_evidence
                .iter()
                .any(|e| e.tool_use_id == *tool_use_id)
            {
                return Ok(());
            }
            out.tool_evidence.push(EvidenceRef {
                tool_use_id: tool_use_id.clone(),
                note: note.trim().to_string(),
            });
        }
        MergeInstruction::Noop => {}
    }
    Ok(())
}

/// Prompt suffix used by the incremental L2 path (Phase 2a).
///
/// Asks the LLM to emit a JSON array of merge instructions rather
/// than a whole new summary — this is the predictive-coding formulation
/// (encode only the residual vs. the prior).
pub const INCREMENTAL_MERGE_PROMPT_SUFFIX: &str = "\n\n\
    严格按 JSON 数组形式返回 merge 指令列表，且只返回这段 JSON。\n\
    可用 op 类型：\n\
    - {\"op\":\"set_task_contract\",\"text\":\"…\"}\n\
    - {\"op\":\"add_fact\",\"text\":\"…\",\"confidence\":0.9,\"evidence\":{\"tool_use_id\":\"…\",\"note\":\"…\"}}\n\
    - {\"op\":\"update_fact\",\"id\":\"fN\",\"text\":\"…\",\"confidence\":0.7}\n\
    - {\"op\":\"invalidate_fact\",\"id\":\"fN\"}\n\
    - {\"op\":\"add_decision\",\"text\":\"…\",\"rationale\":\"…\"}\n\
    - {\"op\":\"add_open_item\",\"text\":\"…\",\"assignee\":\"…\"}\n\
    - {\"op\":\"close_open_item\",\"id\":\"oN\"}\n\
    - {\"op\":\"add_error\",\"text\":\"…\"}\n\
    - {\"op\":\"add_evidence\",\"tool_use_id\":\"…\",\"note\":\"…\"}\n\
    - {\"op\":\"noop\"}\n\
    原则：只处理**新增信息**；已在先验中出现的事实/决策不要重复；\n\
    不得输出任何 JSON 以外的文本，不要 markdown 围栏。";

/// Extract the first `[...]` JSON array from a string (for the
/// incremental merge response).
pub fn extract_json_array(s: &str) -> Option<String> {
    let s = strip_code_fence(s);
    let start = s.find('[')?;
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes[start..].iter().enumerate() {
        let idx = start + i;
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[start..=idx].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse a merge-instruction list from raw LLM text. Tolerates
/// fenced output and leading prose.
pub fn parse_merge_instructions(raw: &str) -> Result<Vec<MergeInstruction>, String> {
    let arr = extract_json_array(raw).ok_or_else(|| "no JSON array found".to_string())?;
    serde_json::from_str::<Vec<MergeInstruction>>(&arr).map_err(|e| e.to_string())
}

// ───────────────── Existing prose-summary path (unchanged) ─────────────────

/// Additional instructions appended to the summariser prompt to request
/// a structured JSON payload. Kept small so legacy models that don't
/// follow the schema still produce usable prose (we fall back to
/// treating the whole response as `summary`).
pub const STRUCTURED_SUMMARY_PROMPT_SUFFIX: &str = "\n\n\
    请以如下 JSON 对象返回（单个代码围栏可选，也可以直接裸输出）：\n\
    {\n\
      \"summary\": \"合并后的滚动摘要正文\",\n\
      \"active_plan_items\": [\"未完成的 todo/handoff 要点，每项 ≤ 80 字\"],\n\
      \"next_step_hint\": \"建议的下一步，≤ 80 字，可为空\"\n\
    }\n\
    如果无法产出 JSON，可直接输出纯摘要文本。";

/// Structured representation of a Level-2 summary pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructuredSummary {
    pub summary: String,
    pub active_plan_items: Vec<String>,
    pub next_step_hint: Option<String>,
}

impl StructuredSummary {
    pub fn from_prose(s: &str) -> Self {
        Self {
            summary: s.trim().to_string(),
            active_plan_items: Vec::new(),
            next_step_hint: None,
        }
    }
}

/// Parse a summariser response into [`StructuredSummary`].
///
/// Strategy:
/// 1. Try to locate a JSON object in the response (`{ ... }` fenced or
///    inline). If parse succeeds, use `summary` / `active_plan_items` /
///    `next_step_hint`.
/// 2. Fallback: treat the entire trimmed response as the `summary` blob
///    (legacy behaviour).
///
/// This is deliberately permissive so a flaky or older summariser never
/// breaks the compaction loop.
pub fn parse_structured_summary(raw: &str) -> StructuredSummary {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return StructuredSummary::default();
    }
    // Strip a leading code fence if present (```json ... ```).
    let stripped = strip_code_fence(trimmed);
    if let Some(obj_str) = extract_json_object(stripped) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&obj_str) {
            let summary = v
                .get("summary")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            let active_plan_items = v
                .get("active_plan_items")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| item.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let next_step_hint = v
                .get("next_step_hint")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            if !summary.is_empty() || !active_plan_items.is_empty() {
                return StructuredSummary {
                    summary,
                    active_plan_items,
                    next_step_hint,
                };
            }
        }
    }
    StructuredSummary::from_prose(trimmed)
}

fn strip_code_fence(input: &str) -> &str {
    let s = input.trim();
    if let Some(rest) = s.strip_prefix("```json") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else if let Some(rest) = s.strip_prefix("```") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else {
        s
    }
}

/// Find the outermost `{...}` block in `s`. Very lenient — used only
/// after a model was explicitly asked to emit JSON.
fn extract_json_object(s: &str) -> Option<String> {
    let start = s.find('{')?;
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes[start..].iter().enumerate() {
        let idx = start + i;
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[start..idx + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Small circuit breaker for the async summary worker. Tracks
/// consecutive failures and the timestamp of the last successful pass
/// so the agent loop can refuse to attempt yet another compaction when
/// the previous one hard-failed or the throttle window is still open.
///
/// Defaults (picked to match hermes-agent's proactive compaction
/// cadence): up to 2 consecutive failures before opening, and a minimum
/// interval of 20 seconds between attempts after a failure.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    max_consecutive_failures: u32,
    min_interval_on_failure: Duration,
    consecutive_failures: u32,
    last_failure_at: Option<Instant>,
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(2, Duration::from_secs(20))
    }
}

impl CircuitBreaker {
    pub fn new(max_consecutive_failures: u32, min_interval_on_failure: Duration) -> Self {
        Self {
            max_consecutive_failures,
            min_interval_on_failure,
            consecutive_failures: 0,
            last_failure_at: None,
        }
    }

    /// True when the breaker is *closed* and a new attempt is allowed.
    pub fn is_closed(&self) -> bool {
        self.is_closed_at(Instant::now())
    }

    /// Test-friendly variant that accepts an explicit "now" so unit
    /// tests do not depend on wall-clock.
    pub fn is_closed_at(&self, now: Instant) -> bool {
        if self.consecutive_failures >= self.max_consecutive_failures {
            return false;
        }
        if let Some(last) = self.last_failure_at {
            if now.saturating_duration_since(last) < self.min_interval_on_failure {
                return false;
            }
        }
        true
    }

    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.last_failure_at = None;
    }

    pub fn record_failure(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_failure_at = Some(Instant::now());
    }

    pub fn record_failure_at(&mut self, now: Instant) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_failure_at = Some(now);
    }

    /// Manual reset — used when a higher layer (user clicking
    /// "compact now") wants to clear a tripped breaker.
    pub fn reset(&mut self) {
        self.consecutive_failures = 0;
        self.last_failure_at = None;
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inline_json_object() {
        let raw = r#"好的，这是总结：
{
  "summary": "合并后的摘要",
  "active_plan_items": ["跑 eval", "写回归测试"],
  "next_step_hint": "等用户确认"
}"#;
        let parsed = parse_structured_summary(raw);
        assert_eq!(parsed.summary, "合并后的摘要");
        assert_eq!(parsed.active_plan_items.len(), 2);
        assert_eq!(parsed.next_step_hint.as_deref(), Some("等用户确认"));
    }

    #[test]
    fn parses_fenced_json_object() {
        let raw = "```json\n{\"summary\":\"s\",\"active_plan_items\":[],\"next_step_hint\":null}\n```";
        let parsed = parse_structured_summary(raw);
        assert_eq!(parsed.summary, "s");
        assert!(parsed.active_plan_items.is_empty());
        assert!(parsed.next_step_hint.is_none());
    }

    #[test]
    fn falls_back_to_prose_when_no_json() {
        let raw = "This is just a plain summary without JSON.";
        let parsed = parse_structured_summary(raw);
        assert_eq!(parsed.summary, raw);
        assert!(parsed.active_plan_items.is_empty());
        assert!(parsed.next_step_hint.is_none());
    }

    #[test]
    fn handles_empty_input() {
        let parsed = parse_structured_summary("   ");
        assert!(parsed.summary.is_empty());
        assert!(parsed.active_plan_items.is_empty());
    }

    #[test]
    fn ignores_braces_inside_strings() {
        let raw = r#"{"summary": "text with } inside", "active_plan_items": ["a"]}"#;
        let parsed = parse_structured_summary(raw);
        assert_eq!(parsed.summary, "text with } inside");
        assert_eq!(parsed.active_plan_items, vec!["a"]);
    }

    #[test]
    fn circuit_breaker_opens_after_consecutive_failures() {
        let mut cb = CircuitBreaker::new(2, Duration::from_millis(0));
        assert!(cb.is_closed());
        cb.record_failure();
        assert!(cb.is_closed());
        cb.record_failure();
        // Two failures reach the threshold — now open.
        assert!(!cb.is_closed());
        assert_eq!(cb.consecutive_failures(), 2);
    }

    #[test]
    fn circuit_breaker_throttles_with_min_interval() {
        let mut cb = CircuitBreaker::new(5, Duration::from_secs(60));
        let t0 = Instant::now();
        cb.record_failure_at(t0);
        // Within the throttle window — closed breaker but refused.
        assert!(!cb.is_closed_at(t0 + Duration::from_secs(30)));
        // After the window elapses, attempts are allowed again.
        assert!(cb.is_closed_at(t0 + Duration::from_secs(61)));
    }

    // ───── Phase 2a/2b structured rolling summary tests ─────

    #[test]
    fn structured_summary_roundtrip() {
        let mut s = StructuredRollingSummary::default();
        s.task_contract = "refactor foo".into();
        s.facts.push(FactItem {
            id: "f1".into(),
            text: "bar uses baz".into(),
            confidence: 0.9,
            ..Default::default()
        });
        let json = s.to_prose();
        let back = StructuredRollingSummary::parse(&json);
        assert_eq!(s, back);
    }

    #[test]
    fn structured_summary_prose_fallback() {
        let s = StructuredRollingSummary::parse("legacy prose body");
        assert_eq!(s.task_contract, "legacy prose body");
        assert!(s.facts.is_empty());
    }

    #[test]
    fn structured_summary_render_non_empty() {
        let mut s = StructuredRollingSummary::default();
        s.task_contract = "do X".into();
        s.decisions.push(DecisionItem {
            id: "d1".into(),
            text: "use SQLite".into(),
            rationale: "file-local".into(),
            ..Default::default()
        });
        s.facts.push(FactItem {
            id: "f1".into(),
            text: "low confidence".into(),
            confidence: 0.3,
            ..Default::default()
        });
        s.facts.push(FactItem {
            id: "f2".into(),
            text: "high confidence".into(),
            confidence: 0.95,
            ..Default::default()
        });
        let out = s.render_for_prompt(10_000);
        assert!(out.contains("do X"));
        assert!(out.contains("SQLite"));
        assert!(out.contains("high confidence"));
    }

    #[test]
    fn merge_add_fact_bumps_confidence_on_rerun() {
        let prev = StructuredRollingSummary::default();
        let r = apply_merge_instructions(
            &prev,
            &[MergeInstruction::AddFact {
                text: "x=1".into(),
                confidence: 0.7,
                evidence: None,
            }],
            1,
        )
        .unwrap();
        assert_eq!(r.facts.len(), 1);
        assert!((r.facts[0].confidence - 0.7).abs() < 1e-4);
        // Re-observe the same fact.
        let r2 = apply_merge_instructions(
            &r,
            &[MergeInstruction::AddFact {
                text: "x=1".into(),
                confidence: 0.7,
                evidence: None,
            }],
            2,
        )
        .unwrap();
        assert_eq!(r2.facts.len(), 1);
        assert!(r2.facts[0].confidence > 0.7 + 0.05);
    }

    #[test]
    fn merge_dedup_preserves_first_evidence() {
        let prev = StructuredRollingSummary::default();
        let r = apply_merge_instructions(
            &prev,
            &[
                MergeInstruction::AddFact {
                    text: "y=2".into(),
                    confidence: 0.8,
                    evidence: Some(EvidenceRef {
                        tool_use_id: "tu1".into(),
                        note: "first".into(),
                    }),
                },
                MergeInstruction::AddFact {
                    text: "y=2".into(),
                    confidence: 0.8,
                    evidence: Some(EvidenceRef {
                        tool_use_id: "tu2".into(),
                        note: "later".into(),
                    }),
                },
            ],
            3,
        )
        .unwrap();
        assert_eq!(r.facts.len(), 1);
        assert_eq!(
            r.facts[0].evidence.as_ref().unwrap().tool_use_id,
            "tu1"
        );
    }

    #[test]
    fn merge_close_open_item_removes_by_id() {
        let prev = StructuredRollingSummary::default();
        let r1 = apply_merge_instructions(
            &prev,
            &[MergeInstruction::AddOpenItem {
                text: "write tests".into(),
                assignee: None,
            }],
            1,
        )
        .unwrap();
        assert_eq!(r1.open_items.len(), 1);
        let id = r1.open_items[0].id.clone();
        let r2 = apply_merge_instructions(
            &r1,
            &[MergeInstruction::CloseOpenItem { id }],
            2,
        )
        .unwrap();
        assert!(r2.open_items.is_empty());
    }

    #[test]
    fn merge_version_bumps_monotonically() {
        let prev = StructuredRollingSummary::default();
        let r1 = apply_merge_instructions(&prev, &[MergeInstruction::Noop], 1).unwrap();
        assert_eq!(r1.version, 1);
        let r2 = apply_merge_instructions(&r1, &[MergeInstruction::Noop], 2).unwrap();
        assert_eq!(r2.version, 2);
    }

    #[test]
    fn merge_rejects_empty_fact() {
        let prev = StructuredRollingSummary::default();
        let err = apply_merge_instructions(
            &prev,
            &[MergeInstruction::AddFact {
                text: "  ".into(),
                confidence: 0.5,
                evidence: None,
            }],
            1,
        );
        assert!(err.is_err());
    }

    #[test]
    fn merge_rejects_empty_task_contract() {
        let prev = StructuredRollingSummary::default();
        let err = apply_merge_instructions(
            &prev,
            &[MergeInstruction::SetTaskContract { text: "".into() }],
            1,
        );
        assert!(err.is_err());
    }

    #[test]
    fn merge_facts_capped() {
        let prev = StructuredRollingSummary::default();
        let mut ins = Vec::new();
        for i in 0..60 {
            ins.push(MergeInstruction::AddFact {
                text: format!("fact {}", i),
                confidence: (i as f32) / 100.0,
                evidence: None,
            });
        }
        let r = apply_merge_instructions(&prev, &ins, 1).unwrap();
        assert!(r.facts.len() <= 40);
    }

    #[test]
    fn merge_last_msg_idx_never_regresses() {
        let mut prev = StructuredRollingSummary::default();
        prev.last_msg_idx_covered = 100;
        let r = apply_merge_instructions(&prev, &[MergeInstruction::Noop], 50).unwrap();
        assert_eq!(r.last_msg_idx_covered, 100);
    }

    #[test]
    fn parse_merge_instructions_inline_array() {
        let raw = r#"[{"op":"add_fact","text":"foo","confidence":0.5},{"op":"noop"}]"#;
        let parsed = parse_merge_instructions(raw).unwrap();
        assert_eq!(parsed.len(), 2);
        matches!(parsed[0], MergeInstruction::AddFact { .. });
    }

    #[test]
    fn parse_merge_instructions_fenced_array() {
        let raw = "```json\n[{\"op\":\"add_error\",\"text\":\"oops\"}]\n```";
        let parsed = parse_merge_instructions(raw).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn parse_merge_instructions_rejects_no_array() {
        let err = parse_merge_instructions("this has no array");
        assert!(err.is_err());
    }

    #[test]
    fn circuit_breaker_success_resets() {
        let mut cb = CircuitBreaker::new(2, Duration::from_millis(100));
        cb.record_failure();
        cb.record_failure();
        assert!(!cb.is_closed());
        cb.record_success();
        assert!(cb.is_closed());
        assert_eq!(cb.consecutive_failures(), 0);
    }
}
