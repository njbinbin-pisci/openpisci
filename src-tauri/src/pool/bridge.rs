//! Thin desktop → kernel coordinator bridge.
//!
//! The desktop command layer (commands/pool.rs, commands/board.rs,
//! commands/collab_trial.rs) needs to invoke mention dispatch / todo
//! resume / todo replace from Tauri command handlers. Rather than hold
//! the plumbing (PoolStore + PoolEventSink + SubagentRuntime +
//! CoordinatorConfig) on every caller, these helpers rebuild them on
//! demand from either an [`AppState`] reference (for synchronous command
//! bodies) or from the raw [`Arc<Mutex<Database>>`] + [`AppHandle`]
//! handles (for spawned background tasks that can't hold the command's
//! borrowed `State<_>`).
//!
//! Implementation notes:
//!   * We build a fresh desktop in-process [`SubagentRuntime`] per call via
//!     `DesktopHost::from_state` / `build_deps_from_db`. The runtime
//!     itself is cheap; Koi work runs inside the GUI product process by
//!     default instead of requiring an `openpisci-headless` sidecar.
//!   * All helpers return kernel-shaped results so command handlers can
//!     serialise them back to the frontend unchanged.

use std::sync::Arc;

use pisci_core::host::{HostRuntime, PoolEventSink, SubagentRuntime};
use pisci_core::models::KoiTodo;
use pisci_kernel::pool::coordinator::{self, CoordinatorConfig, KoiExecResult};
use pisci_kernel::pool::store::PoolStore;
use pisci_kernel::store::Database;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

use crate::host::{DesktopEventSink, DesktopHost};
use crate::runtime::koi::DesktopInProcessSubagentRuntime;
use crate::store::AppState;

/// Bundle of kernel-side dependencies the coordinator needs.
struct Deps {
    store: PoolStore,
    sink: Arc<dyn PoolEventSink>,
    subagent: Arc<dyn SubagentRuntime>,
    cfg: CoordinatorConfig,
}

/// Build [`Deps`] by going through `DesktopHost::from_state`. Used by
/// command bodies that still hold the Tauri `State<'_, AppState>`.
async fn collect_deps(app: &AppHandle, state: &AppState) -> Option<Deps> {
    let host = DesktopHost::from_state(app.clone(), state);
    let sink = host.pool_event_sink();
    let subagent = host.subagent_runtime()?;
    let cfg = resolve_coordinator_config(app).await;
    Some(Deps {
        store: PoolStore::new(state.db.clone()),
        sink,
        subagent,
        cfg,
    })
}

/// Build [`Deps`] from the raw DB handle + app handle. Used by
/// background `tokio::spawn` tasks where the borrowed `AppState` can't
/// cross the spawn boundary.
///
async fn build_deps_from_db(app: &AppHandle, db: Arc<Mutex<Database>>) -> Deps {
    let sink: Arc<dyn PoolEventSink> = Arc::new(DesktopEventSink::new(app.clone()));
    let subagent = build_subagent_runtime(app);
    let cfg = resolve_coordinator_config(app).await;
    Deps {
        store: PoolStore::new(db),
        sink,
        subagent,
        cfg,
    }
}

async fn resolve_coordinator_config(app: &AppHandle) -> CoordinatorConfig {
    let mut cfg = CoordinatorConfig::default();
    if let Some(state) = app.try_state::<AppState>() {
        let secs = state.settings.lock().await.koi_timeout_secs;
        if secs > 0 {
            cfg.default_task_timeout_secs = secs;
        }
    }
    cfg
}

fn build_subagent_runtime(app: &AppHandle) -> Arc<dyn SubagentRuntime> {
    Arc::new(DesktopInProcessSubagentRuntime::new(app.clone()))
}

// ─── Mention dispatch ──────────────────────────────────────────────────

/// Desktop-side wrapper around `coordinator::handle_mention`, taking an
/// [`AppState`] reference (for synchronous command bodies).
pub async fn handle_mention(
    app: &AppHandle,
    state: &AppState,
    sender_id: &str,
    pool_session_id: &str,
    content: &str,
) -> anyhow::Result<()> {
    let deps = collect_deps(app, state)
        .await
        .ok_or_else(|| anyhow::anyhow!("desktop host has no subagent runtime wired"))?;
    coordinator::handle_mention(
        &deps.store,
        deps.sink,
        deps.subagent,
        &deps.cfg,
        sender_id,
        pool_session_id,
        content,
    )
    .await
}

/// Same as [`handle_mention`] but for spawned background tasks that
/// only have the raw DB handle.
pub async fn handle_mention_arc(
    app: &AppHandle,
    db: Arc<Mutex<Database>>,
    sender_id: &str,
    pool_session_id: &str,
    content: &str,
) -> anyhow::Result<()> {
    let deps = build_deps_from_db(app, db).await;
    coordinator::handle_mention(
        &deps.store,
        deps.sink,
        deps.subagent,
        &deps.cfg,
        sender_id,
        pool_session_id,
        content,
    )
    .await
}

// ─── Todo lifecycle ────────────────────────────────────────────────────

pub async fn resume_todo(
    app: &AppHandle,
    state: &AppState,
    todo_id: &str,
    triggered_by: &str,
) -> anyhow::Result<KoiTodo> {
    let deps = collect_deps(app, state)
        .await
        .ok_or_else(|| anyhow::anyhow!("desktop host has no subagent runtime wired"))?;
    coordinator::resume_blocked_todo(
        &deps.store,
        deps.sink,
        deps.subagent,
        &deps.cfg,
        todo_id,
        triggered_by,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn replace_todo(
    app: &AppHandle,
    state: &AppState,
    todo_id: &str,
    new_owner_id: &str,
    task: &str,
    reason: &str,
    triggered_by: &str,
    task_timeout_secs: Option<u32>,
) -> anyhow::Result<KoiTodo> {
    let deps = collect_deps(app, state)
        .await
        .ok_or_else(|| anyhow::anyhow!("desktop host has no subagent runtime wired"))?;
    coordinator::replace_blocked_todo(
        &deps.store,
        deps.sink,
        deps.subagent,
        &deps.cfg,
        todo_id,
        new_owner_id,
        task,
        reason,
        triggered_by,
        task_timeout_secs,
    )
    .await
}

/// Drive a single Koi turn end-to-end. Exposed for test harnesses and
/// any future command that needs direct turn execution without going
/// through mention parsing.
pub async fn execute_todo_turn(
    app: &AppHandle,
    state: &AppState,
    args: coordinator::ExecuteTodoArgs,
) -> anyhow::Result<KoiExecResult> {
    let deps = collect_deps(app, state)
        .await
        .ok_or_else(|| anyhow::anyhow!("desktop host has no subagent runtime wired"))?;
    coordinator::execute_todo_turn(&deps.store, deps.sink, deps.subagent, &deps.cfg, args).await
}

// ─── Patrol helpers ────────────────────────────────────────────────────

/// Activate pending todos for a specific pool (or the pool-free
/// backlog when `pool_session_id` is `None`). Returns how many turns
/// were dispatched.
pub async fn activate_pending_todos_arc(
    app: &AppHandle,
    db: Arc<Mutex<Database>>,
    pool_session_id: Option<&str>,
) -> anyhow::Result<u32> {
    let deps = build_deps_from_db(app, db).await;
    coordinator::activate_pending_todos(
        &deps.store,
        deps.sink,
        deps.subagent,
        &deps.cfg,
        pool_session_id,
    )
    .await
}

/// Same as [`activate_pending_todos_arc`] but from a command handler
/// that still holds a borrowed [`AppState`].
pub async fn activate_pending_todos(
    app: &AppHandle,
    state: &AppState,
    pool_session_id: Option<&str>,
) -> anyhow::Result<u32> {
    let deps = collect_deps(app, state)
        .await
        .ok_or_else(|| anyhow::anyhow!("desktop host has no subagent runtime wired"))?;
    coordinator::activate_pending_todos(
        &deps.store,
        deps.sink,
        deps.subagent,
        &deps.cfg,
        pool_session_id,
    )
    .await
}

/// Watchdog recovery: roll back stale busy Kois + in_progress todos.
/// Does not need a subagent runtime (pure DB pass-through).
pub async fn watchdog_recover(db: Arc<Mutex<Database>>, max_busy_secs: i64) -> (u32, u32) {
    let store = PoolStore::new(db);
    coordinator::watchdog_recover(&store, max_busy_secs).await
}

/// Direct-assign path: create a fresh todo for `koi_id` without a pool
/// session and run it end-to-end.
#[allow(clippy::too_many_arguments)]
pub async fn assign_and_execute(
    app: &AppHandle,
    state: &AppState,
    koi_id: &str,
    task: &str,
    assigned_by: &str,
    priority: &str,
    task_timeout_secs: Option<u32>,
) -> anyhow::Result<KoiExecResult> {
    let deps = collect_deps(app, state)
        .await
        .ok_or_else(|| anyhow::anyhow!("desktop host has no subagent runtime wired"))?;
    coordinator::assign_and_execute(
        &deps.store,
        deps.sink,
        deps.subagent,
        &deps.cfg,
        koi_id,
        task,
        assigned_by,
        priority,
        task_timeout_secs,
    )
    .await
}
