pub mod claude;
pub mod deepseek;
pub mod kimi;
pub mod minimax;
pub mod openai;
pub mod qwen;
pub mod zhipu;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A single message in the conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String,
    pub content: MessageContent,
}

/// Message content - either plain text or a list of content blocks
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl MessageContent {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }
    pub fn as_text(&self) -> String {
        match self {
            Self::Text(t) => t.clone(),
            Self::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub source_type: String, // "base64"
    pub media_type: String, // "image/png"
    pub data: String,       // base64 encoded
}

/// Tool definition sent to the LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// A streaming chunk from the LLM
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum LlmChunk {
    /// Text delta
    TextDelta(String),
    /// Tool use request
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Stream complete
    Done {
        input_tokens: u32,
        output_tokens: u32,
    },
    /// Error
    Error(String),
}

/// Request parameters
#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub messages: Vec<LlmMessage>,
    pub system: Option<String>,
    pub tools: Vec<ToolDef>,
    pub model: String,
    pub max_tokens: u32,
    pub stream: bool,
    /// User-configured vision override. Some(true) = always send images,
    /// Some(false) = never send images, None = auto-detect from model name.
    pub vision_override: Option<bool>,
}

/// Non-streaming response
#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// Unified LLM client trait
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Send a request and return a stream of chunks
    #[allow(dead_code)]
    async fn stream(&self, req: LlmRequest, tx: tokio::sync::mpsc::Sender<LlmChunk>) -> Result<()>;

    /// Send a request and return a complete response (non-streaming)
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse>;
}

// ---------------------------------------------------------------------------
// Token estimation helpers
// ---------------------------------------------------------------------------

/// Estimate the number of tokens in a string.
/// CJK characters count as 1 token each; other characters count as 1 token per 4 chars.
/// Returns 0 for empty strings.
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let mut cjk_count = 0usize;
    let mut ascii_count = 0usize;
    for ch in text.chars() {
        let cp = ch as u32;
        if (0x4E00..=0x9FFF).contains(&cp)
            || (0x3400..=0x4DBF).contains(&cp)
            || (0xF900..=0xFAFF).contains(&cp)
            || (0x3000..=0x303F).contains(&cp)
            || (0xFF00..=0xFFEF).contains(&cp)
        {
            cjk_count += 1;
        } else {
            ascii_count += 1;
        }
    }
    cjk_count + (ascii_count / 4).max(1)
}

/// Estimate the token count for a single LlmMessage.
/// Correctly handles Blocks content (ToolUse/ToolResult) which as_text() ignores.
///
/// Each message has ~8 tokens of framing overhead (role, delimiters) on top of
/// content tokens, which we add here to reduce systematic underestimation.
pub fn estimate_message_tokens(msg: &LlmMessage) -> usize {
    const MSG_OVERHEAD: usize = 8; // role + message framing tokens
    let content_tokens = match &msg.content {
        MessageContent::Text(t) => estimate_tokens(t),
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => estimate_tokens(text),
                ContentBlock::ToolUse { name, input, .. } => {
                    // tool_call_id + function name + arguments JSON
                    8 + estimate_tokens(name) + estimate_tokens(&input.to_string())
                }
                ContentBlock::ToolResult { content, .. } => {
                    // tool_call_id overhead + content
                    4 + estimate_tokens(content)
                }
                ContentBlock::Image { .. } => 256, // rough image token estimate
            })
            .sum(),
    };
    content_tokens + MSG_OVERHEAD
}

/// Estimate the token count for a single tool definition.
pub fn estimate_tool_def_tokens(tool: &ToolDef) -> usize {
    const TOOL_OVERHEAD: usize = 16; // name/description/schema framing
    TOOL_OVERHEAD
        + estimate_tokens(&tool.name)
        + estimate_tokens(&tool.description)
        + estimate_tokens(&tool.input_schema.to_string())
}

/// Estimate the token count consumed by non-message request inputs.
pub fn estimate_request_overhead_tokens(system: Option<&str>, tools: &[ToolDef]) -> usize {
    const REQUEST_FRAMING: usize = 24; // request-level framing and provider metadata
    let system_tokens = system.map(estimate_tokens).unwrap_or(0);
    let tool_tokens: usize = tools.iter().map(estimate_tool_def_tokens).sum();
    REQUEST_FRAMING + system_tokens + tool_tokens
}

/// Estimate the token count consumed by the full request input payload.
pub fn estimate_request_input_tokens(
    messages: &[LlmMessage],
    system: Option<&str>,
    tools: &[ToolDef],
) -> usize {
    estimate_request_overhead_tokens(system, tools)
        + messages.iter().map(estimate_message_tokens).sum::<usize>()
}

/// Compute the safe total token budget for the full input payload
/// (`system + tools + messages`), excluding model output tokens.
pub fn compute_total_input_budget(context_window: u32, max_tokens: u32) -> usize {
    let window = if context_window > 0 {
        context_window as usize
    } else {
        match max_tokens {
            t if t >= 8192 => 128_000,
            t if t >= 4096 => 64_000,
            _ => 32_000,
        }
    };
    let usable = window.saturating_sub(max_tokens as usize);
    (usable as f64 * 0.85) as usize
}

/// Compute the usable token budget for *input messages*.
///
/// `context_window` is the user-configured input context limit (0 = auto).
/// `max_tokens` is the max *output* tokens.
///
/// When `context_window` is 0 (not configured), we use a conservative estimate:
///   - max_tokens >= 8192: assume 128k window (GPT-4o, Kimi, DeepSeek-V3)
///   - max_tokens >= 4096: assume 64k window
///   - otherwise: assume 32k window
///
/// We apply an additional 0.85 safety factor to compensate for the systematic
/// underestimation in `estimate_tokens` (JSON overhead, message framing, etc.).
/// Empirically, actual token counts run ~10-15% higher than estimates.
pub fn compute_context_budget(context_window: u32, max_tokens: u32) -> usize {
    const SYSTEM_OVERHEAD: usize = 3_000; // system prompt + framing
    compute_total_input_budget(context_window, max_tokens).saturating_sub(SYSTEM_OVERHEAD)
}

/// Build the appropriate client based on provider name
pub fn build_client(provider: &str, api_key: &str, base_url: Option<&str>) -> Box<dyn LlmClient> {
    build_client_with_timeout(provider, api_key, base_url, 120)
}

/// Build the appropriate client with a configurable read timeout (seconds).
pub fn build_client_with_timeout(
    provider: &str,
    api_key: &str,
    base_url: Option<&str>,
    read_timeout_secs: u32,
) -> Box<dyn LlmClient> {
    match provider {
        "openai" | "custom" => Box::new(openai::OpenAiClient::with_timeout(
            api_key,
            base_url.unwrap_or("https://api.openai.com/v1"),
            read_timeout_secs,
        )),
        "deepseek" => Box::new(deepseek::DeepSeekClient::with_timeout(
            api_key,
            read_timeout_secs,
        )),
        "qwen" | "tongyi" => Box::new(qwen::QwenClient::with_timeout(api_key, read_timeout_secs)),
        "minimax" => Box::new(minimax::MiniMaxClient::with_timeout(
            api_key,
            read_timeout_secs,
        )),
        "zhipu" => Box::new(zhipu::ZhipuClient::with_timeout(api_key, read_timeout_secs)),
        "kimi" | "moonshot" => Box::new(kimi::KimiClient::with_timeout(api_key, read_timeout_secs)),
        _ => Box::new(claude::ClaudeClient::with_timeout(
            api_key,
            read_timeout_secs,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compute_context_budget, compute_total_input_budget, estimate_request_input_tokens,
        ContentBlock, LlmMessage, MessageContent, ToolDef,
    };
    use serde_json::json;

    #[test]
    fn total_input_budget_exceeds_message_budget() {
        let total = compute_total_input_budget(32_000, 4_096);
        let message = compute_context_budget(32_000, 4_096);
        assert!(total > message);
        assert_eq!(total - message, 3_000);
    }

    #[test]
    fn request_input_estimate_includes_system_and_tools() {
        let messages = vec![LlmMessage {
            role: "user".into(),
            content: MessageContent::Blocks(vec![
                ContentBlock::Text {
                    text: "帮我总结一下".into(),
                },
                ContentBlock::ToolUse {
                    id: "tool_1".into(),
                    name: "search".into(),
                    input: json!({"query": "pisci"}),
                },
            ]),
        }];
        let tools = vec![ToolDef {
            name: "search".into(),
            description: "Search workspace".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"}
                }
            }),
        }];

        let with_overhead = estimate_request_input_tokens(&messages, Some("你是一个助手"), &tools);
        let messages_only: usize = messages.iter().map(super::estimate_message_tokens).sum();
        assert!(with_overhead > messages_only);
    }
}
