//! Shared context-compaction constants.
//!
//! These govern the three tiers of context rendering:
//! 1. Recent turns (within `CTX_FULL_TURNS`) keep **full** tool-result content.
//! 2. Middle turns (older than `CTX_FULL_TURNS` but newer than
//!    `CTX_COMPACT_AFTER`) swap to the rule-based **minimal receipt** version.
//! 3. Older-than-`CTX_COMPACT_AFTER` turns go to Level-2 LLM summarisation, at
//!    which point the full content is restored into the prompt so the model
//!    can produce a good rolling summary.
//!
//! Both `agent::loop_` (at request-assembly time) and `commands::chat` (at
//! DB-reload time) depend on these values — they MUST agree or the message
//! sent to the LLM will be inconsistent with what we counted for the budget.

/// Number of most recent turns that always render with full tool-result detail.
pub const CTX_FULL_TURNS: usize = 3;

/// Number of most recent turns to keep before Level-2 LLM summarisation kicks
/// in for the remainder of the session. Turns at index
/// `turn_age >= CTX_COMPACT_AFTER` get replaced by a single summary message.
pub const CTX_COMPACT_AFTER: usize = 8;

/// Head/tail character counts used by the legacy char-trim fallback.
///
/// Kept as a last-resort emergency trim when `content_minimal` is unavailable
/// and the context is still over budget. New tool results rely on the
/// dual-version receipt scheme instead.
pub const CTX_TRIM_HEAD: usize = 1_000;
pub const CTX_TRIM_TAIL: usize = 300;
