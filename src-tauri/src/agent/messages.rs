#![allow(dead_code)]

use crate::agent::plan::PlanTodoItem;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

impl MessageRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Tool => "tool",
        }
    }
}

/// An agent conversation message (stored in DB and sent to frontend)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: String,
    pub session_id: String,
    pub role: MessageRole,
    /// Main text content
    pub content: String,
    /// Tool calls made by the assistant (JSON array)
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRecord>,
    /// Tool results (for tool messages)
    #[serde(default)]
    pub tool_results: Vec<ToolResultRecord>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultRecord {
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: String,
    pub is_error: bool,
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
    /// Agent loop complete
    Done {
        total_input_tokens: u32,
        total_output_tokens: u32,
    },
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
        /// "thinking" | "tool_call" | "tool_done" | "done"
        status: String,
    },
}
