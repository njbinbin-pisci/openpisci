//! Benchmark-friendly one-shot compression facade.
//!
//! Exposes Pisci's two compaction tiers as a simple JSON-in/JSON-out API
//! that external benchmark drivers (Python / CLI) can call without
//! linking against the `llm` module:
//!
//!  - **L1 (receipt demotion)**: runs `build_request_messages`, swapping
//!    old tool results for their minimal receipts. Zero LLM calls.
//!  - **L2 (rolling summary)**: runs `compact_summarise`, asking the real
//!    configured LLM to distill older messages into a structured summary.
//!    One LLM call.
//!
//! Used by `src-tauri/pisci-cli/src/bin/pisci_compact_one.rs` for the cross-framework
//! compression benchmark against Hermes / Engram / claw-compactor.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::llm::{
    build_client_with_timeout, estimate_message_tokens, ContentBlock, LlmMessage, MessageContent,
};

#[derive(Debug, Deserialize)]
pub struct InputMessage {
    pub role: String,
    #[serde(default)]
    pub content: String,
    /// Optional pre-formed content blocks for tool-heavy samples.
    /// When present, `content` is ignored.
    #[serde(default)]
    pub blocks: Option<Vec<InputBlock>>,
    #[serde(default)]
    pub ts: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputBlock {
    Text {
        text: String,
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
        /// Optional pre-computed minimal receipt (used by L1 demotion).
        /// When absent L1 will fall back to a generic truncation.
        #[serde(default)]
        minimal: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
pub struct BenchRequest {
    pub messages: Vec<InputMessage>,
    /// "L1" or "L2"
    pub mode: String,
    /// For L2: approx tokens to KEEP at the tail (older content is summarised).
    #[serde(default = "default_keep_tokens")]
    pub keep_tokens: usize,
    /// Optional config-dir override (defaults to the normal user app-data dir).
    #[serde(default)]
    pub config_dir: Option<String>,
    /// Optional model override (defaults to settings.model).
    #[serde(default)]
    pub model: Option<String>,
    /// Optional max_tokens override (defaults to settings.max_tokens clamped 512..=1024).
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Optional HTTP read timeout (defaults to settings.llm_read_timeout_secs).
    #[serde(default)]
    pub read_timeout_secs: Option<u32>,
}

fn default_keep_tokens() -> usize {
    2_000
}

#[derive(Debug, Serialize)]
pub struct BenchResponse {
    pub mode: String,
    pub compressed_text: String,
    /// Compressed messages (useful for downstream judge evaluation).
    pub compressed_messages: Vec<serde_json::Value>,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    pub llm_calls: u32,
    pub llm_input_tokens: u32,
    pub llm_output_tokens: u32,
    /// Wall-clock compression time in milliseconds.
    pub latency_ms: f64,
    /// Informational: provider and model actually used.
    pub provider: String,
    pub model: String,
    /// Phase 5 HARNESS mode: per-layer token attribution. Only populated
    /// when `mode == "HARNESS"`. Mirrors
    /// [`crate::agent::harness::context_builder::LayeredTokenBreakdown`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layered: Option<LayeredBreakdown>,
}

/// Phase 5 HARNESS mode — flat, JSON-friendly layered token breakdown.
/// Mirrors the internal `LayeredTokenBreakdown` so Python benchmark
/// scripts don't need to link against the harness crate.
#[derive(Debug, Serialize, Default)]
pub struct LayeredBreakdown {
    pub prompt_persona: u32,
    pub prompt_scene: u32,
    pub prompt_memory: u32,
    pub prompt_project: u32,
    pub prompt_platform_hint: u32,
    pub tool_def_tokens: u32,
    pub history_text_tokens: u32,
    pub history_tool_result_full_tokens: u32,
    pub history_tool_result_receipt_tokens: u32,
    pub rolling_summary_tokens: u32,
    pub state_frame_tokens: u32,
    pub vision_tokens: u32,
    pub request_overhead_tokens: u32,
    pub total: u32,
    /// Channel-utilization proxy: "useful payload" tokens (history +
    /// summary + state_frame) divided by `total`. Close to 1.0 means
    /// the context window is being used mostly for content; low values
    /// mean system-prompt / tool-def overhead dominates.
    pub channel_utilization: f32,
}

fn to_llm_message(m: &InputMessage) -> LlmMessage {
    if let Some(blocks) = &m.blocks {
        let out: Vec<ContentBlock> = blocks
            .iter()
            .map(|b| match b {
                InputBlock::Text { text } => ContentBlock::Text { text: text.clone() },
                InputBlock::ToolUse { id, name, input } => ContentBlock::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                },
                InputBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    minimal: _,
                } => ContentBlock::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                },
            })
            .collect();
        LlmMessage {
            role: m.role.clone(),
            content: MessageContent::Blocks(out),
        }
    } else {
        LlmMessage {
            role: m.role.clone(),
            content: MessageContent::text(&m.content),
        }
    }
}

fn collect_tool_minimals(messages: &[InputMessage]) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for m in messages {
        if let Some(blocks) = &m.blocks {
            for b in blocks {
                if let InputBlock::ToolResult {
                    tool_use_id,
                    minimal: Some(min),
                    ..
                } = b
                {
                    map.insert(tool_use_id.clone(), min.clone());
                }
            }
        }
    }
    map
}

fn message_to_json(m: &LlmMessage) -> serde_json::Value {
    serde_json::to_value(m).unwrap_or_else(|_| serde_json::json!({}))
}

fn render_flat_text(messages: &[LlmMessage]) -> String {
    messages
        .iter()
        .map(|m| {
            let role = m.role.to_uppercase();
            let text = match &m.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => (!text.is_empty()).then(|| text.clone()),
                        ContentBlock::ToolUse { name, input, .. } => Some(format!(
                            "[tool_use:{}] {}",
                            name,
                            serde_json::to_string(input).unwrap_or_default()
                        )),
                        ContentBlock::ToolResult { content, .. } => {
                            Some(format!("[tool_result] {}", content))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            format!("{}: {}", role, text)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub async fn compact_one(req: BenchRequest) -> Result<BenchResponse> {
    let start = std::time::Instant::now();

    let config_path = match &req.config_dir {
        Some(dir) => Path::new(dir).join("config.json"),
        None => crate::store::settings::Settings::default_config_path(),
    };
    let settings = crate::store::settings::Settings::load(&config_path)
        .with_context(|| format!("load settings from {}", config_path.display()))?;

    let provider = settings.provider.clone();
    let model = req.model.clone().unwrap_or_else(|| settings.model.clone());
    let api_key = settings.active_api_key().to_string();
    let base_url = settings.custom_base_url.clone();
    let read_timeout = req
        .read_timeout_secs
        .unwrap_or(settings.llm_read_timeout_secs);
    let max_tokens = req.max_tokens.unwrap_or(settings.max_tokens);

    let input_msgs: Vec<LlmMessage> = req.messages.iter().map(to_llm_message).collect();
    let original_tokens: usize = input_msgs.iter().map(estimate_message_tokens).sum();

    let mode = req.mode.trim().to_uppercase();
    let (compressed_msgs, llm_calls, llm_in, llm_out) = match mode.as_str() {
        "L1" => {
            let tool_minimals = collect_tool_minimals(&req.messages);
            let out = crate::agent::loop_::build_request_messages(
                &input_msgs,
                &tool_minimals,
                crate::agent::compaction::CTX_PRESERVE_RECENT_TURNS,
                crate::agent::compaction::CTX_KEEP_RECENT_TOOL_CARRIERS,
            );
            (out, 0u32, 0u32, 0u32)
        }
        "L1+" | "L1_PLUS" | "L1PLUS" => {
            // L1+ = deterministic rule preprocessing (RLE / stack folding
            // / base64 / ANSI / long paths / table rows) followed by the
            // standard receipt-demotion pipeline. Still zero-LLM.
            let pre: Vec<LlmMessage> = crate::agent::rule_preprocess::preprocess_messages(
                &input_msgs,
                crate::agent::rule_preprocess::Level::L1,
            );
            let tool_minimals = collect_tool_minimals(&req.messages);
            let out = crate::agent::loop_::build_request_messages(
                &pre,
                &tool_minimals,
                crate::agent::compaction::CTX_PRESERVE_RECENT_TURNS,
                crate::agent::compaction::CTX_KEEP_RECENT_TOOL_CARRIERS,
            );
            (out, 0u32, 0u32, 0u32)
        }
        "HARNESS" => {
            // Phase 5: run the full ContextBuilder::finalize pipeline
            // (demotion + supersede + pairing + layered breakdown).
            // Reports the same tier/layer accounting the real request
            // path uses, so the benchmark measures Pisci's actual
            // production context assembly.
            let tool_minimals = collect_tool_minimals(&req.messages);
            let lp =
                crate::agent::harness::LayeredPrompt::from_monolithic("You are a Pisci agent.");
            let tools: Vec<crate::llm::ToolDef> = vec![];
            let budget = crate::agent::harness::LayeredBudget::with_total(200_000);
            let fin = crate::agent::harness::context_builder::ContextBuilder::new(
                input_msgs.clone(),
                &tool_minimals,
                &lp,
                &tools,
                budget,
            )
            .finalize();
            // No LLM call in HARNESS mode — it's the deterministic
            // part of the stack. Stash the breakdown for the caller.
            let breakdown = &fin.breakdown;
            let useful: u32 = breakdown
                .history_text_tokens
                .saturating_add(breakdown.history_tool_result_full_tokens)
                .saturating_add(breakdown.history_tool_result_receipt_tokens)
                .saturating_add(breakdown.rolling_summary_tokens)
                .saturating_add(breakdown.state_frame_tokens);
            let total = breakdown.total();
            let layered = LayeredBreakdown {
                prompt_persona: breakdown.prompt.persona,
                prompt_scene: breakdown.prompt.scene,
                prompt_memory: breakdown.prompt.memory,
                prompt_project: breakdown.prompt.project,
                prompt_platform_hint: breakdown.prompt.platform_hint,
                tool_def_tokens: breakdown.tool_def_tokens,
                history_text_tokens: breakdown.history_text_tokens,
                history_tool_result_full_tokens: breakdown.history_tool_result_full_tokens,
                history_tool_result_receipt_tokens: breakdown.history_tool_result_receipt_tokens,
                rolling_summary_tokens: breakdown.rolling_summary_tokens,
                state_frame_tokens: breakdown.state_frame_tokens,
                vision_tokens: breakdown.vision_tokens,
                request_overhead_tokens: breakdown.request_overhead_tokens,
                total,
                channel_utilization: if total > 0 {
                    useful as f32 / total as f32
                } else {
                    0.0
                },
            };
            // Store the breakdown so we can emit it on the response
            // after the match arm. Tuple carries a marker via
            // `llm_calls = u32::MAX` to signal harness mode downstream.
            // We can't add a new field to the tuple without uglier
            // plumbing, so we handle emission below via a thread-local
            // Option<LayeredBreakdown>. Keep it simple by deviating
            // from the tuple shape:
            return {
                let compressed_text = render_flat_text(&fin.messages);
                let compressed_tokens: usize =
                    fin.messages.iter().map(estimate_message_tokens).sum();
                let latency_ms = start.elapsed().as_secs_f64() * 1000.0;
                Ok(BenchResponse {
                    mode,
                    compressed_text,
                    compressed_messages: fin.messages.iter().map(message_to_json).collect(),
                    original_tokens,
                    compressed_tokens,
                    llm_calls: 0,
                    llm_input_tokens: 0,
                    llm_output_tokens: 0,
                    latency_ms,
                    provider,
                    model,
                    layered: Some(layered),
                })
            };
        }
        "L2" => {
            let client = build_client_with_timeout(
                &provider,
                &api_key,
                if base_url.is_empty() {
                    None
                } else {
                    Some(&base_url)
                },
                read_timeout,
            );
            match crate::agent::loop_::compact_summarise(
                input_msgs.clone(),
                req.keep_tokens,
                client.as_ref(),
                &model,
                max_tokens,
                None,
            )
            .await
            {
                Some(out) => (out.messages, 1u32, out.input_tokens, out.output_tokens),
                None => (input_msgs.clone(), 0u32, 0u32, 0u32),
            }
        }
        other => anyhow::bail!("unknown mode {} (expected L1 or L2)", other),
    };

    let compressed_text = render_flat_text(&compressed_msgs);
    let compressed_tokens: usize = compressed_msgs.iter().map(estimate_message_tokens).sum();
    let latency_ms = start.elapsed().as_secs_f64() * 1000.0;

    Ok(BenchResponse {
        mode,
        compressed_text,
        compressed_messages: compressed_msgs.iter().map(message_to_json).collect(),
        original_tokens,
        compressed_tokens,
        llm_calls,
        llm_input_tokens: llm_in,
        llm_output_tokens: llm_out,
        latency_ms,
        provider,
        model,
        layered: None,
    })
}
