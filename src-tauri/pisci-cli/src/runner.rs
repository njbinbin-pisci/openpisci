//! Shared helpers for running a single pisci-mode agent turn, and
//! bootstrapping a pool-mode parent, from any headless host.
//!
//! `openpisci-headless` dispatches into [`run_pisci_once`] for pisci-mode
//! requests so every headless single-agent run uses the same kernel code
//! path — same tool registry, same event sink wiring, same timeout
//! semantics.
//!
//! [`run_pool_once`] is the minimum-viable headless parent for
//! `openpisci-headless run --mode pool`: it creates (or attaches to) a
//! pool session, posts the user prompt as a pool message, and (if
//! requested) waits for all todos to resolve. Koi turns themselves run
//! as subprocesses via [`pisci_kernel::pool::SubprocessSubagentRuntime`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pisci_core::host::{
    HeadlessCliMode, HeadlessCliRequest, HeadlessCliResponse, HostRuntime, PoolWaitSummary,
    SubagentRuntime, ToolRegistryHandle,
};
use pisci_kernel::agent::plan::new_plan_store;
use pisci_kernel::agent::tool::{new_tool_registry_handle, ToolRegistryHandleExt};
use pisci_kernel::headless::{self, HeadlessDeps};
use pisci_kernel::pool::coordinator::CoordinatorConfig;
use pisci_kernel::pool::model::{CallerContext, CreatePoolArgs, SendPoolMessageArgs};
use pisci_kernel::pool::services as pool_services;
use pisci_kernel::pool::store::PoolStore;
use pisci_kernel::pool::SubprocessSubagentRuntime;
use pisci_kernel::tools::NeutralToolsConfig;

use crate::host::CliHost;

/// Resolve the app data directory for a headless run. Honours the
/// per-request `config_dir` override first, then the
/// `OPENPISCI_CONFIG_DIR` env var, then the OS default (`dirs::data_dir`).
pub fn resolve_app_data_dir(request: Option<&HeadlessCliRequest>) -> PathBuf {
    if let Some(req) = request {
        if let Some(dir) = req.app_data_dir_override() {
            return dir;
        }
    }
    std::env::var("OPENPISCI_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("pisci")
        })
}

/// Execute a single pisci-mode turn end-to-end.
///
/// This is the canonical `pisci-once` pipeline:
///
/// 1. Reject pool mode (desktop-only).
/// 2. Open kernel state (`pisci.db` + `config.json`) under
///    `resolve_app_data_dir`.
/// 3. Register the full neutral tool set; let the [`CliHost`] layer any
///    additional host tools (a no-op today).
/// 4. Build [`HeadlessDeps`] with the `CliEventSink` — stdout NDJSON.
/// 5. Drive [`pisci_kernel::headless::run_pisci_turn`] on a fresh
///    multi-threaded tokio runtime and return the response.
///
/// Errors are flattened into `String` so the binary entry points can
/// `eprintln!` them directly without extra wrapping.
pub fn run_pisci_once(request: HeadlessCliRequest) -> Result<HeadlessCliResponse, String> {
    if matches!(request.mode, HeadlessCliMode::Pool) {
        return run_pool_once(request);
    }

    let app_data_dir = resolve_app_data_dir(Some(&request));
    let host = CliHost::new(app_data_dir.clone());

    let (db, settings) = headless::open_kernel_state(&app_data_dir)
        .map_err(|e| format!("Failed to initialise kernel state: {e}"))?;

    let mut handle: ToolRegistryHandle = new_tool_registry_handle();
    // Headless pisci turns participate in pool flows too (a Koi turn
    // running as its own subprocess still reads/writes the shared
    // `pool_sessions` DB), so we wire the full neutral-tool deps:
    //   • `event_sink` / `plan_store`   → enable `plan_todo`
    //   • `pool_event_sink`             → enable `pool_org` / `pool_chat`
    //   • `subagent_runtime`            → left `None` for the Pisci
    //     headless path itself; mention fan-out inside a Pisci turn
    //     surfaces a clean "no subagent runtime" error instead of
    //     silently recursing. Hosts that drive Koi subprocesses wire
    //     `SubprocessSubagentRuntime` at the top level (desktop app +
    //     `openpisci-headless run --mode pool`).
    let neutral_cfg = NeutralToolsConfig {
        db: Some(db.clone()),
        settings: Some(settings.clone()),
        builtin_tool_enabled: None,
        user_tools_dir: Some(app_data_dir.join("user_tools")),
        event_sink: Some(host.event_sink()),
        plan_store: Some(new_plan_store()),
        pool_event_sink: Some(host.pool_event_sink()),
        subagent_runtime: None,
        coordinator_config: Default::default(),
    };
    pisci_kernel::tools::register_neutral_tools(&mut handle, &neutral_cfg);
    host.host_tools().register(&mut handle);
    let registry = handle
        .into_registry()
        .map_err(|_| "internal: registry handle type mismatch".to_string())?;

    let deps = HeadlessDeps::new(db, settings, registry, host.event_sink());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to start tokio runtime: {e}"))?;

    runtime
        .block_on(headless::run_pisci_turn(request, deps))
        .map_err(|e| format!("run_pisci_turn failed: {e}"))
}

/// Resolve the `openpisci-headless` binary this process is currently
/// running as. The subprocess subagent runtime needs this path to spawn
/// Koi children via `openpisci-headless rpc`.
///
/// Lookup order: `PISCI_HEADLESS_BIN` env var → `current_exe()` → fall
/// back to the bare name and let the OS resolve it via `PATH`.
fn resolve_headless_binary() -> PathBuf {
    if let Ok(raw) = std::env::var("PISCI_HEADLESS_BIN") {
        let raw = raw.trim();
        if !raw.is_empty() {
            return PathBuf::from(raw);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        return exe;
    }
    if cfg!(windows) {
        PathBuf::from("openpisci-headless.exe")
    } else {
        PathBuf::from("openpisci-headless")
    }
}

/// Minimum-viable pool-mode parent runner.
///
/// This is the `openpisci-headless run --mode pool` entry point. It
/// creates (or attaches to) a pool session, posts the caller's prompt as
/// a pool message (which may contain `@koi` mentions the kernel's
/// coordinator will fan out into subprocess Koi turns), and optionally
/// waits for completion before returning a [`PoolWaitSummary`].
///
/// Scope of the MVP:
///   * Only creates a new pool if `pool_id` is absent; attaching to an
///     existing pool just sends the message.
///   * Polls the DB every second for todo completion when
///     `wait_for_completion` is set, bounded by
///     `wait_timeout_secs` (defaults to 10 minutes).
///   * Koi turns run as `openpisci-headless rpc` subprocesses via
///     [`SubprocessSubagentRuntime`].
pub fn run_pool_once(request: HeadlessCliRequest) -> Result<HeadlessCliResponse, String> {
    let app_data_dir = resolve_app_data_dir(Some(&request));

    let headless_bin = resolve_headless_binary();
    let subagent = SubprocessSubagentRuntime::new(headless_bin).with_app_data_dir(&app_data_dir);
    let subagent: Arc<dyn SubagentRuntime> = Arc::new(subagent);

    let host = CliHost::new(app_data_dir.clone()).with_subagent_runtime(subagent.clone());

    let (db, settings) = headless::open_kernel_state(&app_data_dir)
        .map_err(|e| format!("Failed to initialise kernel state: {e}"))?;

    let mut handle: ToolRegistryHandle = new_tool_registry_handle();
    let coordinator_config = CoordinatorConfig::default();
    let neutral_cfg = NeutralToolsConfig {
        db: Some(db.clone()),
        settings: Some(settings.clone()),
        builtin_tool_enabled: None,
        user_tools_dir: Some(app_data_dir.join("user_tools")),
        event_sink: Some(host.event_sink()),
        plan_store: Some(new_plan_store()),
        pool_event_sink: Some(host.pool_event_sink()),
        subagent_runtime: Some(subagent.clone()),
        coordinator_config: coordinator_config.clone(),
    };
    pisci_kernel::tools::register_neutral_tools(&mut handle, &neutral_cfg);
    host.host_tools().register(&mut handle);
    let _registry = handle
        .into_registry()
        .map_err(|_| "internal: registry handle type mismatch".to_string())?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to start tokio runtime: {e}"))?;

    runtime.block_on(async move {
        let store = PoolStore::new(db.clone());
        let sink = host.pool_event_sink();

        // Resolve pool id — create a fresh pool if the caller didn't
        // specify one. Session id doubles as the initial messaging
        // channel: the kernel keys session -> pool via the `pool_session_id`
        // column.
        let pool_id = if let Some(id) = request.pool_id.clone().filter(|s| !s.is_empty()) {
            id
        } else {
            let pool_name = request
                .pool_name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "headless-pool".to_string());
            let caller_session = request.session_id.clone().unwrap_or_default();
            let caller = CallerContext {
                memory_owner_id: "cli",
                session_id: &caller_session,
                session_source: Some("cli"),
                pool_session_id: None,
                cancel: None,
            };
            let created = pool_services::create_pool(
                &store,
                sink.as_ref(),
                &caller,
                CreatePoolArgs {
                    name: pool_name,
                    project_dir: request.workspace.clone(),
                    org_spec: None,
                    task_timeout_secs: request.task_timeout_secs.unwrap_or(600),
                    origin_im_binding_key: None,
                },
            )
            .await
            .map_err(|e| format!("create_pool failed: {e}"))?;
            created
                .get("pool")
                .and_then(|p| p.get("id"))
                .and_then(|id| id.as_str())
                .map(str::to_string)
                .ok_or_else(|| "create_pool returned no pool.id".to_string())?
        };

        // Post the user's prompt as a pool message. If it contains
        // `@koi` mentions the coordinator dispatches subprocess turns.
        let caller_session = request
            .session_id
            .clone()
            .unwrap_or_else(|| pool_id.clone());
        let caller = CallerContext {
            memory_owner_id: "cli",
            session_id: &caller_session,
            session_source: Some("cli"),
            pool_session_id: Some(&pool_id),
            cancel: None,
        };
        let _msg = pool_services::send_pool_message(
            &store,
            sink.clone(),
            Some(subagent.clone()),
            &coordinator_config,
            &caller,
            SendPoolMessageArgs {
                pool_id: pool_id.clone(),
                sender_id: "user".to_string(),
                content: request.prompt.clone(),
                reply_to_message_id: None,
            },
        )
        .await
        .map_err(|e| format!("send_pool_message failed: {e}"))?;

        let pool_wait = if request.wait_for_completion {
            Some(wait_for_pool_idle(&store, &pool_id, request.wait_timeout_secs).await)
        } else {
            None
        };

        Ok(HeadlessCliResponse {
            ok: true,
            mode: HeadlessCliMode::Pool.as_str().to_string(),
            session_id: caller_session,
            pool_id: Some(pool_id),
            response_text: String::new(),
            disabled_tools: Vec::new(),
            pool_wait,
        })
    })
}

async fn wait_for_pool_idle(
    store: &PoolStore,
    pool_id: &str,
    wait_timeout_secs: Option<u64>,
) -> PoolWaitSummary {
    let deadline = Instant::now() + Duration::from_secs(wait_timeout_secs.unwrap_or(600));
    let poll = Duration::from_secs(1);
    loop {
        let pool_id_owned = pool_id.to_string();
        let counts = store
            .read(move |db| {
                let mut active = 0u32;
                let mut done = 0u32;
                let mut cancelled = 0u32;
                let mut blocked = 0u32;
                for t in db.list_koi_todos(None)?.into_iter() {
                    if t.pool_session_id.as_deref() != Some(&pool_id_owned) {
                        continue;
                    }
                    match t.status.as_str() {
                        "done" | "completed" => done += 1,
                        "cancelled" => cancelled += 1,
                        "blocked" => blocked += 1,
                        _ => active += 1,
                    }
                }
                Ok::<_, anyhow::Error>((active, done, cancelled, blocked))
            })
            .await
            .unwrap_or((0, 0, 0, 0));
        let (active, done, cancelled, blocked) = counts;
        let timed_out = Instant::now() >= deadline;
        if active == 0 || timed_out {
            return PoolWaitSummary {
                completed: active == 0,
                timed_out,
                active_todos: active,
                done_todos: done,
                cancelled_todos: cancelled,
                blocked_todos: blocked,
                latest_messages: Vec::new(),
            };
        }
        tokio::time::sleep(poll).await;
    }
}
