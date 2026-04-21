//! Layered system prompt.
//!
//! The layers follow hermes-agent's `_build_system_prompt` ordering
//! (see `references/hermes-agent/run_agent.py` around L3638):
//!
//!   L0 persona         — identity + decision tree (static, cache-friendly)
//!   L1 scene           — scene overlay (e.g. main-chat / koi / fish)
//!   L2 memory          — memory snapshot + task state
//!   L3 project         — project-instruction files (PISCI.md etc.)
//!   L4 (tools)         — tool schemas live in `ToolRegistry::to_tool_defs`,
//!                         not in the system prompt itself.
//!   L? platform_hint   — platform-specific formatting tip
//!
//! Each layer is [`Arc<str>`] so a given layer can be shared verbatim
//! across harnesses (important for provider prefix-caching).

use std::sync::Arc;

use crate::llm::estimate_tokens;

/// Inject mode for tool definitions.
///
/// Minimal is the default going forward (p2-dual-schema); Full is used on
/// `recall_tool_result` or during schema-correction (p3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolDefMode {
    #[default]
    Minimal,
    Full,
}

#[derive(Debug, Clone, Default)]
pub struct LayeredPrompt {
    pub persona: Arc<str>,
    pub scene: Option<Arc<str>>,
    pub memory: Option<Arc<str>>,
    pub project: Option<Arc<str>>,
    pub platform_hint: Option<Arc<str>>,
    pub tool_mode: ToolDefMode,
}

impl LayeredPrompt {
    /// Build from a single pre-assembled string. Used as a migration shim
    /// in p1 where the call sites still own their own prompt assembly —
    /// later phases split into proper layers.
    pub fn from_monolithic(text: impl Into<String>) -> Self {
        Self {
            persona: Arc::from(text.into()),
            ..Self::default()
        }
    }

    /// Concatenate all present layers into a single system prompt string.
    ///
    /// Layers are joined by a blank line so each layer can use its own
    /// top-level markdown heading.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(self.persona.len() + 256);
        out.push_str(&self.persona);
        for layer in [
            self.scene.as_ref(),
            self.memory.as_ref(),
            self.project.as_ref(),
            self.platform_hint.as_ref(),
        ]
        .iter()
        .copied()
        .flatten()
        {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            if !layer.trim().is_empty() {
                out.push('\n');
                out.push_str(layer);
            }
        }
        out
    }

    /// Token counts per layer (for telemetry). Called from
    /// [`super::ContextBuilder`]; separated so p8 can extend it without
    /// breaking the API.
    pub fn token_breakdown(&self) -> LayeredPromptTokens {
        LayeredPromptTokens {
            persona: estimate_tokens(&self.persona) as u32,
            scene: self.scene.as_deref().map(estimate_tokens).unwrap_or(0) as u32,
            memory: self.memory.as_deref().map(estimate_tokens).unwrap_or(0) as u32,
            project: self.project.as_deref().map(estimate_tokens).unwrap_or(0) as u32,
            platform_hint: self
                .platform_hint
                .as_deref()
                .map(estimate_tokens)
                .unwrap_or(0) as u32,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayeredPromptTokens {
    pub persona: u32,
    pub scene: u32,
    pub memory: u32,
    pub project: u32,
    pub platform_hint: u32,
}

impl LayeredPromptTokens {
    pub fn total(&self) -> u32 {
        self.persona
            .saturating_add(self.scene)
            .saturating_add(self.memory)
            .saturating_add(self.project)
            .saturating_add(self.platform_hint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monolithic_prompt_round_trips() {
        let p = LayeredPrompt::from_monolithic("you are pisci");
        assert_eq!(p.render().trim(), "you are pisci");
        assert_eq!(p.tool_mode, ToolDefMode::Minimal);
    }

    #[test]
    fn layered_prompt_concatenates_in_order() {
        let p = LayeredPrompt {
            persona: Arc::from("# Identity\nYou are pisci."),
            scene: Some(Arc::from("# Scene\nmain chat")),
            memory: Some(Arc::from("# Memory\nno notes")),
            project: Some(Arc::from("# Project\nnone")),
            platform_hint: Some(Arc::from("# Platform\nwindows")),
            tool_mode: ToolDefMode::Minimal,
        };
        let rendered = p.render();
        // Check ordering
        let i_persona = rendered.find("# Identity").unwrap();
        let i_scene = rendered.find("# Scene").unwrap();
        let i_memory = rendered.find("# Memory").unwrap();
        let i_project = rendered.find("# Project").unwrap();
        let i_platform = rendered.find("# Platform").unwrap();
        assert!(i_persona < i_scene);
        assert!(i_scene < i_memory);
        assert!(i_memory < i_project);
        assert!(i_project < i_platform);
    }

    #[test]
    fn empty_layers_do_not_pollute_output() {
        let p = LayeredPrompt {
            persona: Arc::from("base"),
            scene: Some(Arc::from("")),
            memory: None,
            project: Some(Arc::from("   ")),
            platform_hint: None,
            tool_mode: ToolDefMode::Minimal,
        };
        let rendered = p.render();
        assert!(rendered.starts_with("base"));
        // No phantom extra whitespace-only sections.
        assert!(!rendered.contains("\n\n\n\n"));
    }

    #[test]
    fn token_breakdown_total_matches_sum_of_parts() {
        let p = LayeredPrompt {
            persona: Arc::from("one two three four"),
            scene: Some(Arc::from("five six")),
            memory: None,
            project: Some(Arc::from("seven eight nine")),
            platform_hint: None,
            tool_mode: ToolDefMode::Minimal,
        };
        let bd = p.token_breakdown();
        assert_eq!(
            bd.total(),
            bd.persona + bd.scene + bd.memory + bd.project + bd.platform_hint
        );
        assert!(bd.persona > 0);
        assert!(bd.scene > 0);
        assert_eq!(bd.memory, 0);
        assert!(bd.project > 0);
    }
}
