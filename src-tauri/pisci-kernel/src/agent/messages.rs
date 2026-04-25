use crate::agent::plan::PlanTodoItem;
use serde::{Deserialize, Serialize};

/// Per-layer token counts mirrored from
/// [`crate::agent::harness::LayeredTokenBreakdown`] but shaped for
/// transport to the frontend. All values are raw token estimates; we
/// intentionally keep the layout flat (no nested `LayeredPromptTokens`)
/// so the frontend can render a stacked ring without needing to know
/// the Rust type hierarchy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LayeredTokenBreakdownSnapshot {
    pub persona: u32,
    pub scene: u32,
    pub memory: u32,
    pub project: u32,
    pub platform_hint: u32,
    pub tool_defs: u32,
    pub history_text: u32,
    pub history_tool_result_full: u32,
    pub history_tool_result_receipt: u32,
    pub rolling_summary: u32,
    pub state_frame: u32,
    pub vision: u32,
    pub request_overhead: u32,
}

/// Events streamed to the frontend during agent execution
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// A new LLM call is starting — frontend should replace the current streaming bubble
    /// with a fresh one (slide old one out, slide new one in).
    /// `iteration` is the 1-based loop iteration index.
    TextSegmentStart { iteration: u32 },
    /// Streaming text delta
    TextDelta { delta: String },
    /// Tool execution started
    ToolStart {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Tool execution finished
    ToolEnd {
        id: String,
        name: String,
        result: String,
        is_error: bool,
    },
    /// Full message committed to DB
    MessageCommit { message: serde_json::Value },
    /// Permission required from user
    PermissionRequest {
        request_id: String,
        tool_name: String,
        tool_input: serde_json::Value,
        description: String,
    },
    /// Snapshot of the current context-window utilisation, emitted by the agent
    /// loop before/after each compaction pass and after each main LLM call.
    /// The frontend renders this as a ring progress indicator next to the send
    /// button. All values are raw token counts (estimates); see
    /// `llm::compute_total_input_budget` for how `total_input_budget` is derived.
    ContextUsage {
        /// Estimated tokens for the next LLM request (system + tools + messages).
        estimated_input_tokens: u32,
        /// Usable input budget for this request (window − max_tokens − safety factor).
        total_input_budget: u32,
        /// 60% of `total_input_budget`; estimates above this value trigger Level-2
        /// proactive compaction on the next iteration.
        trigger_threshold: u32,
        /// Session-lifetime cumulative input tokens (monotonically non-decreasing).
        cumulative_input_tokens: u32,
        /// Session-lifetime cumulative output tokens.
        cumulative_output_tokens: u32,
        /// Rolling summary version, bumped on each successful compaction.
        rolling_summary_version: u32,
        /// Configured auto-compact threshold from settings (step size, 0 = disabled).
        auto_compact_threshold: u32,
        /// p8 — optional per-layer token breakdown so the UI ring can
        /// surface *what* is consuming context (system prompt vs. tools
        /// vs. history vs. vision). Emitted best-effort; absent when the
        /// agent loop is running in a codepath that has not yet computed
        /// the breakdown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layered_breakdown: Option<LayeredTokenBreakdownSnapshot>,
    },
    /// Agent loop complete
    Done {
        total_input_tokens: u32,
        total_output_tokens: u32,
    },
    /// Run was cancelled by the user.
    Cancelled,
    /// Error occurred
    Error { message: String },
    /// Visible plan/todo list for the current task
    PlanUpdate { items: Vec<PlanTodoItem> },
    /// Interactive UI card for the user to fill in (chat_ui tool).
    /// Frontend renders a structured form; user response is sent back via respond_interactive_ui.
    InteractiveUi {
        request_id: String,
        ui_definition: serde_json::Value,
    },
    /// A sub-agent (Fish) is executing — forwarded to the parent session so the user
    /// can see real-time progress without switching sessions.
    FishProgress {
        fish_id: String,
        fish_name: String,
        /// 1-based iteration index inside the Fish agent loop
        iteration: u32,
        /// Which tool the Fish is currently calling (None = LLM thinking)
        tool_name: Option<String>,
        /// "thinking" | "thinking_text" | "tool_call" | "tool_done" | "done"
        status: String,
        /// For status="thinking_text": the streaming text delta from the Fish LLM
        #[serde(skip_serializing_if = "Option::is_none")]
        text_delta: Option<String>,
    },
}
