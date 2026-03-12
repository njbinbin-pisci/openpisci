/// OpenAI-compatible API client (Chat Completions, streaming SSE)
use super::{
    ContentBlock, LlmChunk, LlmClient, LlmMessage, LlmRequest, LlmResponse, MessageContent,
    ToolCall,
};
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

/// Returns true if the model name indicates vision/multimodal capability.
/// Conservative: only well-known vision models are listed.
/// Unknown models → no vision (safe default to avoid 400 errors).
pub fn model_supports_vision(model: &str) -> bool {
    let m = model.to_lowercase();
    // OpenAI vision-capable models
    m.contains("gpt-4o")
        || m.contains("gpt-4-vision")
        || m.contains("gpt-4-turbo")
        || m.contains("o3")
        // Qwen — all qwen3+ and qwen-vl support vision
        || m.contains("qwen-vl")
        || m.contains("qwen3")
        || m.contains("qwen2.5-vl")
        || m.contains("qvq")
        // Claude 3+ (all support vision)
        || m.contains("claude-3")
        || m.contains("claude-sonnet")
        || m.contains("claude-haiku")
        || m.contains("claude-opus")
        // Gemini
        || m.contains("gemini")
        // MiniMax / Kimi with vision
        || m.contains("abab6.5")
}

impl OpenAiClient {
    pub fn new(api_key: &str, base_url: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            http: Client::new(),
        }
    }

    /// Convert an Image block to a safe text placeholder.
    fn image_placeholder(is_latest: bool) -> ContentBlock {
        let msg = if is_latest {
            "[图片/截图已捕获 — 如需查看请使用 browser screenshot 工具重新截图]".to_string()
        } else {
            "[历史截图已省略 — 仅保留最近一轮截图以节省上下文]".to_string()
        };
        ContentBlock::Text { text: msg }
    }

    /// Preprocess messages: strip or downgrade Image blocks according to vision support.
    ///
    /// Rules:
    /// - Non-vision model: replace ALL Image blocks with text placeholders.
    /// - Vision model: keep Image blocks only from the LAST assistant/tool turn;
    ///   replace all older Image blocks with text placeholders.
    fn strip_images(&self, messages: &[LlmMessage], vision: bool) -> Vec<LlmMessage> {
        if !vision {
            // Strip all images
            return messages
                .iter()
                .map(|m| {
                    let content = match &m.content {
                        MessageContent::Blocks(blocks) => {
                            let new_blocks: Vec<ContentBlock> = blocks
                                .iter()
                                .map(|b| {
                                    if matches!(b, ContentBlock::Image { .. }) {
                                        Self::image_placeholder(false)
                                    } else {
                                        b.clone()
                                    }
                                })
                                .collect();
                            MessageContent::Blocks(new_blocks)
                        }
                        other => other.clone(),
                    };
                    LlmMessage {
                        role: m.role.clone(),
                        content,
                    }
                })
                .collect();
        }

        // Vision model: find index of the LAST message containing an Image block
        let last_image_msg = messages.iter().rposition(|m| {
            if let MessageContent::Blocks(blocks) = &m.content {
                blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Image { .. }))
            } else {
                false
            }
        });

        messages
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let is_latest_image_msg = last_image_msg == Some(i);
                let content = match &m.content {
                    MessageContent::Blocks(blocks) => {
                        let new_blocks: Vec<ContentBlock> = blocks
                            .iter()
                            .map(|b| {
                                if matches!(b, ContentBlock::Image { .. }) {
                                    if is_latest_image_msg {
                                        b.clone() // Keep the latest image for vision models
                                    } else {
                                        Self::image_placeholder(false) // Replace older images
                                    }
                                } else {
                                    b.clone()
                                }
                            })
                            .collect();
                        MessageContent::Blocks(new_blocks)
                    }
                    other => other.clone(),
                };
                LlmMessage {
                    role: m.role.clone(),
                    content,
                }
            })
            .collect()
    }

    fn convert_messages(&self, messages: &[LlmMessage], vision: bool) -> Vec<Value> {
        // Pre-pass: build a set of indices that are "safe" to include.
        // A tool_calls message is only safe if ALL its tool_call_ids are satisfied by
        // immediately following tool-result messages. A tool_result message is only safe
        // if it is preceded by a tool_calls message that contains its id.
        // We do this by scanning forward and marking unsafe indices to skip.
        let n = messages.len();
        let mut skip = vec![false; n];

        let mut i = 0;
        while i < n {
            let m = &messages[i];
            // Check if this is an assistant message with tool_calls
            let tool_call_ids: Vec<String> = if let MessageContent::Blocks(blocks) = &m.content {
                blocks
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::ToolUse { id, .. } = b {
                            Some(id.clone())
                        } else {
                            None
                        }
                    })
                    .collect()
            } else {
                vec![]
            };

            if !tool_call_ids.is_empty() {
                // Collect the tool_call_ids that are satisfied by immediately following messages
                let mut satisfied: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                let mut j = i + 1;
                while j < n {
                    if let MessageContent::Blocks(blocks) = &messages[j].content {
                        let has_result = blocks
                            .iter()
                            .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
                        if has_result {
                            for b in blocks {
                                if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                                    satisfied.insert(tool_use_id.clone());
                                }
                            }
                            j += 1;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                // If any tool_call_id is not satisfied, skip this entire tool_calls+results block
                let all_satisfied = tool_call_ids.iter().all(|id| satisfied.contains(id));
                if !all_satisfied {
                    tracing::warn!(
                        "Skipping tool_calls message with unsatisfied ids {:?} (satisfied: {:?})",
                        tool_call_ids,
                        satisfied
                    );
                    skip[i] = true;
                    // Also skip the immediately following tool-result messages for this block
                    let mut k = i + 1;
                    while k < n {
                        if let MessageContent::Blocks(blocks) = &messages[k].content {
                            if blocks
                                .iter()
                                .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
                            {
                                skip[k] = true;
                                k += 1;
                                continue;
                            }
                        }
                        break;
                    }
                }
            }
            i += 1;
        }

        // Debug: log the pre-pass skip decisions
        for (idx, m) in messages.iter().enumerate() {
            let summary = match &m.content {
                MessageContent::Text(t) => format!("text({} chars)", t.len()),
                MessageContent::Blocks(blocks) => {
                    let uses: Vec<_> = blocks
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::ToolUse { id, name, .. } = b {
                                Some(format!("use({name}/{id})"))
                            } else {
                                None
                            }
                        })
                        .collect();
                    let results: Vec<_> = blocks
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                                Some(format!("result({tool_use_id})"))
                            } else {
                                None
                            }
                        })
                        .collect();
                    let texts: usize = blocks
                        .iter()
                        .filter(|b| matches!(b, ContentBlock::Text { .. }))
                        .count();
                    format!("blocks[uses={uses:?} results={results:?} texts={texts}]")
                }
            };
            tracing::debug!(
                "convert_messages pre-pass [{idx}] role={} skip={} content={}",
                m.role,
                skip[idx],
                summary
            );
        }

        let mut result: Vec<Value> = Vec::new();
        // Images from tool results that need to be appended as a separate user message
        // right after the tool messages (OpenAI format requires this).
        let mut pending_vision: Vec<Value> = Vec::new();

        for (idx, m) in messages.iter().enumerate() {
            if skip[idx] {
                tracing::debug!("convert_messages [{idx}] SKIPPED (pre-pass)");
                continue;
            }

            // Flush any pending vision images before starting a new non-tool message
            // (so they appear immediately after the last tool message).
            if !pending_vision.is_empty() && m.role != "tool" {
                tracing::debug!(
                    "convert_messages [{idx}] flushing {} pending_vision images before role={}",
                    pending_vision.len(),
                    m.role
                );
                result.push(json!({
                    "role": "user",
                    "content": std::mem::take(&mut pending_vision)
                }));
            }

            // Defense: skip orphaned tool-result messages that have no preceding tool_calls.
            // These can appear when context is truncated mid-turn.
            if let MessageContent::Blocks(blocks) = &m.content {
                let has_tool_result = blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
                if has_tool_result {
                    let last_role = result
                        .last()
                        .and_then(|v| v["role"].as_str())
                        .unwrap_or("none");
                    let last_has_tool_calls = result
                        .last()
                        .and_then(|v| v["tool_calls"].as_array())
                        .map(|a| !a.is_empty())
                        .unwrap_or(false);
                    if !last_has_tool_calls {
                        tracing::warn!(
                            "convert_messages [{idx}] SKIP orphaned tool_result (last result role={last_role}, has_tool_calls={last_has_tool_calls})"
                        );
                        continue;
                    }
                }
            }

            match &m.content {
                MessageContent::Text(t) => {
                    result.push(json!({"role": m.role, "content": t}));
                }
                MessageContent::Blocks(blocks) => {
                    let has_tool_use = blocks
                        .iter()
                        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
                    let has_tool_result = blocks
                        .iter()
                        .any(|b| matches!(b, ContentBlock::ToolResult { .. }));

                    if has_tool_result {
                        // OpenAI: each ToolResult → separate "tool" role message (content must be string).
                        // Image blocks are collected and will become a "user" message right after.
                        for block in blocks {
                            match block {
                                ContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                    ..
                                } => {
                                    result.push(json!({
                                        "role": "tool",
                                        "tool_call_id": tool_use_id,
                                        "content": content
                                    }));
                                }
                                ContentBlock::Image { source } if vision => {
                                    pending_vision.push(json!({
                                        "type": "image_url",
                                        "image_url": {
                                            "url": format!("data:{};base64,{}", source.media_type, source.data)
                                        }
                                    }));
                                }
                                ContentBlock::Image { .. } => {
                                    // Non-vision model — image already replaced by strip_images(),
                                    // this branch is a safety fallback; simply skip.
                                }
                                ContentBlock::Text { text } if !text.is_empty() => {
                                    // Text mixed into a tool-result block would break the OpenAI
                                    // message ordering contract — drop it and log a warning.
                                    let preview: String = text.chars().take(80).collect();
                                    tracing::warn!(
                                        "convert_messages: dropping Text block inside tool-result message to avoid API error (text={:?})",
                                        preview
                                    );
                                }
                                _ => {}
                            }
                        }
                    } else if has_tool_use {
                        let mut text_content = String::new();
                        let mut tool_calls: Vec<Value> = Vec::new();

                        for block in blocks {
                            match block {
                                ContentBlock::Text { text } => text_content.push_str(text),
                                ContentBlock::ToolUse { id, name, input } => {
                                    tool_calls.push(json!({
                                        "id": id,
                                        "type": "function",
                                        "function": {
                                            "name": name,
                                            "arguments": serde_json::to_string(input)
                                                .unwrap_or_else(|_| "{}".to_string())
                                        }
                                    }));
                                }
                                // Images inside a ToolUse message are unusual; skip silently.
                                _ => {}
                            }
                        }

                        let mut msg = json!({
                            "role": "assistant",
                            "tool_calls": tool_calls,
                            "content": Value::Null
                        });
                        if !text_content.is_empty() {
                            msg["content"] = json!(text_content);
                        }
                        result.push(msg);
                    } else {
                        // Regular user/assistant message — may contain text + images.
                        let mut parts: Vec<Value> = Vec::new();
                        for b in blocks {
                            match b {
                                ContentBlock::Text { text } if !text.is_empty() => {
                                    parts.push(json!({"type": "text", "text": text}));
                                }
                                ContentBlock::Image { source } if vision => {
                                    parts.push(json!({
                                        "type": "image_url",
                                        "image_url": {
                                            "url": format!("data:{};base64,{}", source.media_type, source.data)
                                        }
                                    }));
                                }
                                // Non-vision model: Image already replaced upstream; skip here.
                                _ => {}
                            }
                        }

                        if parts.is_empty() {
                            continue;
                        }

                        // Collapse single-text to plain string (cleaner API payload)
                        if parts.len() == 1 {
                            if let Some(text) = parts[0]["text"].as_str() {
                                result.push(json!({"role": m.role, "content": text}));
                                continue;
                            }
                        }
                        result.push(json!({"role": m.role, "content": parts}));
                    }
                }
            }
        }

        // Flush any remaining pending vision images
        if !pending_vision.is_empty() {
            result.push(json!({
                "role": "user",
                "content": pending_vision
            }));
        }

        // Debug: log the final message sequence sent to the API
        let seq: Vec<String> = result
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let role = v["role"].as_str().unwrap_or("?");
                let detail = if let Some(tcs) = v["tool_calls"].as_array() {
                    let ids: Vec<_> = tcs.iter().filter_map(|tc| tc["id"].as_str()).collect();
                    format!("tool_calls{ids:?}")
                } else if v["tool_call_id"].is_string() {
                    format!("tool_call_id={}", v["tool_call_id"].as_str().unwrap_or("?"))
                } else {
                    let content_len = v["content"].as_str().map(|s| s.len()).unwrap_or(0);
                    format!("content({content_len} chars)")
                };
                format!("[{i}]{role}:{detail}")
            })
            .collect();
        tracing::debug!("convert_messages final sequence: {}", seq.join(" → "));

        result
    }

    fn build_body(&self, req: &LlmRequest) -> Value {
        let vision = req
            .vision_override
            .unwrap_or_else(|| model_supports_vision(&req.model));
        tracing::info!(
            "build_body: model={} vision_override={:?} vision={}",
            req.model,
            req.vision_override,
            vision
        );
        let stripped = self.strip_images(&req.messages, vision);
        let messages = self.convert_messages(&stripped, vision);
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
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    })
                })
                .collect();
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
        let response = self
            .http
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
        let mut tool_bufs: std::collections::HashMap<usize, (String, String, String)> =
            std::collections::HashMap::new();
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
                        // Drain any tool calls that arrived before [DONE]
                        for (_, (id, name, args_buf)) in tool_bufs.drain() {
                            let input = serde_json::from_str(&args_buf)
                                .unwrap_or(Value::Object(serde_json::Map::new()));
                            let _ = tx.send(LlmChunk::ToolUse { id, name, input }).await;
                        }
                        let _ = tx
                            .send(LlmChunk::Done {
                                input_tokens,
                                output_tokens,
                            })
                            .await;
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
                                        let _ =
                                            tx.send(LlmChunk::TextDelta(text.to_string())).await;
                                    }
                                }

                                // Tool calls
                                if let Some(tool_calls) = delta["tool_calls"].as_array() {
                                    for tc in tool_calls {
                                        let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                                        let entry = tool_bufs.entry(idx).or_insert_with(|| {
                                            let id = tc["id"].as_str().unwrap_or("").to_string();
                                            let name = tc["function"]["name"]
                                                .as_str()
                                                .unwrap_or("")
                                                .to_string();
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
                                        let _ =
                                            tx.send(LlmChunk::ToolUse { id, name, input }).await;
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
        let response = self
            .http
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
        let choices = val["choices"]
            .as_array()
            .ok_or_else(|| anyhow!("OpenAI response missing 'choices' field"))?;
        if choices.is_empty() {
            return Err(anyhow!("OpenAI response returned empty choices"));
        }
        let message = &choices[0]["message"];
        let text = message["content"].as_str().unwrap_or("").to_string();

        let mut tool_calls = Vec::new();
        if let Some(tcs) = message["tool_calls"].as_array() {
            for tc in tcs {
                let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                let input =
                    serde_json::from_str(args_str).unwrap_or(Value::Object(serde_json::Map::new()));
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
