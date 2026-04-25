//! Host traits — the contract between `pisci-kernel` (OS/UI-neutral runtime)
//! and the concrete hosts that embed it (Tauri desktop, `openpisci-headless` CLI,
//! future server process, …).
//!
//! The kernel always consumes these traits behind `Arc<dyn Trait>` pointers
//! obtained from a [`HostRuntime`]. The desktop host implements them by
//! forwarding to Tauri events / windows; the CLI host implements them by
//! writing NDJSON to stdout and returning deterministic defaults for
//! interactive prompts.
//!
//! `pisci-core` remains dependency-light on purpose: only `chrono`, `serde`,
//! `serde_json` and `async-trait`. No tokio, no reqwest, no rusqlite. If a
//! future trait needs async, express it via `#[async_trait]`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

// -- EventSink -------------------------------------------------------------

/// Publishes agent events out of the kernel so that a host can surface them
/// to whatever UI (Tauri window, terminal, web socket) it maintains.
///
/// Every `emit_session` call corresponds to a one-off payload tied to a
/// single agent session. `emit_broadcast` is for cross-session events
/// (completion notifications, state changes in the global view).
pub trait EventSink: Send + Sync {
    fn emit_session(&self, session_id: &str, event: &str, payload: Value);
    fn emit_broadcast(&self, event: &str, payload: Value);
}

// -- Notifier --------------------------------------------------------------

/// Request shape for a yes/no confirmation prompt that the agent wants to
/// surface to the user before performing a risky action (delete file, run
/// destructive shell command, …).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfirmRequest {
    pub request_id: String,
    pub session_id: Option<String>,
    pub title: String,
    pub body: String,
    /// Optional tool name the confirmation is gating.
    pub tool: Option<String>,
    /// Preset response if no human is around (CLI host uses this).
    pub default: Option<bool>,
}

/// Rich interactive prompt (e.g. a form rendered in the desktop chat panel
/// that expects a JSON response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractiveRequest {
    pub request_id: String,
    pub session_id: Option<String>,
    pub kind: String,
    pub payload: Value,
    pub default: Option<Value>,
}

/// Surface user-visible toasts and wait for confirmation/interactive
/// responses. Async methods are only meaningful on desktop; the CLI host
/// returns instantly.
#[async_trait]
pub trait Notifier: Send + Sync {
    fn toast(&self, level: &str, message: &str, pool_id: Option<&str>, duration_ms: Option<u64>);

    async fn request_confirmation(&self, req: ConfirmRequest) -> bool;
    async fn request_interactive(&self, req: InteractiveRequest) -> Value;
}

// -- HostTools -------------------------------------------------------------

/// Opaque handle to the kernel's `ToolRegistry`, supplied to `HostTools::
/// register` so hosts can drop in platform-specific tools without the core
/// crate taking a dependency on the concrete registry type.
///
/// Because `pisci-core` cannot mention the kernel's concrete `ToolRegistry`
/// type, we keep the payload type-erased behind `Box<dyn Any>`. Hosts that
/// live inside the same process (always the case for us: desktop and CLI
/// link against the exact same kernel build) can recover the concrete type
/// through [`downcast_mut`](Self::downcast_mut) / [`downcast_ref`](Self::downcast_ref).
///
/// The ergonomic kernel-side helpers (`as_registry_mut`, `register_tool`,
/// …) live in `pisci-kernel::agent::tool::ToolRegistryHandleExt` so host
/// crates can drop the downcast entirely:
///
/// ```ignore
/// use pisci_kernel::agent::tool::ToolRegistryHandleExt;
///
/// impl HostTools for DesktopHostTools {
///     fn register(&self, handle: &mut ToolRegistryHandle) {
///         let reg = handle.as_registry_mut().expect("kernel registry");
///         reg.register(Box::new(MyDesktopTool::new()));
///     }
/// }
/// ```
pub struct ToolRegistryHandle {
    /// Type-erased pointer managed by the kernel; boxing keeps the ABI
    /// stable while we move real tool registration code in.
    pub inner: Box<dyn std::any::Any + Send + Sync>,
    /// Snapshot of the concrete `T` used to build this handle. We capture
    /// it at construction time because `dyn Any` erases the type name —
    /// handy for diagnostics and for cross-kernel version-mismatch
    /// error messages.
    type_name: &'static str,
}

impl ToolRegistryHandle {
    /// Construct a handle from any type-erased value. Intended for internal
    /// use by the kernel when it hands the registry to a host.
    pub fn new<T: std::any::Any + Send + Sync>(value: T) -> Self {
        Self {
            inner: Box::new(value),
            type_name: std::any::type_name::<T>(),
        }
    }

    /// Mutable downcast to the kernel's concrete registry type. Returns
    /// `None` if `T` does not match the stored payload — that should never
    /// happen in-process but failing soft makes wiring bugs easier to debug.
    pub fn downcast_mut<T: std::any::Any>(&mut self) -> Option<&mut T> {
        self.inner.downcast_mut::<T>()
    }

    /// Shared downcast for read-only access (tool-list inspection,
    /// diagnostics, capability reporting).
    pub fn downcast_ref<T: std::any::Any>(&self) -> Option<&T> {
        self.inner.downcast_ref::<T>()
    }

    /// Consume the handle and recover the concrete payload. On type
    /// mismatch the handle is returned untouched in `Err` so the caller
    /// can try a different type or wrap it again.
    pub fn into_inner<T: std::any::Any + Send + Sync>(self) -> Result<T, Self> {
        let type_name = self.type_name;
        match self.inner.downcast::<T>() {
            Ok(boxed) => Ok(*boxed),
            Err(inner) => Err(Self { inner, type_name }),
        }
    }

    /// Typed scoped mutation. Folds the `downcast_mut + option-map` dance
    /// into one call so host adapters read linearly. Returns `None` only
    /// when the payload type does not match.
    pub fn with_mut<T: std::any::Any, R>(&mut self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        self.inner.downcast_mut::<T>().map(f)
    }

    /// Name of the concrete type this handle was built with. Captured at
    /// construction time so we still have it even after the payload has
    /// been downcast behind `dyn Any`.
    pub fn type_name(&self) -> &'static str {
        self.type_name
    }
}

/// Injection point for platform-specific tools (browser / UIA / screen /
/// COM / PowerShell / WMI / IM gateways). The desktop host attaches its
/// tool implementations inside `register`; the CLI host does nothing.
pub trait HostTools: Send + Sync {
    fn register(&self, registry: &mut ToolRegistryHandle);
}

// -- SecretsStore ----------------------------------------------------------

/// Read/write access to host-managed secrets (API keys, OAuth tokens). The
/// desktop host encrypts them at rest via `chacha20poly1305`; the CLI host
/// backs onto environment variables.
pub trait SecretsStore: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, value: &str) -> anyhow::Result<()>;
}

// -- HostRuntime -----------------------------------------------------------

/// Aggregate host interface. The kernel only ever borrows from this;
/// never stores host-specific types directly.
pub trait HostRuntime: Send + Sync {
    fn event_sink(&self) -> Arc<dyn EventSink>;
    fn notifier(&self) -> Arc<dyn Notifier>;
    fn host_tools(&self) -> Arc<dyn HostTools>;
    fn secrets(&self) -> Arc<dyn SecretsStore>;
    fn app_data_dir(&self) -> PathBuf;

    /// Typed pool-event outlet. Hosts that care about pool state
    /// transitions (desktop UI, CLI NDJSON, benchmark harnesses) override
    /// this; hosts that don't keep the null default, which silently drops
    /// every event. The return type is `Arc<dyn PoolEventSink>` so
    /// kernel services can clone the handle into background tasks
    /// without borrowing the host.
    fn pool_event_sink(&self) -> Arc<dyn PoolEventSink> {
        Arc::new(NullPoolEventSink)
    }

    /// Optional [`SubagentRuntime`] the host provides for fanning out
    /// Koi turns as subprocesses. Hosts that don't run multi-agent
    /// coordination (pure Pisci CLI chat, benchmark harness) return
    /// `None`; hosts that do (desktop app, `openpisci-headless run
    /// --mode pool`) plug in a [`SubprocessSubagentRuntime`].
    fn subagent_runtime(&self) -> Option<Arc<dyn SubagentRuntime>> {
        None
    }
}

// -- Shared headless schema ------------------------------------------------
//
// The CLI request / response and context-toggles schema lives in `pisci-core`
// so that hosts (pisci-desktop, pisci-cli) and external consumers (python
// benchmark scripts) share a single canonical shape.

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HeadlessCliMode {
    #[default]
    Pisci,
    Pool,
}

impl HeadlessCliMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pisci => "pisci",
            Self::Pool => "pool",
        }
    }
}

/// Fine-grained knobs for context assembly. The kernel reads these when a
/// headless CLI run requests ablation-style behaviour from bench_swe_lite.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HeadlessContextToggles {
    #[serde(default)]
    pub disable_memory_context: bool,
    #[serde(default)]
    pub disable_task_state_context: bool,
    #[serde(default)]
    pub disable_pool_context: bool,
    #[serde(default)]
    pub disable_project_instructions: bool,
    #[serde(default)]
    pub disable_rolling_summary: bool,
    #[serde(default)]
    pub disable_state_frame: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HeadlessCliRequest {
    pub prompt: String,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub mode: HeadlessCliMode,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub session_title: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub config_dir: Option<String>,
    #[serde(default)]
    pub pool_id: Option<String>,
    #[serde(default)]
    pub pool_name: Option<String>,
    #[serde(default)]
    pub pool_size: Option<u32>,
    #[serde(default)]
    pub koi_ids: Vec<String>,
    #[serde(default)]
    pub task_timeout_secs: Option<u32>,
    #[serde(default)]
    pub wait_for_completion: bool,
    #[serde(default)]
    pub wait_timeout_secs: Option<u64>,
    #[serde(default)]
    pub extra_system_context: Option<String>,
    #[serde(default)]
    pub context_toggles: HeadlessContextToggles,
    #[serde(default)]
    pub output: Option<String>,
}

impl HeadlessCliRequest {
    pub fn app_data_dir_override(&self) -> Option<PathBuf> {
        self.config_dir
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisabledToolInfo {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PoolWaitSummary {
    pub completed: bool,
    pub timed_out: bool,
    pub active_todos: u32,
    pub done_todos: u32,
    pub cancelled_todos: u32,
    pub blocked_todos: u32,
    pub latest_messages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadlessCliResponse {
    pub ok: bool,
    pub mode: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_id: Option<String>,
    pub response_text: String,
    pub disabled_tools: Vec<DisabledToolInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_wait: Option<PoolWaitSummary>,
}

// ─── Pool events & traits ───────────────────────────────────────────────
//
// The pool runtime emits a finite, strongly-typed event stream that hosts
// surface in whatever way suits them: the desktop maps each variant onto
// a Tauri event name (e.g. `pool_message_{id}`), while the CLI writes each
// event as one NDJSON line on stdout. Both hosts share the exact same
// wire shape so that downstream consumers (frontend, python harnesses,
// subprocess readers) see one canonical protocol.

/// Categorical description of a todo row mutation. Used by
/// [`PoolEvent::TodoChanged`] so frontends can run cheap reducer logic
/// without diffing the full snapshot against their local cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoChangeAction {
    Created,
    Updated,
    Claimed,
    Completed,
    Cancelled,
    Blocked,
    Resumed,
    Replaced,
    Deleted,
}

/// Lightweight snapshot of a `KoiTodo` row. Kept as a flat struct so hosts
/// can forward it verbatim to their UI layer without reaching into
/// `pisci_core::models`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoSnapshot {
    pub id: String,
    pub owner_id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub priority: String,
    pub assigned_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_message_id: Option<i64>,
    pub source_type: String,
    #[serde(default)]
    pub task_timeout_secs: u32,
}

impl From<&crate::models::KoiTodo> for TodoSnapshot {
    fn from(t: &crate::models::KoiTodo) -> Self {
        Self {
            id: t.id.clone(),
            owner_id: t.owner_id.clone(),
            title: t.title.clone(),
            description: t.description.clone(),
            status: t.status.clone(),
            priority: t.priority.clone(),
            assigned_by: t.assigned_by.clone(),
            pool_session_id: t.pool_session_id.clone(),
            claimed_by: t.claimed_by.clone(),
            depends_on: t.depends_on.clone(),
            blocked_reason: t.blocked_reason.clone(),
            result_message_id: t.result_message_id,
            source_type: t.source_type.clone(),
            task_timeout_secs: t.task_timeout_secs,
        }
    }
}

/// Flat pool-session snapshot — mirrors `PoolSession` but only carries the
/// fields hosts actually forward to the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolSessionSnapshot {
    pub id: String,
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    #[serde(default)]
    pub task_timeout_secs: u32,
}

impl From<&crate::models::PoolSession> for PoolSessionSnapshot {
    fn from(p: &crate::models::PoolSession) -> Self {
        Self {
            id: p.id.clone(),
            name: p.name.clone(),
            status: p.status.clone(),
            project_dir: p.project_dir.clone(),
            task_timeout_secs: p.task_timeout_secs,
        }
    }
}

/// Structured view of a newly appended pool message. The event carries the
/// full row verbatim (rather than just the id) so frontends can render
/// without a round-trip to the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolMessageSnapshot {
    pub id: i64,
    pub pool_session_id: String,
    pub sender_id: String,
    pub content: String,
    pub msg_type: String,
    #[serde(default)]
    pub metadata: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub todo_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<&crate::models::PoolMessage> for PoolMessageSnapshot {
    fn from(m: &crate::models::PoolMessage) -> Self {
        // `PoolMessage::metadata` is a JSON-encoded string for historical
        // reasons; the wire-level snapshot promotes it back to a `Value`
        // so hosts don't need to re-parse. Fallback to `Null` on malformed
        // stored metadata instead of dropping the event.
        let metadata: Value = if m.metadata.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&m.metadata).unwrap_or(Value::Null)
        };
        Self {
            id: m.id,
            pool_session_id: m.pool_session_id.clone(),
            sender_id: m.sender_id.clone(),
            content: m.content.clone(),
            msg_type: m.msg_type.clone(),
            metadata,
            todo_id: m.todo_id.clone(),
            reply_to_message_id: m.reply_to_message_id,
            event_type: m.event_type.clone(),
            created_at: m.created_at,
        }
    }
}

/// Fully typed pool-layer event stream.
///
/// The kernel emits these; hosts translate each variant into their own
/// transport (Tauri `emit`, stdout NDJSON, websocket, …). Variants are
/// deliberately coarse — one per observable state transition — so every
/// downstream consumer can subscribe exhaustively.
///
/// `#[serde(tag = "kind", rename_all = "snake_case")]` keeps the wire
/// format stable and future-compatible: adding a new variant with a new
/// tag is a non-breaking change for forward-compatible consumers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PoolEvent {
    PoolCreated {
        pool: PoolSessionSnapshot,
    },
    PoolUpdated {
        pool: PoolSessionSnapshot,
    },
    PoolPaused {
        pool: PoolSessionSnapshot,
    },
    PoolResumed {
        pool: PoolSessionSnapshot,
    },
    PoolArchived {
        pool_id: String,
    },

    MessageAppended {
        pool_id: String,
        message: PoolMessageSnapshot,
    },

    TodoChanged {
        pool_id: String,
        action: TodoChangeAction,
        todo: TodoSnapshot,
    },

    KoiAssigned {
        pool_id: String,
        koi_id: String,
        todo_id: String,
    },
    KoiStatusChanged {
        pool_id: String,
        koi_id: String,
        status: String,
    },
    KoiStaleRecovered {
        pool_id: String,
        koi_id: String,
        recovered_todo_count: u32,
    },

    CoordinatorIdle {
        pool_id: String,
    },
    CoordinatorCompleted {
        pool_id: String,
        summary: PoolWaitSummary,
    },
    CoordinatorTimedOut {
        pool_id: String,
        summary: PoolWaitSummary,
    },

    /// Progress frame from a running `call_fish` sub-agent. Forwarded to
    /// the parent session's event stream so the UI can show the worker's
    /// tool calls / partial output.
    FishProgress {
        parent_session_id: String,
        fish_id: String,
        stage: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<Value>,
    },
}

impl PoolEvent {
    /// Stable string key for metrics and tracing.
    pub fn kind(&self) -> &'static str {
        match self {
            PoolEvent::PoolCreated { .. } => "pool_created",
            PoolEvent::PoolUpdated { .. } => "pool_updated",
            PoolEvent::PoolPaused { .. } => "pool_paused",
            PoolEvent::PoolResumed { .. } => "pool_resumed",
            PoolEvent::PoolArchived { .. } => "pool_archived",
            PoolEvent::MessageAppended { .. } => "message_appended",
            PoolEvent::TodoChanged { .. } => "todo_changed",
            PoolEvent::KoiAssigned { .. } => "koi_assigned",
            PoolEvent::KoiStatusChanged { .. } => "koi_status_changed",
            PoolEvent::KoiStaleRecovered { .. } => "koi_stale_recovered",
            PoolEvent::CoordinatorIdle { .. } => "coordinator_idle",
            PoolEvent::CoordinatorCompleted { .. } => "coordinator_completed",
            PoolEvent::CoordinatorTimedOut { .. } => "coordinator_timed_out",
            PoolEvent::FishProgress { .. } => "fish_progress",
        }
    }

    /// Pool id most relevant to this event, when the variant carries one.
    /// Some variants (`FishProgress`) are scoped to a session rather than
    /// a pool, and return `None`.
    pub fn pool_id(&self) -> Option<&str> {
        match self {
            PoolEvent::PoolCreated { pool }
            | PoolEvent::PoolUpdated { pool }
            | PoolEvent::PoolPaused { pool }
            | PoolEvent::PoolResumed { pool } => Some(&pool.id),
            PoolEvent::PoolArchived { pool_id }
            | PoolEvent::MessageAppended { pool_id, .. }
            | PoolEvent::TodoChanged { pool_id, .. }
            | PoolEvent::KoiAssigned { pool_id, .. }
            | PoolEvent::KoiStatusChanged { pool_id, .. }
            | PoolEvent::KoiStaleRecovered { pool_id, .. }
            | PoolEvent::CoordinatorIdle { pool_id }
            | PoolEvent::CoordinatorCompleted { pool_id, .. }
            | PoolEvent::CoordinatorTimedOut { pool_id, .. } => Some(pool_id),
            PoolEvent::FishProgress { .. } => None,
        }
    }
}

/// Host-supplied outlet for [`PoolEvent`]s.
///
/// Implementations must be cheap / non-blocking (or at least
/// fire-and-forget) — the pool runtime calls `emit_pool` synchronously
/// from its state-mutating paths. Long-running transport work (HTTP,
/// file I/O) belongs on a background task inside the implementation.
pub trait PoolEventSink: Send + Sync {
    fn emit_pool(&self, event: &PoolEvent);
}

// A no-op sink is handy for tests and for hosts that consume pool events
// through a different channel (e.g. polling) during a migration window.
pub struct NullPoolEventSink;

impl PoolEventSink for NullPoolEventSink {
    fn emit_pool(&self, _event: &PoolEvent) {}
}

// ─── Subagent runtime ──────────────────────────────────────────────────
//
// `SubagentRuntime` abstracts how a Koi turn is actually executed:
//   * Desktop + CLI in 0.8.0: spawn `openpisci-headless run --mode pisci`
//     as a child process and stream NDJSON back (subprocess isolation,
//     crash containment, identical code path for both hosts).
//   * Tests: `StubSubagentRuntime` that returns a scripted outcome.
//
// The runtime is driven by the kernel pool coordinator (Phase 2.1) and
// exposed to `call_koi` / `call_fish` through `ToolContext`.

/// Input payload for one Koi turn. The coordinator fills this in before
/// handing it to the runtime; the runtime is responsible for materialising
/// the turn (subprocess, in-process, remote …) and reporting back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KoiTurnRequest {
    pub pool_id: String,
    pub koi_id: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub todo_id: Option<String>,
    /// Fully assembled system prompt — the runtime passes this through
    /// verbatim; prompt assembly lives in the coordinator.
    pub system_prompt: String,
    /// User message for the turn (the task brief / pool mention text).
    pub user_prompt: String,
    /// Workspace directory the Koi should operate in (git worktree path
    /// on desktop; arbitrary project dir in bench / CLI contexts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_timeout_secs: Option<u32>,
    /// Opaque list of tool profile hints (e.g. `"browser"`, `"ssh"`).
    /// The subprocess runtime forwards these so the child can enable the
    /// matching neutral / host-specific tools.
    #[serde(default)]
    pub extra_tool_profile: Vec<String>,
    /// Extra system context to inject after the core prompt. Used for
    /// continuity / memory / org_spec slices when the coordinator has
    /// already resolved them and doesn't want the child to reassemble.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_system_context: Option<String>,
}

/// Handle returned by `spawn_koi_turn` — opaque to the coordinator, used
/// only to pair `cancel_koi_turn` / `wait_koi_turn` with the running
/// turn. The concrete runtime may store a subprocess pid, an internal
/// task handle, or whatever it needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KoiTurnHandle {
    pub turn_id: String,
    pub pool_id: String,
    pub koi_id: String,
}

/// Terminal outcome of a Koi turn. Only emitted once per handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KoiTurnOutcome {
    pub handle: KoiTurnHandle,
    pub exit_kind: KoiTurnExit,
    pub response_text: String,
    /// Final assistant response / summary text (already shown to the user
    /// in pool_chat by the kernel services).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KoiTurnExit {
    Completed,
    Cancelled,
    TimedOut,
    Crashed,
}

/// Abstraction over how a Koi turn is materialised. Subprocess-backed in
/// 0.8.0; stub-backed in tests; potentially remote-backed later. The
/// coordinator never calls `fork`, `Command` or `tokio::spawn` directly.
#[async_trait]
pub trait SubagentRuntime: Send + Sync {
    async fn spawn_koi_turn(&self, request: KoiTurnRequest) -> anyhow::Result<KoiTurnHandle>;
    async fn cancel_koi_turn(&self, handle: &KoiTurnHandle) -> anyhow::Result<()>;
    async fn wait_koi_turn(&self, handle: &KoiTurnHandle) -> anyhow::Result<KoiTurnOutcome>;
}

// ─── Pool coordinator run shape ────────────────────────────────────────
//
// `PoolRunRequest` / `PoolRunResponse` are the kernel-level contract for
// "run one pool coordinator turn (or loop until idle) against this pool".
// Both the headless CLI `openpisci-headless pool` and the desktop
// `openpisci --mode pool` drive the same function in Phase 3 — no host
// owns its own copy of the loop.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolRunRequest {
    pub pool_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// When true, the coordinator runs until idle (`active_todos == 0`
    /// stable for ≥ `idle_window_secs`) or `wait_timeout_secs` elapses.
    /// When false, exactly one coordinator turn runs and returns.
    #[serde(default)]
    pub run_until_idle: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_window_secs: Option<u64>,
    #[serde(default)]
    pub context_toggles: HeadlessContextToggles,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolRunResponse {
    pub ok: bool,
    pub pool_id: String,
    pub session_id: String,
    pub response_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<PoolWaitSummary>,
}

// ─── HumanGate ────────────────────────────────────────────────────────
//
// `HumanGate` is a thin abstraction for "pause the agent and ask the
// user something". It sits alongside `Notifier` but carries pool-aware
// context and a scoped default — the CLI gate auto-confirms (or denies,
// via env var) without user input, while the desktop gate surfaces a
// modal. Kept as a distinct trait so a future CI run can inject a
// scripted gate without touching `Notifier`.

#[async_trait]
pub trait HumanGate: Send + Sync {
    /// Yes/no gate that blocks until the user answers (or the implementor
    /// decides based on policy / env var).
    async fn confirm(&self, req: ConfirmRequest) -> bool;
    /// Richer interactive prompt; the returned JSON matches the request
    /// `kind` contract.
    async fn interact(&self, req: InteractiveRequest) -> Value;
}

/// Trivial always-`default` gate — handy for CLI and tests.
pub struct DefaultAnswerHumanGate;

#[async_trait]
impl HumanGate for DefaultAnswerHumanGate {
    async fn confirm(&self, req: ConfirmRequest) -> bool {
        req.default.unwrap_or(false)
    }
    async fn interact(&self, req: InteractiveRequest) -> Value {
        req.default.unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tool_registry_handle_tests {
    use super::ToolRegistryHandle;

    // A stand-in registry to avoid a dependency back on the kernel.
    #[derive(Default)]
    struct FakeRegistry {
        names: Vec<String>,
    }

    #[test]
    fn downcast_mut_and_ref_roundtrip() {
        let mut h = ToolRegistryHandle::new(FakeRegistry::default());
        h.downcast_mut::<FakeRegistry>()
            .expect("downcast_mut")
            .names
            .push("shell".into());
        let r = h.downcast_ref::<FakeRegistry>().expect("downcast_ref");
        assert_eq!(r.names, vec!["shell".to_string()]);
    }

    #[test]
    fn with_mut_folds_downcast_and_closure() {
        let mut h = ToolRegistryHandle::new(FakeRegistry::default());
        let pushed = h
            .with_mut::<FakeRegistry, _>(|r| {
                r.names.push("file_read".into());
                r.names.len()
            })
            .expect("type matches");
        assert_eq!(pushed, 1);
    }

    #[test]
    fn with_mut_returns_none_on_type_mismatch() {
        let mut h = ToolRegistryHandle::new(FakeRegistry::default());
        // Wrong type — should not panic, just return None.
        let r: Option<()> = h.with_mut::<String, _>(|_| ());
        assert!(r.is_none());
    }

    #[test]
    fn into_inner_recovers_value_or_returns_handle() {
        let h = ToolRegistryHandle::new(FakeRegistry {
            names: vec!["shell".into()],
        });
        let recovered = h.into_inner::<FakeRegistry>().ok().expect("match");
        assert_eq!(recovered.names, vec!["shell".to_string()]);

        let h2 = ToolRegistryHandle::new(42u32);
        let err = h2.into_inner::<FakeRegistry>();
        assert!(err.is_err(), "should return Err(self) on mismatch");
        // And we can still try a different type on the returned handle.
        let still_u32: u32 = err.err().unwrap().into_inner::<u32>().ok().unwrap();
        assert_eq!(still_u32, 42);
    }

    #[test]
    fn type_name_reports_inner_type() {
        let h = ToolRegistryHandle::new(FakeRegistry::default());
        assert!(h.type_name().contains("FakeRegistry"));
    }
}
