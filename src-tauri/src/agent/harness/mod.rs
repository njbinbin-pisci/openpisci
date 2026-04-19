//! Unified agent harness.
//!
//! A "harness" is the engineering envelope around the raw LLM call —
//! layered system prompt, tool schemas, memory, history, state frame, and
//! request assembly — shared between the pisci chat, koi, fish, scheduler
//! and debug loops. The goal is that each call site only fills a
//! [`HarnessConfig`] and hands it to a runner; all compaction / supersede /
//! sanitisation / telemetry lives inside this module.
//!
//! This module is being introduced incrementally (see the
//! `unified-harness-context` plan). p0 lays down the types and the
//! [`ContextBuilder::finalize`] hook; p1 migrates the five existing call
//! sites; later phases add dual-schema tools, async rolling summary,
//! state frame, layered telemetry, etc.

pub mod budget;
pub mod config;
pub mod context_builder;
pub mod layered_prompt;
pub mod request_builder;
pub mod runner;

pub use budget::{CompactionTier, LayeredBudget};
pub use config::{HarnessConfig, ProviderKind, StateFrameProvider, SummaryStore};
pub use context_builder::{ContextBuilder, FinalizedRequest, LayeredTokenBreakdown};
pub use layered_prompt::{LayeredPrompt, LayeredPromptTokens, ToolDefMode};
pub use request_builder::{cap_max_tokens, RequestBuilder};
pub use runner::HarnessRunner;
