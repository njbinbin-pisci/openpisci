//! Platform-neutral pool / multi-agent orchestration module.
//!
//! The desktop (Tauri) and CLI hosts used to each carry their own copy of
//! pool-CRUD, org-spec management, todo board, and message fan-out logic.
//! Pool orchestration lives here in the kernel; hosts only
//! provide:
//!
//! * a [`pisci_core::host::PoolEventSink`] to surface events to their UI
//!   transport (Tauri `emit`, NDJSON stdout, websocket, …)
//! * a [`pisci_core::host::SubagentRuntime`] that knows how to run Koi
//!   turns. Desktop hosts provide an in-process implementation; the
//!   kernel keeps [`subagent::SubprocessSubagentRuntime`] for CLI/eval or
//!   explicit isolation, plus [`subagent::StubSubagentRuntime`] for
//!   unit/integration tests.
//!
//! Module layout:
//!
//! * [`model`] — input-argument structs shared by all services
//! * [`metadata`] — re-exports of the coordination-metadata helpers
//!   (`enrich_pool_message_metadata`, …) so call sites depend on
//!   `pisci_kernel::pool::metadata::*` instead of reaching through
//!   `pisci_core::project_state`
//! * [`store`] — thin async facade around the shared `Database`
//! * [`git`] — `git init`, worktree setup/cleanup, and
//!   `merge_koi_branches` helpers used by the coordinator and
//!   `merge_branches`
//! * [`subagent`] — subprocess/stub implementations of
//!   [`pisci_core::host::SubagentRuntime`] plus the JSON-RPC wire
//!   protocol
//! * [`coordinator`] — kernel-owned Koi-turn orchestration
//!   (execute/resume/replace todos, handle `@mention` fan-out). Used
//!   by the services layer, the desktop pool bridge, and the CLI pool
//!   driver.
//! * [`services`] — the business functions that tools call. Every mutating
//!   service emits zero or more [`PoolEvent`]s through the supplied sink
//!   before returning.
//!
//! Services that drive multi-agent work follow this signature:
//!
//! ```ignore
//! pub async fn foo(
//!     store: &PoolStore,
//!     sink: Arc<dyn PoolEventSink>,
//!     subagent: Option<Arc<dyn SubagentRuntime>>,
//!     cfg: &CoordinatorConfig,
//!     caller: &CallerContext<'_>,
//!     args: FooArgs,
//! ) -> anyhow::Result<Value>
//! ```
//!
//! Services that only mutate the store/emit events keep the original
//! `sink: &dyn PoolEventSink` form. The returned `Value` is the "result
//! payload" a tool formats into user-visible text; tests assert against
//! it directly.

pub mod coordinator;
pub mod git;
pub mod metadata;
pub mod model;
pub mod services;
pub mod store;
pub mod subagent;

pub use model::{
    AssignKoiArgs, CallerContext, CreatePoolArgs, CreateTodoArgs, DeleteTodoArgs, ReplaceTodoArgs,
    SendPoolMessageArgs, UpdateOrgSpecArgs, UpdateTodoStatusArgs,
};
pub use store::PoolStore;
pub use subagent::{
    NotificationSink as SubagentNotificationSink, StubOutcome, StubSubagentRuntime,
    SubprocessSubagentRuntime,
};

/// Session-source tags that must NOT auto-archive a pool. Heartbeat /
/// inbox sessions might decide to archive optimistically; the service
/// layer blocks those to force explicit user intent.
///
/// Kept in sync with `src-tauri/src/commands/chat.rs` until Phase 4
/// deletes the desktop copy.
pub mod session_source {
    pub const PISCI_INBOX_GLOBAL: &str = "pisci_inbox_global";
    pub const PISCI_POOL: &str = "pisci_pool";
    pub const PISCI_INBOX_POOL: &str = "pisci_inbox_pool";
    pub const PISCI_HEARTBEAT_GLOBAL: &str = "pisci_heartbeat_global";
    pub const PISCI_HEARTBEAT_POOL: &str = "pisci_heartbeat_pool";

    /// Returns true if the session source is one where automatic
    /// archiving should be blocked.
    pub fn is_heartbeat_like(source: &str) -> bool {
        matches!(
            source,
            PISCI_INBOX_GLOBAL
                | PISCI_POOL
                | PISCI_INBOX_POOL
                | PISCI_HEARTBEAT_GLOBAL
                | PISCI_HEARTBEAT_POOL
        )
    }
}
