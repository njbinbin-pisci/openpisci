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
