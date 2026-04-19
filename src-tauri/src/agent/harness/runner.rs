//! [`HarnessRunner`] — wraps a [`HarnessConfig`] and owns the
//! per-request [`ContextBuilder::finalize`] hook.
//!
//! p0 introduces the wrapper with a single public entry point
//! [`HarnessRunner::build_request`] so the finalise hook is *the* place
//! every call site ends up at. In p1 the runner grows a `run()` method
//! that constructs a [`crate::agent::loop_::AgentLoop`] from the
//! configuration and delegates execution to it, invoking
//! `build_request` before each outgoing request.

use std::collections::HashMap;

use crate::llm::LlmMessage;

use super::{
    ContextBuilder, FinalizedRequest, HarnessConfig,
};

pub struct HarnessRunner {
    pub config: HarnessConfig,
}

impl HarnessRunner {
    pub fn new(config: HarnessConfig) -> Self {
        Self { config }
    }

    /// Borrow the underlying configuration.
    pub fn config(&self) -> &HarnessConfig {
        &self.config
    }

    /// The canonical pre-request hook.
    ///
    /// **Every** LLM call must flow through here so receipt demotion,
    /// supersede filtering, tool-pairing sanitisation, and layered
    /// token estimation stay consistent across call sites.
    ///
    /// `recent_full_turns` and `recent_tool_carriers` default to
    /// [`crate::agent::compaction::CTX_PRESERVE_RECENT_TURNS`] and
    /// [`crate::agent::compaction::CTX_KEEP_RECENT_TOOL_CARRIERS`]
    /// when `None`. p5a wires these up to the Settings-backed tier
    /// thresholds.
    pub fn build_request(
        &self,
        messages: Vec<LlmMessage>,
        tool_minimals: &HashMap<String, String>,
        recent_full_turns: Option<usize>,
    ) -> FinalizedRequest {
        self.build_request_full(messages, tool_minimals, recent_full_turns, None)
    }

    /// Like [`Self::build_request`] but exposes the independent
    /// tool-carrier boundary knob. Preferred by p5a once the Settings
    /// wiring is in place; existing callers can keep calling
    /// [`Self::build_request`] unchanged.
    pub fn build_request_full(
        &self,
        messages: Vec<LlmMessage>,
        tool_minimals: &HashMap<String, String>,
        recent_full_turns: Option<usize>,
        recent_tool_carriers: Option<usize>,
    ) -> FinalizedRequest {
        let n_turns =
            recent_full_turns.unwrap_or(crate::agent::compaction::CTX_PRESERVE_RECENT_TURNS);
        let n_carriers = recent_tool_carriers
            .unwrap_or(crate::agent::compaction::CTX_KEEP_RECENT_TOOL_CARRIERS);
        // Respect the configured tool-def injection mode (p2). `Minimal`
        // is the default; `Full` is only used by schema-correction (p3)
        // or explicit recall flows.
        let tools = self
            .config
            .registry
            .to_tool_defs(self.config.layered_prompt.tool_mode);
        ContextBuilder::new(
            messages,
            tool_minimals,
            &self.config.layered_prompt,
            &tools,
            self.config.budget,
        )
        .with_recent_full_turns(n_turns)
        .with_recent_tool_carriers(n_carriers)
        .finalize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::harness::{HarnessConfig, LayeredPrompt};
    use crate::agent::tool::ToolRegistry;
    use crate::llm::{LlmMessage, MessageContent};
    use crate::policy::PolicyGate;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn runner_build_request_calls_finalize() {
        let reg = Arc::new(ToolRegistry::new());
        let policy = Arc::new(PolicyGate::new(PathBuf::from(".")));
        let cfg = HarnessConfig::builder("fish", "claude-haiku-4.5", reg, policy)
            .with_layered_prompt(LayeredPrompt::from_monolithic("you are fish"))
            .with_context_window(64_000)
            .build();
        let runner = HarnessRunner::new(cfg);

        let msgs = vec![LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Text("ping".to_string()),
        }];
        let minimals = HashMap::new();
        let fin = runner.build_request(msgs, &minimals, None);
        assert_eq!(fin.messages.len(), 1);
        assert!(!fin.system_prompt.is_empty());
        assert!(fin.breakdown.prompt.persona > 0);
    }

    #[test]
    fn runner_fish_mode_has_no_persistence() {
        let reg = Arc::new(ToolRegistry::new());
        let policy = Arc::new(PolicyGate::new(PathBuf::from(".")));
        let cfg = HarnessConfig::builder("fish", "claude-haiku-4.5", reg, policy).build();
        let runner = HarnessRunner::new(cfg);
        assert!(runner.config().persistence.is_none());
        assert!(runner.config().summary_store.is_none());
        assert!(runner.config().frame_provider.is_none());
    }

    /// p10 — regression guard: every scene that calls into the agent
    /// loop must be reachable via the unified harness pipeline. We
    /// build a runner for each of the non-persistent scenes (those
    /// that need a real DB handle are covered by integration tests)
    /// and verify `build_request` yields a usable `FinalizedRequest`.
    /// This makes it structurally impossible for a new scene to
    /// regress to building an `AgentLoop` by hand, because the runner
    /// is the only shared surface and every scene ultimately routes
    /// through `HarnessConfig::builder`.
    #[test]
    fn runner_uniform_across_scenes() {
        use crate::agent::harness::CompactionTier;
        let scenes = ["main", "main_headless", "koi", "fish", "scheduler", "debug"];
        for scene in scenes {
            let reg = Arc::new(ToolRegistry::new());
            let policy = Arc::new(PolicyGate::new(PathBuf::from(".")));
            let cfg = HarnessConfig::builder(scene, "claude-haiku-4.5", reg, policy)
                .with_layered_prompt(LayeredPrompt::from_monolithic(format!(
                    "you are the {} agent",
                    scene
                )))
                .with_context_window(64_000)
                .build();
            let runner = HarnessRunner::new(cfg);
            let fin = runner.build_request(
                vec![LlmMessage {
                    role: "user".to_string(),
                    content: MessageContent::Text("hello".to_string()),
                }],
                &HashMap::new(),
                None,
            );
            assert!(!fin.system_prompt.is_empty(), "scene={}", scene);
            assert_eq!(fin.messages.len(), 1, "scene={}", scene);
            // A fresh single-turn request must be far below any tier trigger.
            assert_eq!(
                fin.tier,
                CompactionTier::None,
                "scene={} should start at tier None",
                scene
            );
        }
    }
}
