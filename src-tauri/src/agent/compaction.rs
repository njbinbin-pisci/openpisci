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

/// Number of most recent *user-text turns* whose trailing messages render
/// with full tool-result detail. A "user-text turn" is a `user` message
/// whose content is plain `Text` (not a tool-result carrier).
///
/// This is one of the **two** independent boundaries the request-assembly
/// path honours (see [`build_request_messages`]). The other is
/// [`CTX_KEEP_RECENT_TOOL_CARRIERS`]. The final cutoff is the `min` index
/// of the two, so whichever boundary keeps *more* messages full wins.
pub const CTX_PRESERVE_RECENT_TURNS: usize = 3;

/// Backwards-compatible alias retained for call sites that predate the p5
/// two-boundary scheme. New code should use [`CTX_PRESERVE_RECENT_TURNS`].
pub const CTX_FULL_TURNS: usize = CTX_PRESERVE_RECENT_TURNS;

/// Number of most recent *tool-result-carrying messages* to preserve with
/// full detail regardless of user-turn count.
///
/// This protects a long, multi-tool-call single-turn workflow (think
/// "screenshot → uia inspect → shell run" inside one reasoning step)
/// from having its own early tool results demoted while that same turn
/// is still in flight. Taken as a *min* with
/// [`CTX_PRESERVE_RECENT_TURNS`], so we always keep whichever boundary
/// is further back (i.e. preserves more detail).
pub const CTX_KEEP_RECENT_TOOL_CARRIERS: usize = 8;

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
