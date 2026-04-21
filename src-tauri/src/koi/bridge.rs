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
//!   * We build a fresh `SubprocessSubagentRuntime` per call via
//!     `DesktopHost::from_state` / `build_deps_from_db`. The runtime
//!     itself is cheap (an `Arc<Mutex<HashMap<_,_>>>`) and the subprocess
//!     spawn only happens when the kernel actually fans out a turn.
//!   * All helpers return kernel-shaped results so command handlers can
//!     serialise them back to the frontend unchanged.

use std::path::PathBuf;
use std::sync::Arc;

use pisci_core::host::{HostRuntime, PoolEventSink, SubagentRuntime};
use pisci_core::models::KoiTodo;
use pisci_kernel::pool::coordinator::{self, CoordinatorConfig, KoiExecResult};
use pisci_kernel::pool::store::PoolStore;
use pisci_kernel::store::Database;
use tauri::AppHandle;
use tokio::sync::Mutex;

use crate::host::{DesktopEventSink, DesktopHost};
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
fn collect_deps(app: &AppHandle, state: &AppState) -> Option<Deps> {
    let host = DesktopHost::from_state(app.clone(), state);
    let sink = host.pool_event_sink();
    let subagent = host.subagent_runtime()?;
    Some(Deps {
        store: PoolStore::new(state.db.clone()),
        sink,
        subagent,
        cfg: CoordinatorConfig::default(),
    })
}

/// Build [`Deps`] from the raw DB handle + app handle. Used by
/// background `tokio::spawn` tasks where the borrowed `AppState` can't
/// cross the spawn boundary.
///
/// This duplicates the binary-resolution policy from
/// [`DesktopHost::from_state`] — if that policy ever needs to change,
/// both paths must be kept in sync.
fn build_deps_from_db(app: &AppHandle, db: Arc<Mutex<Database>>) -> Deps {
    let sink: Arc<dyn PoolEventSink> = Arc::new(DesktopEventSink::new(app.clone()));
    let subagent = build_subagent_runtime(app);
    Deps {
        store: PoolStore::new(db),
        sink,
        subagent,
        cfg: CoordinatorConfig::default(),
    }
}

fn build_subagent_runtime(app: &AppHandle) -> Arc<dyn SubagentRuntime> {
    use tauri::Manager;
    let app_data_dir = app
        .path()
        .app_data_dir()
        .ok()
        .or_else(|| Some(PathBuf::from(".pisci")));
    let headless_bin = resolve_headless_binary();
    let subprocess = pisci_kernel::pool::SubprocessSubagentRuntime::new(headless_bin);
    let subprocess = if let Some(ref dir) = app_data_dir {
        subprocess.with_app_data_dir(dir.clone())
    } else {
        subprocess
    };
    Arc::new(subprocess)
}

fn resolve_headless_binary() -> PathBuf {
    if let Ok(raw) = std::env::var("PISCI_HEADLESS_BIN") {
        let raw = raw.trim();
        if !raw.is_empty() {
            return PathBuf::from(raw);
        }
    }
    let exe_name = if cfg!(windows) {
        "openpisci-headless.exe"
    } else {
        "openpisci-headless"
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(exe_name);
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from(exe_name)
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
    let deps = build_deps_from_db(app, db);
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
        .ok_or_else(|| anyhow::anyhow!("desktop host has no subagent runtime wired"))?;
    coordinator::execute_todo_turn(&deps.store, deps.sink, deps.subagent, &deps.cfg, args).await
}
