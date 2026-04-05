/// Anthropic Claude API client (Messages API, streaming SSE)
use super::{ContentBlock, LlmChunk, LlmClient, LlmRequest, LlmResponse, MessageContent, ToolCall};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc::Sender;

const CLAUDE_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct ClaudeClient {
    api_key: String,
    http: Client,
}

impl ClaudeClient {
    pub fn new(api_key: &str) -> Self {
        Self::with_timeout(api_key, 120)
    }

    pub fn with_timeout(api_key: &str, read_timeout_secs: u32) -> Self {
        let secs = read_timeout_secs.max(30) as u64;
        let http = Client::builder()
            .read_timeout(std::time::Duration::from_secs(secs))
            .build()
            .unwrap_or_default();
        Self {
            api_key: api_key.to_string(),
            http,
        }
    }

    fn build_body(&self, req: &LlmRequest) -> Value {
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                let content: Value = match &m.content {
                    MessageContent::Text(t) => json!(t),
                    MessageContent::Blocks(blocks) => {
                        let parts: Vec<Value> = blocks
                            .iter()
                            .map(|b| match b {
                                ContentBlock::Text { text } => {
                                    json!({"type": "text", "text": text})
                                }
                                ContentBlock::Image { source } => json!({
                                    "type": "image",
                                    "source": {
                                        "type": source.source_type,
                                        "media_type": source.media_type,
                                        "data": source.data
                                    }
                                }),
                                ContentBlock::ToolUse { id, name, input } => json!({
                                    "type": "tool_use",
                                    "id": id,
                                    "name": name,
                                    "input": input
                                }),
                                ContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                    is_error,
                                } => json!({
                                    "type": "tool_result",
                                    "tool_use_id": tool_use_id,
                                    "content": content,
                                    "is_error": is_error
                                }),
                            })
                            .collect();
                        json!(parts)
                    }
                };
                json!({"role": m.role, "content": content})
            })
            .collect();

        let mut body = json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "messages": messages,
            "stream": req.stream,
        });

        if let Some(sys) = &req.system {
            body["system"] = json!(sys);
        }

        if !req.tools.is_empty() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    })
                })
                .collect();
            body["tools"] = json!(tools);
        }

        body
    }
}

// ---------------------------------------------------------------------------
// SSE event types from Anthropic
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct SseEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    delta: Option<Delta>,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    content_block: Option<ContentBlockStart>,
    #[serde(default)]
    message: Option<MessageStart>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct Delta {
    #[serde(rename = "type")]
    delta_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ContentBlockStart {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct MessageStart {
    #[serde(default)]
    usage: Option<Usage>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

#[async_trait]
impl LlmClient for ClaudeClient {
    async fn stream(&self, req: LlmRequest, tx: Sender<LlmChunk>) -> Result<()> {
        let mut req_with_stream = req.clone();
        req_with_stream.stream = true;

        let body = self.build_body(&req_with_stream);

        let response = self
            .http
            .post(CLAUDE_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow!("Claude API error {}: {}", status, text));
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        // Track tool use accumulation: index -> (id, name, json_buf)
        let mut tool_bufs: std::collections::HashMap<usize, (String, String, String)> =
            std::collections::HashMap::new();
        let mut input_tokens = 0u32;
        let mut output_tokens = 0u32;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            // Process complete SSE lines
            while let Some(pos) = buffer.find("\n\n") {
                let block = buffer[..pos].to_string();
                buffer = buffer[pos + 2..].to_string();

                for line in block.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        if data == "[DONE]" {
                            break;
                        }
                        if let Ok(event) = serde_json::from_str::<SseEvent>(data) {
                            match event.event_type.as_str() {
                                "message_start" => {
                                    if let Some(msg) = event.message {
                                        if let Some(u) = msg.usage {
                                            input_tokens = u.input_tokens;
                                        }
                                    }
                                }
                                "content_block_start" => {
                                    if let (Some(idx), Some(cb)) =
                                        (event.index, event.content_block)
                                    {
                                        if cb.block_type == "tool_use" {
                                            tool_bufs.insert(
                                                idx,
                                                (
                                                    cb.id.unwrap_or_default(),
                                                    cb.name.unwrap_or_default(),
                                                    String::new(),
                                                ),
                                            );
                                        }
                                    }
                                }
                                "content_block_delta" => {
                                    if let Some(delta) = event.delta {
                                        match delta.delta_type.as_deref() {
                                            Some("text_delta") => {
                                                if let Some(text) = delta.text {
                                                    let _ =
                                                        tx.send(LlmChunk::TextDelta(text)).await;
                                                }
                                            }
                                            Some("input_json_delta") => {
                                                if let (Some(idx), Some(partial)) =
                                                    (event.index, delta.partial_json)
                                                {
                                                    if let Some(buf) = tool_bufs.get_mut(&idx) {
                                                        buf.2.push_str(&partial);
                                                    }
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                "content_block_stop" => {
                                    if let Some(idx) = event.index {
                                        if let Some((id, name, json_buf)) = tool_bufs.remove(&idx) {
                                            let input = serde_json::from_str(&json_buf).unwrap_or(
                                                serde_json::Value::Object(serde_json::Map::new()),
                                            );
                                            let _ = tx
                                                .send(LlmChunk::ToolUse { id, name, input })
                                                .await;
                                        }
                                    }
                                }
                                "message_delta" => {
                                    if let Some(u) = event.usage {
                                        output_tokens = u.output_tokens;
                                    }
                                }
                                "message_stop" => {
                                    let _ = tx
                                        .send(LlmChunk::Done {
                                            input_tokens,
                                            output_tokens,
                                        })
                                        .await;
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse> {
        let mut req_no_stream = req.clone();
        req_no_stream.stream = false;
        let body = self.build_body(&req_no_stream);

        let response = self
            .http
            .post(CLAUDE_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow!("Claude API error {}: {}", status, text));
        }

        let body = response.bytes().await?;
        let val: Value = serde_json::from_slice(&body).map_err(|e| {
            let preview: String = String::from_utf8_lossy(&body).chars().take(200).collect();
            anyhow!(
                "Claude response JSON decode error: {} (body preview: {})",
                e,
                preview
            )
        })?;
        let mut text = String::new();
        let mut tool_calls = Vec::new();

        if let Some(content) = val["content"].as_array() {
            for block in content {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = block["text"].as_str() {
                            text.push_str(t);
                        }
                    }
                    Some("tool_use") => {
                        tool_calls.push(ToolCall {
                            id: block["id"].as_str().unwrap_or("").to_string(),
                            name: block["name"].as_str().unwrap_or("").to_string(),
                            input: block["input"].clone(),
                        });
                    }
                    _ => {}
                }
            }
        }

        Ok(LlmResponse {
            content: text,
            tool_calls,
            input_tokens: val["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32,
            output_tokens: val["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32,
        })
    }
}
