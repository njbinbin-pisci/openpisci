//! Cross-cutting notification + decision-request abstraction.
//!
//! This module is *kernel-side* — it only owns serialisable types
//! (`NotificationLevel`, `NotificationTarget`, `NotificationRequest`,
//! `NotificationOutcome`, `PendingDecision*`) plus a parser for the
//! "target token" strings that flow through `app_control(notify_user, …)`
//! and `scheduled_tasks.notify_targets_json`.
//!
//! The actual fan-out (UI toast, IM gateway send, decision-response
//! waiting, etc.) lives in the host crate (`pisci-desktop::notify`),
//! which can pull in `tauri::AppHandle` and the `GatewayManager`. Kernel
//! callers should only ever construct `NotificationRequest` / `Pending*`
//! values and hand them off.

use serde::{Deserialize, Serialize};

/// Severity of a notification. Determines default UI duration and
/// whether the request should escalate beyond the main UI when no
/// explicit targets are provided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationLevel {
    Info,
    Warning,
    Error,
    Critical,
}

impl NotificationLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            NotificationLevel::Info => "info",
            NotificationLevel::Warning => "warning",
            NotificationLevel::Error => "error",
            NotificationLevel::Critical => "critical",
        }
    }

    /// Default auto-dismiss duration (ms). `0` means "persistent until
    /// the user dismisses it" — used for `Critical` so escalations
    /// don't disappear silently.
    pub fn default_duration_ms(self) -> i64 {
        match self {
            NotificationLevel::Critical => 0,
            NotificationLevel::Error => 12_000,
            NotificationLevel::Warning => 8_000,
            NotificationLevel::Info => 5_000,
        }
    }

    pub fn parse_lenient(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "info" | "" => Some(NotificationLevel::Info),
            "warn" | "warning" => Some(NotificationLevel::Warning),
            "error" => Some(NotificationLevel::Error),
            "critical" | "crit" => Some(NotificationLevel::Critical),
            _ => None,
        }
    }
}

impl Default for NotificationLevel {
    fn default() -> Self {
        NotificationLevel::Info
    }
}

/// Where a notification should be delivered.
///
/// The target list is open-ended on purpose — Pisci, scheduled tasks,
/// pool heartbeats and the host can all enrich the list with their own
/// destinations. The host `dispatch_notification` walks the list in
/// order and reports a per-target outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotificationTarget {
    /// Main desktop UI toast.
    Ui,
    /// A specific IM conversation, identified by its stable
    /// `binding_key` (see `im_session_bindings.binding_key`).
    ImBinding { binding_key: String },
    /// An internal Pisci `session_id`. The host looks up the
    /// matching `im_session_bindings` row and routes to whichever
    /// channel/conversation last spoke to that session.
    ImSession { session_id: String },
}

impl NotificationTarget {
    pub fn ui() -> Self {
        NotificationTarget::Ui
    }

    pub fn im_binding(binding_key: impl Into<String>) -> Self {
        NotificationTarget::ImBinding {
            binding_key: binding_key.into(),
        }
    }

    pub fn im_session(session_id: impl Into<String>) -> Self {
        NotificationTarget::ImSession {
            session_id: session_id.into(),
        }
    }

    /// Parse a short token form used by tools and config:
    /// `"ui"`, `"im_binding:<key>"`, `"im_session:<sid>"`.
    pub fn parse_token(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("empty notification target".into());
        }
        if trimmed.eq_ignore_ascii_case("ui") {
            return Ok(NotificationTarget::Ui);
        }
        if let Some(rest) = trimmed
            .strip_prefix("im_binding:")
            .or_else(|| trimmed.strip_prefix("im-binding:"))
        {
            let key = rest.trim();
            if key.is_empty() {
                return Err("im_binding target requires a binding_key".into());
            }
            return Ok(NotificationTarget::ImBinding {
                binding_key: key.to_string(),
            });
        }
        if let Some(rest) = trimmed
            .strip_prefix("im_session:")
            .or_else(|| trimmed.strip_prefix("im-session:"))
        {
            let sid = rest.trim();
            if sid.is_empty() {
                return Err("im_session target requires a session_id".into());
            }
            return Ok(NotificationTarget::ImSession {
                session_id: sid.to_string(),
            });
        }
        Err(format!(
            "unknown notification target '{}': expected 'ui', 'im_binding:<key>', or 'im_session:<id>'",
            trimmed
        ))
    }

    pub fn to_token(&self) -> String {
        match self {
            NotificationTarget::Ui => "ui".to_string(),
            NotificationTarget::ImBinding { binding_key } => {
                format!("im_binding:{}", binding_key)
            }
            NotificationTarget::ImSession { session_id } => {
                format!("im_session:{}", session_id)
            }
        }
    }

    pub fn parse_tokens<I, S>(tokens: I) -> Result<Vec<NotificationTarget>, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        tokens
            .into_iter()
            .map(|t| NotificationTarget::parse_token(t.as_ref()))
            .collect()
    }
}

/// A single notification request describing *what* to say plus
/// *where* to deliver it. The host expands this into channel-specific
/// payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationRequest {
    pub title: String,
    pub message: String,
    #[serde(default)]
    pub level: NotificationLevel,
    /// "pisci", "heartbeat_auto", "scheduled_task", etc. Used both for
    /// telemetry and for de-duplication on the UI side.
    #[serde(default)]
    pub source: String,
    /// Optional pool reference so the UI can deep-link the toast and
    /// the IM message can mention the project name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_id: Option<String>,
    /// Optional `pending_decisions.id` to wire the notification to a
    /// specific decision request. Phase 4 will use this; Phase 0/1/2
    /// callers leave it empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    /// Auto-dismiss duration (ms). `None` defers to the level default;
    /// `Some(0)` means "persistent until the user dismisses".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Where the notification should land. Empty list defaults to
    /// `[Ui]` — the host applies that fallback so kernel callers can
    /// send `request.targets.clear()` to mean "use defaults".
    #[serde(default)]
    pub targets: Vec<NotificationTarget>,
}

impl NotificationRequest {
    pub fn new(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            level: NotificationLevel::Info,
            source: String::new(),
            pool_id: None,
            decision_id: None,
            duration_ms: None,
            targets: Vec::new(),
        }
    }

    pub fn with_level(mut self, level: NotificationLevel) -> Self {
        self.level = level;
        self
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    pub fn with_pool(mut self, pool_id: impl Into<String>) -> Self {
        self.pool_id = Some(pool_id.into());
        self
    }

    pub fn with_decision(mut self, decision_id: impl Into<String>) -> Self {
        self.decision_id = Some(decision_id.into());
        self
    }

    pub fn with_duration_ms(mut self, duration_ms: i64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    pub fn add_target(mut self, target: NotificationTarget) -> Self {
        self.targets.push(target);
        self
    }

    pub fn with_targets(mut self, targets: Vec<NotificationTarget>) -> Self {
        self.targets = targets;
        self
    }

    pub fn effective_duration_ms(&self) -> i64 {
        self.duration_ms
            .unwrap_or_else(|| self.level.default_duration_ms())
    }

    /// Drop targets that point at the same place. Preserves order so
    /// the UI fallback (when present) stays first.
    pub fn dedup_targets(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.targets.retain(|t| seen.insert(t.to_token()));
    }
}

/// Per-target delivery result reported back by the host. Kernel-side
/// schedulers use this to log success / failure without having to know
/// about Tauri or HTTP details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationOutcome {
    pub target: NotificationTarget,
    pub delivered: bool,
    /// Free-form host description: `"toast emitted"`, `"sent via wechat"`,
    /// `"binding not found"` etc.
    pub detail: String,
}

impl NotificationOutcome {
    pub fn ok(target: NotificationTarget, detail: impl Into<String>) -> Self {
        Self {
            target,
            delivered: true,
            detail: detail.into(),
        }
    }

    pub fn failed(target: NotificationTarget, detail: impl Into<String>) -> Self {
        Self {
            target,
            delivered: false,
            detail: detail.into(),
        }
    }
}

/// Status of a `pending_decisions` row. Phase 0 just lays the rails;
/// Phase 4 will wire the resolution flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PendingDecisionStatus {
    Pending,
    Responded,
    Cancelled,
    Expired,
}

impl PendingDecisionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PendingDecisionStatus::Pending => "pending",
            PendingDecisionStatus::Responded => "responded",
            PendingDecisionStatus::Cancelled => "cancelled",
            PendingDecisionStatus::Expired => "expired",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "responded" => PendingDecisionStatus::Responded,
            "cancelled" | "canceled" => PendingDecisionStatus::Cancelled,
            "expired" => PendingDecisionStatus::Expired,
            _ => PendingDecisionStatus::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_target_tokens_round_trip() {
        let tokens = [
            "ui",
            "im_binding:wechat::dm:user-1",
            "im_session:im_wechat_x",
        ];
        let parsed = NotificationTarget::parse_tokens(tokens).expect("parse all");
        let round_trip: Vec<String> = parsed.iter().map(NotificationTarget::to_token).collect();
        assert_eq!(round_trip, tokens);
    }

    #[test]
    fn parse_target_token_rejects_blank_payload() {
        assert!(NotificationTarget::parse_token("im_binding:").is_err());
        assert!(NotificationTarget::parse_token("im_session:").is_err());
        assert!(NotificationTarget::parse_token("").is_err());
        assert!(NotificationTarget::parse_token("smoke_signal").is_err());
    }

    #[test]
    fn parse_target_token_accepts_dash_alias() {
        let parsed =
            NotificationTarget::parse_token("im-binding:wechat::dm:user-1").expect("parse");
        assert_eq!(parsed, NotificationTarget::im_binding("wechat::dm:user-1"));
    }

    #[test]
    fn dedup_targets_preserves_first_occurrence() {
        let mut req = NotificationRequest::new("title", "body").with_targets(vec![
            NotificationTarget::Ui,
            NotificationTarget::im_session("im_wechat_x"),
            NotificationTarget::Ui,
            NotificationTarget::im_session("im_wechat_x"),
        ]);
        req.dedup_targets();
        assert_eq!(
            req.targets,
            vec![
                NotificationTarget::Ui,
                NotificationTarget::im_session("im_wechat_x"),
            ]
        );
    }

    #[test]
    fn level_default_durations_match_legacy_app_control_behaviour() {
        assert_eq!(NotificationLevel::Info.default_duration_ms(), 5_000);
        assert_eq!(NotificationLevel::Warning.default_duration_ms(), 8_000);
        assert_eq!(NotificationLevel::Error.default_duration_ms(), 12_000);
        assert_eq!(NotificationLevel::Critical.default_duration_ms(), 0);
    }

    #[test]
    fn pending_decision_status_parse_is_lenient() {
        assert_eq!(
            PendingDecisionStatus::parse("RESPONDED"),
            PendingDecisionStatus::Responded
        );
        assert_eq!(
            PendingDecisionStatus::parse("canceled"),
            PendingDecisionStatus::Cancelled
        );
        assert_eq!(
            PendingDecisionStatus::parse("foo"),
            PendingDecisionStatus::Pending
        );
    }
}
