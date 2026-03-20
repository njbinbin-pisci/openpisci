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

/// Message content — either plain text or a list of content blocks
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
pub fn estimate_message_tokens(msg: &LlmMessage) -> usize {
    match &msg.content {
        MessageContent::Text(t) => estimate_tokens(t),
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => estimate_tokens(text),
                ContentBlock::ToolUse { name, input, .. } => {
                    estimate_tokens(name) + estimate_tokens(&input.to_string())
                }
                ContentBlock::ToolResult { content, .. } => estimate_tokens(content),
                ContentBlock::Image { .. } => 256, // rough image token estimate
            })
            .sum(),
    }
}

/// Compute the usable token budget from settings.
///
/// `context_window` is the user-configured input context limit (0 = auto).
/// `max_tokens` is the max *output* tokens (used only for auto-fallback).
///
/// Budget = (context_window × 0.85) − 2000 (system prompt overhead)
pub fn compute_context_budget(context_window: u32, max_tokens: u32) -> usize {
    const SYSTEM_OVERHEAD: usize = 2_000;
    let window = if context_window > 0 {
        context_window as usize
    } else {
        match max_tokens {
            t if t >= 8192 => 100_000,
            t if t >= 4096 => 60_000,
            _ => 30_000,
        }
    };
    ((window as f64 * 0.85) as usize).saturating_sub(SYSTEM_OVERHEAD)
}

/// Build the appropriate client based on provider name
pub fn build_client(provider: &str, api_key: &str, base_url: Option<&str>) -> Box<dyn LlmClient> {
    match provider {
        "openai" | "custom" => Box::new(openai::OpenAiClient::new(
            api_key,
            base_url.unwrap_or("https://api.openai.com/v1"),
        )),
        "deepseek" => Box::new(deepseek::DeepSeekClient::new(api_key)),
        "qwen" | "tongyi" => Box::new(qwen::QwenClient::new(api_key)),
        "minimax" => Box::new(minimax::MiniMaxClient::new(api_key)),
        "zhipu" => Box::new(zhipu::ZhipuClient::new(api_key)),
        "kimi" | "moonshot" => Box::new(kimi::KimiClient::new(api_key)),
        _ => Box::new(claude::ClaudeClient::new(api_key)),
    }
}
