//! Harness configuration assembled per call site.
//!
//! Every call site that previously constructed an [`crate::agent::loop_::AgentLoop`]
//! literal (pisci chat / koi / fish / scheduler / debug) will build a
//! [`HarnessConfig`] in p1. The three `Option<...>` fields (`persistence`,
//! `summary_store`, `frame_provider`) deliberately allow the fish
//! ephemeral loop to opt out of any DB-bound feature — the plan explicitly
//! forbids tying the harness to SQLite.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::agent::tool::ToolRegistry;
use crate::policy::PolicyGate;
use crate::store::Database;

use super::{LayeredBudget, LayeredPrompt};

/// Upstream provider flavour. Used by the future [`RequestBuilder`] (p12)
/// to pick the right message transformation.
///
/// [`RequestBuilder`]: crate::agent::harness
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    OpenAi,
    Other,
}

impl ProviderKind {
    /// Best-effort inference from a model id string.
    pub fn from_model_id(model: &str) -> Self {
        let lower = model.to_ascii_lowercase();
        if lower.contains("claude") || lower.contains("anthropic") {
            ProviderKind::Anthropic
        } else if lower.contains("gpt") || lower.contains("openai") || lower.contains("o1") {
            ProviderKind::OpenAi
        } else {
            ProviderKind::Other
        }
    }
}

/// Snapshot of a session's rolling summary (p7).
#[derive(Debug, Clone)]
pub struct SummarySnapshot {
    pub version: u32,
    pub summary_text: String,
    pub structured: Option<serde_json::Value>,
}

/// Persists rolling summaries. Implemented on top of the `sessions` table
/// in p7. Fish-style ephemeral runs pass `None` and skip async summary.
pub trait SummaryStore: Send + Sync {
    fn load_latest(&self, session_id: &str) -> Option<SummarySnapshot>;
    fn store(&self, session_id: &str, snapshot: SummarySnapshot) -> anyhow::Result<()>;
}

/// Supplies the "state frame" synthetic context (p6). Fish skips this.
pub trait StateFrameProvider: Send + Sync {
    /// Return the latest state frame JSON for the given session, if any.
    fn latest(&self, session_id: &str) -> Option<serde_json::Value>;
}

/// Compaction thresholds + per-tool-result cap + optional summary model
/// plumbed down from `store::settings::Settings`. Each scene factory
/// accepts this as a single parameter so the scope of additions per
/// call site stays small.
#[derive(Debug, Clone)]
pub struct CompactionSettings {
    pub micro_percent: u8,
    pub auto_percent: u8,
    pub full_percent: u8,
    pub max_tool_result_tokens: u32,
    pub summary_model: Option<String>,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            micro_percent: super::budget::DEFAULT_TIER_MICRO_PERCENT,
            auto_percent: super::budget::DEFAULT_TIER_AUTO_PERCENT,
            full_percent: super::budget::DEFAULT_TIER_FULL_PERCENT,
            max_tool_result_tokens: super::budget::DEFAULT_MAX_TOOL_RESULT_TOKENS,
            summary_model: None,
        }
    }
}

impl CompactionSettings {
    /// Pull from the store-level settings struct. Invalid orderings are
    /// clamped by `LayeredBudget::with_tier_percents` at apply time.
    pub fn from_settings(s: &crate::store::settings::Settings) -> Self {
        Self {
            micro_percent: s.compaction_micro_percent,
            auto_percent: s.compaction_auto_percent,
            full_percent: s.compaction_full_percent,
            max_tool_result_tokens: s.max_tool_result_tokens,
            summary_model: s.summary_model.clone(),
        }
    }
}

/// User confirmation preferences — identical shape to
/// [`crate::agent::loop_::ConfirmFlags`] but duplicated here to keep the
/// harness module free of cross-file coupling while p1 migrates the
/// existing structs.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConfirmFlags {
    pub confirm_shell: bool,
    pub confirm_file_write: bool,
}

impl From<ConfirmFlags> for crate::agent::loop_::ConfirmFlags {
    fn from(f: ConfirmFlags) -> Self {
        crate::agent::loop_::ConfirmFlags {
            confirm_shell: f.confirm_shell,
            confirm_file_write: f.confirm_file_write,
        }
    }
}

impl From<crate::agent::loop_::ConfirmFlags> for ConfirmFlags {
    fn from(f: crate::agent::loop_::ConfirmFlags) -> Self {
        ConfirmFlags {
            confirm_shell: f.confirm_shell,
            confirm_file_write: f.confirm_file_write,
        }
    }
}

/// Per-run harness configuration.
///
/// Intentionally non-`Clone` at the top level because [`tauri::AppHandle`]
/// and the summary/frame trait objects are not cheap to clone. The inner
/// pieces are all `Arc` so downstream code can share.
pub struct HarnessConfig {
    /// Scene tag, e.g. `"main"`, `"koi"`, `"fish"`, `"scheduler"`, `"debug"`.
    pub scene: String,

    /// Primary model id.
    pub model: String,
    /// Models to try in order after rate-limit / overloaded / not-found.
    pub fallback_models: Vec<String>,
    /// Output cap (`max_tokens`).
    pub max_tokens: u32,
    /// Model context window (0 = auto).
    pub context_window: u32,

    /// Layered system prompt (L0..Lhint).
    pub layered_prompt: LayeredPrompt,
    /// Tool registry, registered by the call site before handing over.
    pub registry: Arc<ToolRegistry>,
    /// Scene policy gate.
    pub policy: Arc<PolicyGate>,
    /// User confirmation preferences.
    pub confirm_flags: ConfirmFlags,
    /// User-configured vision override (None = auto from model id).
    pub vision_override: Option<bool>,

    /// Derived token budget + tier classifier.
    pub budget: LayeredBudget,

    /// Provider flavour — stable through a run; p12 uses this to pick the
    /// per-provider request shape.
    pub provider_kind: ProviderKind,

    /// Legacy single-threshold auto compaction. Kept during p5a migration.
    /// `0` disables threshold-driven compaction.
    pub auto_compact_input_tokens_threshold: u32,

    // ── Optional wiring ─────────────────────────────────────────────────
    /// Sqlite handle — only wired when this harness persists messages.
    /// **None** for fish and any other ephemeral loop.
    pub persistence: Option<Arc<Mutex<Database>>>,
    /// Rolling-summary store (p7). None => async summary disabled.
    pub summary_store: Option<Arc<dyn SummaryStore>>,
    /// State frame provider (p6). None => no synthetic frame message.
    pub frame_provider: Option<Arc<dyn StateFrameProvider>>,
    /// Tauri app handle for UI events. None => no UI surface (fish,
    /// certain debug runs).
    pub app_handle: Option<tauri::AppHandle>,
    /// Optional override model used to produce rolling summaries (p7).
    /// `None` means "reuse the main model".
    pub summary_model: Option<String>,
}

impl HarnessConfig {
    /// Start a builder from the minimum required fields. All optional
    /// wiring defaults to `None`, which is the fish-style configuration.
    pub fn builder(
        scene: impl Into<String>,
        model: impl Into<String>,
        registry: Arc<ToolRegistry>,
        policy: Arc<PolicyGate>,
    ) -> HarnessConfigBuilder {
        HarnessConfigBuilder::new(scene, model, registry, policy)
    }

    // ── Scene factories ────────────────────────────────────────────────
    //
    // Each factory encodes the *defaults* that used to live as inline
    // struct literals at the call site. Call sites supply only the
    // runtime-varying pieces (system prompt, model, tokens, vision, etc.)
    // and the factory wires in the rest.

    /// Pisci main-chat harness. Persists to DB, reacts to UI, full
    /// confirmation flags, async summary + state frame enabled.
    #[allow(clippy::too_many_arguments)]
    pub fn for_main_chat(
        model: String,
        fallback_models: Vec<String>,
        registry: Arc<ToolRegistry>,
        policy: Arc<PolicyGate>,
        system_prompt: String,
        max_tokens: u32,
        context_window: u32,
        confirm_flags: ConfirmFlags,
        vision_override: Option<bool>,
        auto_compact_input_tokens_threshold: u32,
        compaction: CompactionSettings,
        db: Arc<Mutex<Database>>,
        app_handle: tauri::AppHandle,
    ) -> Self {
        HarnessConfigBuilder::new("main", model, registry, policy)
            .with_fallback_models(fallback_models)
            .with_layered_prompt(LayeredPrompt::from_monolithic(system_prompt))
            .with_max_tokens(max_tokens)
            .with_context_window(context_window)
            .with_confirm_flags(confirm_flags)
            .with_vision_override(vision_override)
            .with_auto_compact_threshold(auto_compact_input_tokens_threshold)
            .with_compaction_settings(&compaction)
            .with_persistence(db)
            .with_app_handle(app_handle)
            .build()
    }

    /// Headless main-chat harness (no UI, no user confirmations).
    /// Used by `run_agent_headless` and similar server/trigger paths
    /// that still want the main-chat scene policy but without blocking
    /// on interactive confirmations.
    #[allow(clippy::too_many_arguments)]
    pub fn for_main_headless(
        model: String,
        fallback_models: Vec<String>,
        registry: Arc<ToolRegistry>,
        policy: Arc<PolicyGate>,
        system_prompt: String,
        max_tokens: u32,
        context_window: u32,
        vision_override: Option<bool>,
        auto_compact_input_tokens_threshold: u32,
        compaction: CompactionSettings,
        db: Arc<Mutex<Database>>,
    ) -> Self {
        HarnessConfigBuilder::new("main_headless", model, registry, policy)
            .with_fallback_models(fallback_models)
            .with_layered_prompt(LayeredPrompt::from_monolithic(system_prompt))
            .with_max_tokens(max_tokens)
            .with_context_window(context_window)
            .with_vision_override(vision_override)
            .with_auto_compact_threshold(auto_compact_input_tokens_threshold)
            .with_compaction_settings(&compaction)
            .with_persistence(db)
            .build()
    }

    /// Koi sub-agent harness. Shares DB with pisci (so koi-produced
    /// messages persist in its own session), but usually no confirm
    /// prompts (koi runs semi-autonomously).
    #[allow(clippy::too_many_arguments)]
    pub fn for_koi(
        model: String,
        fallback_models: Vec<String>,
        registry: Arc<ToolRegistry>,
        policy: Arc<PolicyGate>,
        system_prompt: String,
        max_tokens: u32,
        context_window: u32,
        vision_override: Option<bool>,
        auto_compact_input_tokens_threshold: u32,
        compaction: CompactionSettings,
        db: Option<Arc<Mutex<Database>>>,
        app_handle: Option<tauri::AppHandle>,
    ) -> Self {
        let mut b = HarnessConfigBuilder::new("koi", model, registry, policy)
            .with_fallback_models(fallback_models)
            .with_layered_prompt(LayeredPrompt::from_monolithic(system_prompt))
            .with_max_tokens(max_tokens)
            .with_context_window(context_window)
            .with_vision_override(vision_override)
            .with_auto_compact_threshold(auto_compact_input_tokens_threshold)
            .with_compaction_settings(&compaction);
        if let Some(db) = db {
            b = b.with_persistence(db);
        }
        if let Some(h) = app_handle {
            b = b.with_app_handle(h);
        }
        b.build()
    }

    /// Fish ephemeral harness. Zero persistence, zero confirmations,
    /// UI only for progress forwarding.
    #[allow(clippy::too_many_arguments)]
    pub fn for_fish(
        model: String,
        registry: Arc<ToolRegistry>,
        policy: Arc<PolicyGate>,
        system_prompt: String,
        max_tokens: u32,
        vision_capable: bool,
        auto_compact_input_tokens_threshold: u32,
        compaction: CompactionSettings,
        app_handle: tauri::AppHandle,
    ) -> Self {
        HarnessConfigBuilder::new("fish", model, registry, policy)
            .with_layered_prompt(LayeredPrompt::from_monolithic(system_prompt))
            .with_max_tokens(max_tokens)
            .with_vision_override(Some(vision_capable))
            .with_auto_compact_threshold(auto_compact_input_tokens_threshold)
            .with_compaction_settings(&compaction)
            .with_app_handle(app_handle)
            .build()
    }

    /// Scheduled-task harness. Persists to DB under a dedicated
    /// scheduler session; no UI surface.
    #[allow(clippy::too_many_arguments)]
    pub fn for_scheduler(
        model: String,
        fallback_models: Vec<String>,
        registry: Arc<ToolRegistry>,
        policy: Arc<PolicyGate>,
        system_prompt: String,
        max_tokens: u32,
        context_window: u32,
        vision_override: Option<bool>,
        auto_compact_input_tokens_threshold: u32,
        compaction: CompactionSettings,
        db: Arc<Mutex<Database>>,
    ) -> Self {
        HarnessConfigBuilder::new("scheduler", model, registry, policy)
            .with_fallback_models(fallback_models)
            .with_layered_prompt(LayeredPrompt::from_monolithic(system_prompt))
            .with_max_tokens(max_tokens)
            .with_context_window(context_window)
            .with_vision_override(vision_override)
            .with_auto_compact_threshold(auto_compact_input_tokens_threshold)
            .with_compaction_settings(&compaction)
            .with_persistence(db)
            .build()
    }

    /// Debug / trial harness. May or may not persist depending on the
    /// scenario; caller provides `db` explicitly.
    #[allow(clippy::too_many_arguments)]
    pub fn for_debug(
        model: String,
        registry: Arc<ToolRegistry>,
        policy: Arc<PolicyGate>,
        system_prompt: String,
        max_tokens: u32,
        context_window: u32,
        vision_override: Option<bool>,
        compaction: CompactionSettings,
        db: Option<Arc<Mutex<Database>>>,
        app_handle: Option<tauri::AppHandle>,
    ) -> Self {
        let mut b = HarnessConfigBuilder::new("debug", model, registry, policy)
            .with_layered_prompt(LayeredPrompt::from_monolithic(system_prompt))
            .with_max_tokens(max_tokens)
            .with_context_window(context_window)
            .with_vision_override(vision_override)
            .with_compaction_settings(&compaction);
        if let Some(db) = db {
            b = b.with_persistence(db);
        }
        if let Some(h) = app_handle {
            b = b.with_app_handle(h);
        }
        b.build()
    }

    /// Bridge: turn a harness config into the legacy
    /// [`crate::agent::loop_::AgentLoop`] value that existing call sites
    /// consume. p1 uses this during the incremental migration; later
    /// phases (p12) move the per-provider request shaping into a
    /// dedicated `RequestBuilder` so the bridge becomes narrower.
    ///
    /// `notification_rx` and `confirmation_responses` are per-run
    /// plumbing and stay on the call site — they aren't baked into the
    /// config.
    pub fn into_agent_loop(
        self,
        client: Box<dyn crate::llm::LlmClient>,
        notification_rx: Option<tokio::sync::mpsc::Receiver<String>>,
        confirmation_responses: Option<crate::agent::loop_::ConfirmationResponseMap>,
    ) -> crate::agent::loop_::AgentLoop {
        crate::agent::loop_::AgentLoop {
            client,
            registry: self.registry,
            policy: self.policy,
            system_prompt: self.layered_prompt.render(),
            model: self.model,
            max_tokens: self.max_tokens,
            context_window: self.context_window,
            fallback_models: self.fallback_models,
            db: self.persistence,
            app_handle: self.app_handle,
            confirmation_responses,
            confirm_flags: self.confirm_flags.into(),
            vision_override: self.vision_override,
            notification_rx: notification_rx.map(tokio::sync::Mutex::new),
            auto_compact_input_tokens_threshold: self.auto_compact_input_tokens_threshold,
        }
    }
}

/// Builder. All `with_*` methods consume and return `self` for chaining.
pub struct HarnessConfigBuilder {
    inner: HarnessConfig,
}

impl HarnessConfigBuilder {
    fn new(
        scene: impl Into<String>,
        model: impl Into<String>,
        registry: Arc<ToolRegistry>,
        policy: Arc<PolicyGate>,
    ) -> Self {
        let model = model.into();
        let provider_kind = ProviderKind::from_model_id(&model);
        let inner = HarnessConfig {
            scene: scene.into(),
            model,
            fallback_models: Vec::new(),
            max_tokens: 4_096,
            context_window: 0,
            layered_prompt: LayeredPrompt::default(),
            registry,
            policy,
            confirm_flags: ConfirmFlags::default(),
            vision_override: None,
            budget: LayeredBudget::default(),
            provider_kind,
            auto_compact_input_tokens_threshold: 0,
            persistence: None,
            summary_store: None,
            frame_provider: None,
            app_handle: None,
            summary_model: None,
        };
        Self { inner }
    }

    pub fn with_fallback_models(mut self, models: Vec<String>) -> Self {
        self.inner.fallback_models = models;
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.inner.max_tokens = max_tokens;
        self.refresh_budget();
        self
    }

    pub fn with_context_window(mut self, context_window: u32) -> Self {
        self.inner.context_window = context_window;
        self.refresh_budget();
        self
    }

    pub fn with_layered_prompt(mut self, lp: LayeredPrompt) -> Self {
        self.inner.layered_prompt = lp;
        self
    }

    pub fn with_confirm_flags(mut self, cf: ConfirmFlags) -> Self {
        self.inner.confirm_flags = cf;
        self
    }

    pub fn with_vision_override(mut self, ov: Option<bool>) -> Self {
        self.inner.vision_override = ov;
        self
    }

    pub fn with_auto_compact_threshold(mut self, tokens: u32) -> Self {
        self.inner.auto_compact_input_tokens_threshold = tokens;
        self
    }

    pub fn with_tier_percents(mut self, micro: u8, auto: u8, full: u8) -> Self {
        self.inner.budget = self.inner.budget.with_tier_percents(micro, auto, full);
        self
    }

    pub fn with_max_tool_result_tokens(mut self, tokens: u32) -> Self {
        self.inner.budget = self.inner.budget.with_max_tool_result_tokens(tokens);
        self
    }

    /// Apply a full `CompactionSettings` bundle — tier percents, per-
    /// tool-result cap, and the optional summary-model override — in a
    /// single call. Used by scene factories to keep their signatures
    /// narrow.
    pub fn with_compaction_settings(mut self, c: &CompactionSettings) -> Self {
        self.inner.budget = self
            .inner
            .budget
            .with_tier_percents(c.micro_percent, c.auto_percent, c.full_percent)
            .with_max_tool_result_tokens(c.max_tool_result_tokens);
        self.inner.summary_model = c.summary_model.clone().filter(|s| !s.trim().is_empty());
        self
    }

    pub fn with_persistence(mut self, db: Arc<Mutex<Database>>) -> Self {
        self.inner.persistence = Some(db);
        self
    }

    pub fn with_summary_store(mut self, store: Arc<dyn SummaryStore>) -> Self {
        self.inner.summary_store = Some(store);
        self
    }

    pub fn with_frame_provider(mut self, provider: Arc<dyn StateFrameProvider>) -> Self {
        self.inner.frame_provider = Some(provider);
        self
    }

    pub fn with_app_handle(mut self, h: tauri::AppHandle) -> Self {
        self.inner.app_handle = Some(h);
        self
    }

    /// Set the optional summary-model override (p5a / p7). Empty strings
    /// and `None` both mean "reuse the main model".
    pub fn with_summary_model(mut self, m: Option<String>) -> Self {
        self.inner.summary_model = m.filter(|s| !s.trim().is_empty());
        self
    }

    pub fn build(self) -> HarnessConfig {
        self.inner
    }

    fn refresh_budget(&mut self) {
        // Re-derive caps from the current (context_window, max_tokens)
        // while preserving any previously-set tier percents / tool cap.
        let prev = self.inner.budget;
        let new =
            LayeredBudget::from_context_window(self.inner.context_window, self.inner.max_tokens);
        // Preserve overrides if they diverged from defaults.
        let micro_pct = pct_of(prev.trigger_micro, prev.total);
        let auto_pct = pct_of(prev.trigger_auto, prev.total);
        let full_pct = pct_of(prev.trigger_full, prev.total);
        let mut new = new;
        if prev.total > 0 && micro_pct > 0 && auto_pct > micro_pct && full_pct > auto_pct {
            new = new.with_tier_percents(micro_pct, auto_pct, full_pct);
        }
        if prev.max_tool_result_tokens != super::budget::DEFAULT_MAX_TOOL_RESULT_TOKENS {
            new = new.with_max_tool_result_tokens(prev.max_tool_result_tokens);
        }
        self.inner.budget = new;
    }
}

fn pct_of(value: u32, total: u32) -> u8 {
    if total == 0 {
        0
    } else {
        ((value as u64 * 100 / total as u64).min(100)) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tool::ToolRegistry;
    use crate::policy::gate::PolicyGate;
    use std::path::PathBuf;

    fn scaffolding() -> (Arc<ToolRegistry>, Arc<PolicyGate>) {
        let reg = Arc::new(ToolRegistry::new());
        let policy = Arc::new(PolicyGate::new(PathBuf::from(".")));
        (reg, policy)
    }

    #[test]
    fn builder_defaults_to_fish_style_no_persistence() {
        let (reg, policy) = scaffolding();
        let cfg = HarnessConfig::builder("fish", "claude-haiku-4.5", reg, policy).build();
        assert_eq!(cfg.scene, "fish");
        assert_eq!(cfg.provider_kind, ProviderKind::Anthropic);
        assert!(cfg.persistence.is_none());
        assert!(cfg.summary_store.is_none());
        assert!(cfg.frame_provider.is_none());
        assert!(cfg.app_handle.is_none());
        assert_eq!(cfg.fallback_models, Vec::<String>::new());
    }

    #[test]
    fn builder_respects_context_window_and_tier_overrides() {
        let (reg, policy) = scaffolding();
        let cfg = HarnessConfig::builder("main", "claude-sonnet-4.5", reg, policy)
            .with_context_window(200_000)
            .with_max_tokens(8_192)
            .with_tier_percents(55, 75, 92)
            .with_max_tool_result_tokens(6_000)
            .build();
        assert!(cfg.budget.total > 150_000, "total={}", cfg.budget.total);
        assert_eq!(cfg.budget.max_tool_result_tokens, 6_000);
        // Tier ordering preserved.
        assert!(cfg.budget.trigger_micro < cfg.budget.trigger_auto);
        assert!(cfg.budget.trigger_auto < cfg.budget.trigger_full);
    }

    #[test]
    fn provider_kind_inference_covers_common_models() {
        assert_eq!(
            ProviderKind::from_model_id("claude-sonnet-4.5"),
            ProviderKind::Anthropic
        );
        assert_eq!(
            ProviderKind::from_model_id("gpt-5-mini"),
            ProviderKind::OpenAi
        );
        assert_eq!(
            ProviderKind::from_model_id("o1-preview"),
            ProviderKind::OpenAi
        );
        assert_eq!(
            ProviderKind::from_model_id("qwen-max"),
            ProviderKind::Other
        );
    }

    #[test]
    fn confirm_flags_round_trip_with_agent_loop_flags() {
        let cf = ConfirmFlags {
            confirm_shell: true,
            confirm_file_write: false,
        };
        let loop_cf: crate::agent::loop_::ConfirmFlags = cf.into();
        assert!(loop_cf.confirm_shell);
        assert!(!loop_cf.confirm_file_write);
        let round: ConfirmFlags = loop_cf.into();
        assert!(round.confirm_shell);
        assert!(!round.confirm_file_write);
    }
}
