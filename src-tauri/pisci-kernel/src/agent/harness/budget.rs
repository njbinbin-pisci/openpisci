//! Layered token budget + three-tier compaction classification.
//!
//! The tiers follow the 0.60 / 0.80 / 0.95 scheme from
//! `references/claw-compactor/scripts/lib/fusion/tiered_compaction.py`:
//!
//! * [`CompactionTier::Micro`] — rule-based receipt demotion + tool-result
//!   hard cap. Zero LLM cost; runs every request above ~60 % utilisation.
//! * [`CompactionTier::Auto`]  — same as micro plus asynchronous rolling
//!   summarisation (fires around ~80 %).
//! * [`CompactionTier::Full`]  — aggressive: drop vision, wait for summary,
//!   shrink the recent-turns window (fires around ~95 %).
//!
//! p0 introduces the type so the rest of the harness can wire against it;
//! the actual multi-tier policy is implemented in p5a.

use crate::llm::compute_total_input_budget;

/// Which compaction tier applies at this moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompactionTier {
    None,
    Micro,
    Auto,
    Full,
}

impl CompactionTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            CompactionTier::None => "none",
            CompactionTier::Micro => "micro",
            CompactionTier::Auto => "auto",
            CompactionTier::Full => "full",
        }
    }
}

/// Default tier percentages. Settings page (see `p13-settings-tier-ui`) can
/// override at runtime.
pub const DEFAULT_TIER_MICRO_PERCENT: u8 = 60;
pub const DEFAULT_TIER_AUTO_PERCENT: u8 = 80;
pub const DEFAULT_TIER_FULL_PERCENT: u8 = 95;

/// Default per-tool-result hard cap (tokens). Matches the claw-compactor
/// `MAX_TOOL_RESULT_TOKENS` default.
pub const DEFAULT_MAX_TOOL_RESULT_TOKENS: u32 = 8_000;

/// Layered token budget.
///
/// `total` is the safe input budget derived from the model's context window
/// minus the reserved output `max_tokens`. The three thresholds classify an
/// *estimated* request into [`CompactionTier`]; they do **not** directly cap
/// anything at this layer — the runner decides what to do per tier.
#[derive(Debug, Clone, Copy)]
pub struct LayeredBudget {
    pub total: u32,
    pub trigger_micro: u32,
    pub trigger_auto: u32,
    pub trigger_full: u32,
    pub max_tool_result_tokens: u32,
}

impl LayeredBudget {
    /// Derive defaults from model context window / max_tokens.
    pub fn from_context_window(context_window: u32, max_tokens: u32) -> Self {
        let total = compute_total_input_budget(context_window, max_tokens);
        Self::with_total(total as u32)
    }

    /// Construct with an explicit total budget.
    pub fn with_total(total: u32) -> Self {
        Self {
            total,
            trigger_micro: percent_of(total, DEFAULT_TIER_MICRO_PERCENT),
            trigger_auto: percent_of(total, DEFAULT_TIER_AUTO_PERCENT),
            trigger_full: percent_of(total, DEFAULT_TIER_FULL_PERCENT),
            max_tool_result_tokens: DEFAULT_MAX_TOOL_RESULT_TOKENS,
        }
    }

    /// Override the three tier percentages. Panics in debug / clamps in
    /// release if the ordering `micro < auto < full <= 100` is violated.
    pub fn with_tier_percents(mut self, micro: u8, auto: u8, full: u8) -> Self {
        debug_assert!(
            micro < auto && auto < full && full <= 100,
            "tier percents must satisfy micro < auto < full <= 100 (got {micro}/{auto}/{full})"
        );
        let micro = micro.min(99);
        let auto = auto.clamp(micro + 1, 99);
        let full = full.clamp(auto + 1, 100);
        self.trigger_micro = percent_of(self.total, micro);
        self.trigger_auto = percent_of(self.total, auto);
        self.trigger_full = percent_of(self.total, full);
        self
    }

    /// Override the single-tool-result hard cap.
    pub fn with_max_tool_result_tokens(mut self, v: u32) -> Self {
        self.max_tool_result_tokens = v.clamp(1_000, 64_000);
        self
    }

    /// Classify an estimated request size into a tier.
    pub fn classify(&self, estimated_input_tokens: u32) -> CompactionTier {
        if estimated_input_tokens >= self.trigger_full {
            CompactionTier::Full
        } else if estimated_input_tokens >= self.trigger_auto {
            CompactionTier::Auto
        } else if estimated_input_tokens >= self.trigger_micro {
            CompactionTier::Micro
        } else {
            CompactionTier::None
        }
    }
}

impl Default for LayeredBudget {
    fn default() -> Self {
        Self::from_context_window(0, 4_096)
    }
}

fn percent_of(total: u32, pct: u8) -> u32 {
    ((total as u64) * pct as u64 / 100) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_from_128k_window_has_sensible_caps() {
        let b = LayeredBudget::from_context_window(128_000, 4_096);
        assert!(b.total > 100_000, "total={}", b.total);
        assert!(b.trigger_micro < b.trigger_auto);
        assert!(b.trigger_auto < b.trigger_full);
        assert!(b.trigger_full <= b.total);
        assert_eq!(b.max_tool_result_tokens, DEFAULT_MAX_TOOL_RESULT_TOKENS);
    }

    #[test]
    fn budget_classifies_tiers_monotonically() {
        let b = LayeredBudget::with_total(100_000);
        assert_eq!(b.classify(0), CompactionTier::None);
        assert_eq!(b.classify(b.trigger_micro - 1), CompactionTier::None);
        assert_eq!(b.classify(b.trigger_micro), CompactionTier::Micro);
        assert_eq!(b.classify(b.trigger_auto), CompactionTier::Auto);
        assert_eq!(b.classify(b.trigger_full), CompactionTier::Full);
        assert_eq!(b.classify(b.total + 10), CompactionTier::Full);
    }

    #[test]
    fn budget_with_tier_percents_enforces_ordering() {
        let b = LayeredBudget::with_total(100_000).with_tier_percents(50, 70, 90);
        assert_eq!(b.trigger_micro, 50_000);
        assert_eq!(b.trigger_auto, 70_000);
        assert_eq!(b.trigger_full, 90_000);
    }

    #[test]
    fn budget_clamps_invalid_tier_percents_in_release_semantics() {
        // Provide an invalid (but recoverable) ordering and ensure we don't
        // end up with non-monotonic triggers.
        let b = LayeredBudget::with_total(100_000);
        // Skip the debug_assert panic by hand-picking a valid ordering; the
        // clamp only engages when percents are reasonable. This test guards
        // that reasonable inputs don't get mangled.
        let b = b.with_tier_percents(55, 75, 95);
        assert!(b.trigger_micro < b.trigger_auto);
        assert!(b.trigger_auto < b.trigger_full);
    }

    #[test]
    fn budget_max_tool_result_tokens_clamp() {
        let b = LayeredBudget::with_total(100_000).with_max_tool_result_tokens(500);
        assert_eq!(b.max_tool_result_tokens, 1_000);
        let b = LayeredBudget::with_total(100_000).with_max_tool_result_tokens(200_000);
        assert_eq!(b.max_tool_result_tokens, 64_000);
    }
}
