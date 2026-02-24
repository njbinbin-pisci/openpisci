/// OpenAI-compatible API client (Chat Completions, streaming SSE)
use super::{LlmChunk, LlmClient, LlmMessage, LlmRequest, LlmResponse, MessageContent, ToolCall};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::mpsc::Sender;

pub struct OpenAiClient {
    api_key: String,
    base_url: String,
    http: Client,
}

impl OpenAiClient {
    pub fn new(api_key: &str, base_url: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            http: Client::new(),
        }
    }

    fn convert_messages(&self, messages: &[LlmMessage]) -> Vec<Value> {
        messages.iter().map(|m| {
            let content = match &m.content {
                MessageContent::Text(t) => json!(t),
                MessageContent::Blocks(blocks) => {
                    let parts: Vec<Value> = blocks.iter().filter_map(|b| {
                        use super::ContentBlock;
                        match b {
                            ContentBlock::Text { text } => Some(json!({"type": "text", "text": text})),
                            ContentBlock::Image { source } => Some(json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", source.media_type, source.data)
                                }
                            })),
                            ContentBlock::ToolResult { tool_use_id, content, .. } => Some(json!({
                                "role": "tool",
                                "tool_call_id": tool_use_id,
                                "content": content
                            })),
                            _ => None,
                        }
                    }).collect();
                    json!(parts)
                }
            };
            json!({"role": m.role, "content": content})
        }).collect()
    }

    fn build_body(&self, req: &LlmRequest) -> Value {
        let messages = self.convert_messages(&req.messages);
        let mut body = json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "messages": messages,
            "stream": req.stream,
        });

        if let Some(sys) = &req.system {
            // Prepend system message
            if let Some(arr) = body["messages"].as_array_mut() {
                arr.insert(0, json!({"role": "system", "content": sys}));
            }
        }

        if !req.tools.is_empty() {
            let tools: Vec<Value> = req.tools.iter().map(|t| json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                }
            })).collect();
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
        }

        body
    }
}

#[async_trait]
impl LlmClient for OpenAiClient {
    async fn stream(&self, req: LlmRequest, tx: Sender<LlmChunk>) -> Result<()> {
        let mut req_stream = req.clone();
        req_stream.stream = true;
        let body = self.build_body(&req_stream);

        let url = format!("{}/chat/completions", self.base_url);
        let response = self.http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow!("OpenAI API error {}: {}", status, text));
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        // tool call accumulation: index -> (id, name, args_buf)
        let mut tool_bufs: std::collections::HashMap<usize, (String, String, String)> = std::collections::HashMap::new();
        let mut input_tokens = 0u32;
        let mut output_tokens = 0u32;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim().to_string();
                buffer = buffer[pos + 1..].to_string();

                if let Some(data) = line.strip_prefix("data: ") {
                    if data == "[DONE]" {
                        let _ = tx.send(LlmChunk::Done { input_tokens, output_tokens }).await;
                        return Ok(());
                    }
                    if let Ok(val) = serde_json::from_str::<Value>(data) {
                        // Usage
                        if let Some(usage) = val.get("usage") {
                            input_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
                            output_tokens = usage["completion_tokens"].as_u64().unwrap_or(0) as u32;
                        }

                        if let Some(choices) = val["choices"].as_array() {
                            for choice in choices {
                                let delta = &choice["delta"];

                                // Text delta
                                if let Some(text) = delta["content"].as_str() {
                                    if !text.is_empty() {
                                        let _ = tx.send(LlmChunk::TextDelta(text.to_string())).await;
                                    }
                                }

                                // Tool calls
                                if let Some(tool_calls) = delta["tool_calls"].as_array() {
                                    for tc in tool_calls {
                                        let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                                        let entry = tool_bufs.entry(idx).or_insert_with(|| {
                                            let id = tc["id"].as_str().unwrap_or("").to_string();
                                            let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                                            (id, name, String::new())
                                        });
                                        if let Some(args) = tc["function"]["arguments"].as_str() {
                                            entry.2.push_str(args);
                                        }
                                    }
                                }

                                // Finish reason
                                if let Some("tool_calls") = choice["finish_reason"].as_str() {
                                    for (_, (id, name, args_buf)) in tool_bufs.drain() {
                                        let input = serde_json::from_str(&args_buf)
                                            .unwrap_or(Value::Object(serde_json::Map::new()));
                                        let _ = tx.send(LlmChunk::ToolUse { id, name, input }).await;
                                    }
                                }
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

        let url = format!("{}/chat/completions", self.base_url);
        let response = self.http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow!("OpenAI API error {}: {}", status, text));
        }

        let val: Value = response.json().await?;
        let message = &val["choices"][0]["message"];
        let text = message["content"].as_str().unwrap_or("").to_string();

        let mut tool_calls = Vec::new();
        if let Some(tcs) = message["tool_calls"].as_array() {
            for tc in tcs {
                let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                let input = serde_json::from_str(args_str)
                    .unwrap_or(Value::Object(serde_json::Map::new()));
                tool_calls.push(ToolCall {
                    id: tc["id"].as_str().unwrap_or("").to_string(),
                    name: tc["function"]["name"].as_str().unwrap_or("").to_string(),
                    input,
                });
            }
        }

        Ok(LlmResponse {
            content: text,
            tool_calls,
            input_tokens: val["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            output_tokens: val["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32,
        })
    }
}
