//! Input-argument structs for `pool::services`.
//!
//! Kept as plain structs (not `serde` inputs) so the caller — a tool
//! wrapper or kernel test — builds them explicitly. JSON parsing stays
//! in the tool layer; the services never see a raw `Value` for their
//! own arguments.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Caller identity + cancellation token forwarded from the agent loop.
///
/// `memory_owner_id` is either `"pisci"` (the coordinator) or the Koi's
/// id when a Koi-scene turn invoked the tool. Services enforce the
/// "pisci may do anything / koi can only manage its own todos" rule
/// from this field.
#[derive(Clone)]
pub struct CallerContext<'a> {
    pub memory_owner_id: &'a str,
    pub session_id: &'a str,
    pub session_source: Option<&'a str>,
    pub pool_session_id: Option<&'a str>,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl<'a> CallerContext<'a> {
    pub fn is_pisci(&self) -> bool {
        self.memory_owner_id == "pisci"
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .map(|f| f.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Default)]
pub struct CreatePoolArgs {
    pub name: String,
    /// Optional filesystem directory to bind the project to. When set,
    /// the service initialises a git repo (via [`crate::pool::git`])
    /// before creating the DB row.
    pub project_dir: Option<String>,
    pub org_spec: Option<String>,
    pub task_timeout_secs: u32,
    /// Optional IM binding (`im_session_bindings.binding_key`) that
    /// originated this pool — set when an IM-driven Pisci session
    /// requests the pool. Pool-level events (heartbeat alerts,
    /// decision requests) fan out to this binding alongside the UI
    /// toast.
    pub origin_im_binding_key: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateOrgSpecArgs {
    pub pool_id: String,
    pub org_spec: Option<String>,
    pub task_timeout_secs: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct SendPoolMessageArgs {
    pub pool_id: String,
    pub sender_id: String,
    pub content: String,
    pub reply_to_message_id: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct AssignKoiArgs {
    pub pool_id: String,
    pub koi_id: String,
    pub task: String,
    pub priority: String,
    pub timeout_secs: u32,
}

#[derive(Debug, Clone, Default)]
pub struct PostStatusArgs {
    pub pool_id: String,
    pub content: String,
    pub event_type: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct WaitForKoiArgs {
    pub pool_id: String,
    pub koi_id: Option<String>,
    pub todo_id: Option<String>,
    pub min_wait_secs: u64,
    pub timeout_secs: u64,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct CreateTodoArgs {
    pub pool_id: String,
    pub title: String,
    pub description: String,
    pub priority: String,
    pub timeout_secs: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ReplaceTodoArgs {
    pub todo_id: String,
    pub new_owner_id: String,
    pub task: String,
    pub reason: String,
    pub timeout_secs: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct DeleteTodoArgs {
    pub todo_id: Option<String>,
    pub pool_id: Option<String>,
    pub status: Option<String>,
    pub owner_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateTodoStatusArgs {
    pub todo_id: String,
    pub new_status: String,
}
