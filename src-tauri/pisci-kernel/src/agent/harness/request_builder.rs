//! `RequestBuilder` — a centralised place to assemble an [`LlmRequest`]
//! with provider-specific tweaks applied in one spot instead of being
//! scattered across the agent loop, the scene factories and each LLM
//! client.
//!
//! Today's responsibilities (p12 minimum-viable):
//!
//! * Hold the common inputs (messages, system prompt, tools, model,
//!   max_tokens, stream flag, vision override).
//! * Emit [`LlmRequest`] tailored for a given [`ProviderKind`]:
//!   * Anthropic models accept Anthropic-shaped blocks directly; the
//!     existing `ClaudeClient` consumes them verbatim.
//!   * OpenAI / "Other" providers inherit the same internal shape; the
//!     per-client converter runs the block → tool_calls/messages
//!     transformation.
//!   * Provider-specific `max_tokens` ceilings are applied here so the
//!     call sites don't need to re-derive them.
//!
//! The builder is immutable once constructed — `build_for` clones the
//! relevant parts so the same builder can be reused across a retry loop
//! with different provider choices (helps the fallback flow).

use crate::agent::harness::ProviderKind;
use crate::llm::{LlmMessage, LlmRequest, ToolDef};

/// Hard per-provider ceilings on `max_tokens`.
///
/// These mirror the defaults most providers actually support today; a
/// user-configured value wins only when it is *below* the ceiling to
/// avoid the common "server rejects oversized max_tokens" failure mode.
const ANTHROPIC_MAX_TOKENS_CEILING: u32 = 8192;
const OPENAI_MAX_TOKENS_CEILING: u32 = 16_384;
const OTHER_MAX_TOKENS_CEILING: u32 = 8192;

#[derive(Debug, Clone)]
pub struct RequestBuilder {
    messages: Vec<LlmMessage>,
    system: Option<String>,
    tools: Vec<ToolDef>,
    model: String,
    max_tokens: u32,
    stream: bool,
    vision_override: Option<bool>,
}

impl RequestBuilder {
    pub fn new(
        messages: Vec<LlmMessage>,
        system: Option<String>,
        tools: Vec<ToolDef>,
        model: impl Into<String>,
        max_tokens: u32,
    ) -> Self {
        Self {
            messages,
            system,
            tools,
            model: model.into(),
            max_tokens,
            stream: true,
            vision_override: None,
        }
    }

    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    pub fn with_vision_override(mut self, vision: Option<bool>) -> Self {
        self.vision_override = vision;
        self
    }

    /// Produce an `LlmRequest` sized and tweaked for the target
    /// provider. This is a pure function of `self` — no hidden
    /// side-effects — so callers can safely reuse the builder across a
    /// fallback / retry sequence.
    pub fn build_for(&self, provider: ProviderKind) -> LlmRequest {
        let max_tokens = cap_max_tokens(self.max_tokens, provider);
        LlmRequest {
            messages: self.messages.clone(),
            system: self.system.clone(),
            tools: self.tools.clone(),
            model: self.model.clone(),
            max_tokens,
            stream: self.stream,
            vision_override: self.vision_override,
        }
    }
}

/// Cap a requested `max_tokens` at the provider's known ceiling. Returns
/// the original value when it is already below the ceiling (or when the
/// provider is `Other` and we have no authoritative ceiling).
pub fn cap_max_tokens(max_tokens: u32, provider: ProviderKind) -> u32 {
    let ceiling = match provider {
        ProviderKind::Anthropic => ANTHROPIC_MAX_TOKENS_CEILING,
        ProviderKind::OpenAi => OPENAI_MAX_TOKENS_CEILING,
        ProviderKind::Other => OTHER_MAX_TOKENS_CEILING,
    };
    max_tokens.min(ceiling)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ContentBlock, MessageContent};
    use serde_json::json;

    fn sample_msgs() -> Vec<LlmMessage> {
        vec![LlmMessage {
            role: "user".into(),
            content: MessageContent::Blocks(vec![ContentBlock::Text { text: "hi".into() }]),
        }]
    }

    fn sample_tool() -> ToolDef {
        ToolDef {
            name: "shell".into(),
            description: "run shell".into(),
            input_schema: json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        }
    }

    #[test]
    fn build_for_preserves_common_inputs() {
        let builder = RequestBuilder::new(
            sample_msgs(),
            Some("system prompt".into()),
            vec![sample_tool()],
            "claude-sonnet-4.5",
            2048,
        );
        let req = builder.build_for(ProviderKind::Anthropic);
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.system.as_deref(), Some("system prompt"));
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.model, "claude-sonnet-4.5");
        assert_eq!(req.max_tokens, 2048);
        assert!(req.stream);
        assert!(req.vision_override.is_none());
    }

    #[test]
    fn anthropic_caps_max_tokens_at_8192() {
        let builder = RequestBuilder::new(Vec::new(), None, Vec::new(), "claude-opus-4", 32_000);
        let req = builder.build_for(ProviderKind::Anthropic);
        assert_eq!(req.max_tokens, 8192);
    }

    #[test]
    fn openai_caps_max_tokens_at_16k() {
        let builder = RequestBuilder::new(Vec::new(), None, Vec::new(), "gpt-5", 32_000);
        let req = builder.build_for(ProviderKind::OpenAi);
        assert_eq!(req.max_tokens, 16_384);
    }

    #[test]
    fn other_providers_get_conservative_ceiling() {
        let builder = RequestBuilder::new(Vec::new(), None, Vec::new(), "qwen-max", 64_000);
        let req = builder.build_for(ProviderKind::Other);
        assert_eq!(req.max_tokens, 8192);
    }

    #[test]
    fn cap_does_not_inflate_below_ceiling() {
        assert_eq!(cap_max_tokens(512, ProviderKind::Anthropic), 512);
        assert_eq!(cap_max_tokens(1024, ProviderKind::OpenAi), 1024);
        assert_eq!(cap_max_tokens(4096, ProviderKind::Other), 4096);
    }

    #[test]
    fn builder_is_reusable_for_fallback() {
        let builder =
            RequestBuilder::new(sample_msgs(), None, Vec::new(), "claude-sonnet-4.5", 10_000)
                .with_stream(false)
                .with_vision_override(Some(false));
        let req_a = builder.build_for(ProviderKind::Anthropic);
        let req_b = builder.build_for(ProviderKind::OpenAi);
        assert_eq!(req_a.max_tokens, 8192);
        assert_eq!(req_b.max_tokens, 10_000);
        assert!(!req_a.stream);
        assert!(!req_b.stream);
        assert_eq!(req_a.vision_override, Some(false));
        assert_eq!(req_b.vision_override, Some(false));
    }
}
