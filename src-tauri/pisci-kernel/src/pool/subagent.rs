//! Subprocess-backed [`SubagentRuntime`] implementation + a stub for tests.
//!
//! # Wire protocol (v1)
//!
//! The parent (desktop / CLI host) drives a child `openpisci-headless`
//! process via **newline-delimited JSON-RPC 2.0** over stdin / stdout.
//! One message per line, no Content-Length framing (the LSP style is
//! overkill for our two-party, low-volume stream).
//!
//! ## Methods the parent can call (parent → child requests)
//!
//! * `koi.turn` (params: [`KoiTurnRequest`]) → [`KoiTurnOutcome`] —
//!   run a single Koi turn end-to-end inside the child. The child must
//!   reply exactly once with either `result` or `error`.
//! * `koi.cancel` (params: `{"turn_id": "<string>"}`) → `null` — set the
//!   child's cancel flag for the in-flight turn. The subsequent
//!   `koi.turn` response will carry `exit_kind = "cancelled"`.
//! * `shutdown` (no params) → `null` — child finishes any in-flight turn,
//!   then exits 0.
//!
//! ## Notifications the child may emit (child → parent, no `id`)
//!
//! * `event.agent` — a serialised `pisci_core::agent::AgentEvent` (tool
//!   calls, streaming text …). Parents that surface per-event UI route
//!   these straight into their `EventSink`.
//! * `event.pool` — a serialised [`pisci_core::host::PoolEvent`] (the
//!   child may mutate shared DB state and want the parent to broadcast
//!   the event to its UI without a double DB read).
//! * `log` (params: `{"level": "<trace|debug|info|warn|error>",
//!   "target": "<string>", "message": "<string>"}`) — surfaced via
//!   `tracing::event!`.
//!
//! Any other notification is ignored with a `tracing::warn!` so the
//! protocol can grow forward-compatibly.
//!
//! # Cancellation model
//!
//! `cancel_koi_turn` sends a `koi.cancel` notification (no response
//! awaited) — cheap, doesn't block the parent. If the child doesn't exit
//! within [`SubprocessSubagentRuntime::force_kill_after`] of that call,
//! `wait_koi_turn` fires a hard SIGKILL / TerminateProcess and reports
//! `KoiTurnExit::Cancelled` with a `"killed_after_cancel_grace"` error.
//!
//! # Error mapping
//!
//! * Child exits non-zero before replying → `KoiTurnExit::Crashed` with
//!   the captured stderr tail.
//! * RPC error object returned → `KoiTurnExit::Crashed` with the JSON-RPC
//!   error message.
//! * Host-side timeout (`KoiTurnRequest::task_timeout_secs`) → `koi.cancel`
//!   sent first, then `KoiTurnExit::TimedOut` reported regardless of the
//!   eventual `result`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pisci_core::host::{
    KoiTurnExit, KoiTurnHandle, KoiTurnOutcome, KoiTurnRequest, SubagentRuntime,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;
use uuid::Uuid;

// ─── Wire types ──────────────────────────────────────────────────────

/// Raw JSON-RPC frame (request / response / notification). `id` is
/// present iff the frame is a request or response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcFrame {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Method names, kept as constants so typos show up as clippy errors
/// rather than runtime surprises.
pub mod method {
    pub const KOI_TURN: &str = "koi.turn";
    pub const KOI_CANCEL: &str = "koi.cancel";
    pub const SHUTDOWN: &str = "shutdown";

    pub const EVENT_AGENT: &str = "event.agent";
    pub const EVENT_POOL: &str = "event.pool";
    pub const LOG: &str = "log";
}

/// Convenience helper for hosts that want to render a log notification
/// through the normal `tracing` macros.
pub fn dispatch_log_notification(params: &Value) {
    let level = params
        .get("level")
        .and_then(|v| v.as_str())
        .unwrap_or("info");
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let target = params
        .get("target")
        .and_then(|v| v.as_str())
        .unwrap_or("subagent");
    match level {
        "error" => tracing::error!(target: "subagent", child_target = %target, "{}", message),
        "warn" => tracing::warn!(target: "subagent", child_target = %target, "{}", message),
        "debug" => tracing::debug!(target: "subagent", child_target = %target, "{}", message),
        "trace" => tracing::trace!(target: "subagent", child_target = %target, "{}", message),
        _ => tracing::info!(target: "subagent", child_target = %target, "{}", message),
    }
}

// ─── SubprocessSubagentRuntime ───────────────────────────────────────

/// Spawns `openpisci-headless rpc` as a child process per turn and
/// drives it via line-delimited JSON-RPC.
///
/// The runtime is cheap to `Arc<_>` and clone; each `spawn_koi_turn`
/// creates a fresh child and registers it in an in-memory map keyed by
/// `turn_id`, so `cancel_koi_turn` / `wait_koi_turn` can find it without
/// the caller keeping a side handle.
pub struct SubprocessSubagentRuntime {
    binary: PathBuf,
    app_data_dir: Option<PathBuf>,
    /// Extra environment variables to pass through (e.g. `OPENPISCI_LLM_*`).
    extra_env: Vec<(String, String)>,
    /// After sending `koi.cancel`, how long to wait before SIGKILL.
    force_kill_after: Duration,
    inflight: Arc<Mutex<HashMap<String, InflightTurn>>>,
    /// Optional forwarder the parent can plug in to observe
    /// `event.agent` / `event.pool` / `log` notifications. Called from
    /// the reader task; must not block.
    notification_sink: Option<Arc<dyn NotificationSink>>,
}

/// Trait the parent implements when it wants to relay child
/// notifications to its own event bus. Keeping it here (instead of
/// reusing `EventSink`) so the subprocess runtime can evolve
/// independently.
pub trait NotificationSink: Send + Sync {
    fn on_agent_event(&self, turn_id: &str, pool_id: &str, koi_id: &str, payload: &Value);
    fn on_pool_event(&self, turn_id: &str, payload: &Value);
    fn on_log(&self, turn_id: &str, params: &Value);
}

impl SubprocessSubagentRuntime {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            app_data_dir: None,
            extra_env: Vec::new(),
            force_kill_after: Duration::from_secs(5),
            inflight: Arc::new(Mutex::new(HashMap::new())),
            notification_sink: None,
        }
    }

    pub fn with_app_data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.app_data_dir = Some(dir.into());
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env.push((key.into(), value.into()));
        self
    }

    pub fn with_notification_sink(mut self, sink: Arc<dyn NotificationSink>) -> Self {
        self.notification_sink = Some(sink);
        self
    }

    pub fn with_force_kill_after(mut self, d: Duration) -> Self {
        self.force_kill_after = d;
        self
    }

    pub fn binary_path(&self) -> &Path {
        &self.binary
    }

    fn build_command(&self) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.arg("rpc")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = &self.app_data_dir {
            cmd.env("OPENPISCI_CONFIG_DIR", dir);
        }
        for (k, v) in &self.extra_env {
            cmd.env(k, v);
        }
        cmd
    }
}

struct InflightTurn {
    /// Write end of the child's stdin, wrapped so multiple callers
    /// (turn writer + cancel) can serialise their frames.
    stdin: Arc<Mutex<ChildStdin>>,
    /// Terminal outcome receiver — the reader task fulfils this.
    outcome_rx: Mutex<Option<oneshot::Receiver<anyhow::Result<KoiTurnOutcome>>>>,
    /// Handle to the driver task so `wait_koi_turn` can join it.
    driver: Mutex<Option<JoinHandle<()>>>,
    /// Shared cancel flag observed by the driver when the grace period
    /// elapses.
    hard_kill: Arc<tokio::sync::Notify>,
    /// Child handle — kept so we can kill it after the grace period.
    child: Arc<Mutex<Child>>,
}

#[async_trait]
impl SubagentRuntime for SubprocessSubagentRuntime {
    async fn spawn_koi_turn(&self, request: KoiTurnRequest) -> anyhow::Result<KoiTurnHandle> {
        let turn_id = Uuid::new_v4().to_string();
        let handle = KoiTurnHandle {
            turn_id: turn_id.clone(),
            pool_id: request.pool_id.clone(),
            koi_id: request.koi_id.clone(),
        };

        let mut cmd = self.build_command();
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn openpisci-headless: {e}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("openpisci-headless stdin unavailable after spawn"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("openpisci-headless stdout unavailable after spawn"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("openpisci-headless stderr unavailable after spawn"))?;

        let stdin = Arc::new(Mutex::new(stdin));
        let child = Arc::new(Mutex::new(child));
        let hard_kill = Arc::new(tokio::sync::Notify::new());
        let (outcome_tx, outcome_rx) = oneshot::channel();

        let driver = spawn_driver(
            handle.clone(),
            stdin.clone(),
            stdout,
            stderr,
            child.clone(),
            hard_kill.clone(),
            outcome_tx,
            self.notification_sink.clone(),
            request,
        );

        let entry = InflightTurn {
            stdin,
            outcome_rx: Mutex::new(Some(outcome_rx)),
            driver: Mutex::new(Some(driver)),
            hard_kill,
            child,
        };
        self.inflight.lock().await.insert(turn_id.clone(), entry);

        Ok(handle)
    }

    async fn cancel_koi_turn(&self, handle: &KoiTurnHandle) -> anyhow::Result<()> {
        let (stdin, hard_kill, child) = {
            let map = self.inflight.lock().await;
            let entry = map
                .get(&handle.turn_id)
                .ok_or_else(|| anyhow::anyhow!("no inflight turn with id '{}'", handle.turn_id))?;
            (
                entry.stdin.clone(),
                entry.hard_kill.clone(),
                entry.child.clone(),
            )
        };

        // Best-effort soft cancel — child may have already exited.
        let frame = json!({
            "jsonrpc": "2.0",
            "method": method::KOI_CANCEL,
            "params": { "turn_id": handle.turn_id },
        });
        let _ = write_frame(&stdin, &frame).await;

        // Schedule the hard kill. The driver task observes `hard_kill`
        // and fires `child.kill()` if the child hasn't exited by then.
        let grace = self.force_kill_after;
        let hard_kill = hard_kill.clone();
        let child_for_kill = child.clone();
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            hard_kill.notify_waiters();
            // Belt-and-braces: if the driver has already exited, kill
            // the child directly.
            let mut guard = child_for_kill.lock().await;
            let _ = guard.start_kill();
        });

        Ok(())
    }

    async fn wait_koi_turn(&self, handle: &KoiTurnHandle) -> anyhow::Result<KoiTurnOutcome> {
        let (rx, driver) = {
            let mut map = self.inflight.lock().await;
            let entry = map
                .get_mut(&handle.turn_id)
                .ok_or_else(|| anyhow::anyhow!("no inflight turn with id '{}'", handle.turn_id))?;
            let rx =
                entry.outcome_rx.lock().await.take().ok_or_else(|| {
                    anyhow::anyhow!("wait_koi_turn called twice for the same handle")
                })?;
            let driver = entry.driver.lock().await.take();
            (rx, driver)
        };

        let outcome = match rx.await {
            Ok(res) => res?,
            Err(_canceled) => anyhow::bail!("subagent driver dropped before reporting outcome"),
        };

        if let Some(h) = driver {
            // Driver already finished; joining now is instant.
            let _ = h.await;
        }
        self.inflight.lock().await.remove(&handle.turn_id);

        Ok(outcome)
    }
}

// ─── Driver task ──────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn spawn_driver(
    handle: KoiTurnHandle,
    stdin: Arc<Mutex<ChildStdin>>,
    stdout: ChildStdout,
    stderr: ChildStderr,
    child: Arc<Mutex<Child>>,
    hard_kill: Arc<tokio::sync::Notify>,
    outcome_tx: oneshot::Sender<anyhow::Result<KoiTurnOutcome>>,
    notification_sink: Option<Arc<dyn NotificationSink>>,
    request: KoiTurnRequest,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let outcome = drive_turn(
            handle.clone(),
            stdin,
            stdout,
            stderr,
            child,
            hard_kill,
            notification_sink,
            request,
        )
        .await;
        let _ = outcome_tx.send(outcome);
    })
}

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

#[allow(clippy::too_many_arguments)]
async fn drive_turn(
    handle: KoiTurnHandle,
    stdin: Arc<Mutex<ChildStdin>>,
    stdout: ChildStdout,
    stderr: ChildStderr,
    child: Arc<Mutex<Child>>,
    hard_kill: Arc<tokio::sync::Notify>,
    notification_sink: Option<Arc<dyn NotificationSink>>,
    request: KoiTurnRequest,
) -> anyhow::Result<KoiTurnOutcome> {
    // Issue the initial `koi.turn` request.
    let request_id = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let params = serde_json::to_value(&request)?;
    let frame = json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": method::KOI_TURN,
        "params": params,
    });
    write_frame(&stdin, &frame).await?;

    // Accumulate stderr so a crashing child carries actionable context.
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    {
        let buf = stderr_buf.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim_end();
                        if !trimmed.is_empty() {
                            tracing::debug!(target: "subagent::stderr", "{}", trimmed);
                            let mut g = buf.lock().await;
                            if g.len() < 16 * 1024 {
                                g.push_str(trimmed);
                                g.push('\n');
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    let mut reader = BufReader::new(stdout).lines();
    let mut response: Option<RpcFrame> = None;
    let mut killed_after_cancel = false;

    loop {
        tokio::select! {
            biased;
            _ = hard_kill.notified() => {
                tracing::warn!(target: "subagent", turn_id = %handle.turn_id, "hard kill fired after cancel grace");
                killed_after_cancel = true;
                let mut c = child.lock().await;
                let _ = c.start_kill();
                // Continue the loop to drain stdout / wait on the child.
            }
            line = reader.next_line() => {
                match line {
                    Ok(Some(raw)) => {
                        let trimmed = raw.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<RpcFrame>(trimmed) {
                            Ok(frame) => {
                                if frame.id == Some(request_id) {
                                    response = Some(frame);
                                    break;
                                }
                                if let (Some(method), Some(params)) = (frame.method.as_deref(), frame.params.as_ref()) {
                                    dispatch_notification(
                                        method,
                                        params,
                                        &handle,
                                        notification_sink.as_deref(),
                                    );
                                }
                            }
                            Err(err) => {
                                tracing::warn!(target: "subagent", turn_id = %handle.turn_id, "invalid JSON-RPC frame: {err}; line=`{trimmed}`");
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        tracing::warn!(target: "subagent", turn_id = %handle.turn_id, "stdout read error: {err}");
                        break;
                    }
                }
            }
        }
    }

    // Drain child — we always wait so stdio handles flush.
    let exit_status = {
        let mut c = child.lock().await;
        c.wait().await.ok()
    };

    let stderr_tail = stderr_buf.lock().await.trim().to_string();

    if killed_after_cancel {
        return Ok(KoiTurnOutcome {
            handle,
            exit_kind: KoiTurnExit::Cancelled,
            response_text: String::new(),
            error: Some("killed_after_cancel_grace".into()),
            exit_code: exit_status.and_then(|s| s.code()),
        });
    }

    if let Some(frame) = response {
        if let Some(err) = frame.error {
            return Ok(KoiTurnOutcome {
                handle,
                exit_kind: KoiTurnExit::Crashed,
                response_text: String::new(),
                error: Some(format!("{} (code {})", err.message, err.code)),
                exit_code: exit_status.and_then(|s| s.code()),
            });
        }
        let result = frame
            .result
            .ok_or_else(|| anyhow::anyhow!("response missing both `result` and `error`"))?;
        // Expect the child to already fill in `handle` — but we trust
        // our own handle to avoid drift.
        let mut outcome: KoiTurnOutcome = serde_json::from_value(result)
            .map_err(|e| anyhow::anyhow!("failed to parse KoiTurnOutcome: {e}"))?;
        outcome.handle = handle;
        if outcome.exit_code.is_none() {
            outcome.exit_code = exit_status.and_then(|s| s.code());
        }
        return Ok(outcome);
    }

    // Child closed stdout without a response → crash.
    let exit_code = exit_status.as_ref().and_then(|s| s.code()).unwrap_or(-1);
    let error = if stderr_tail.is_empty() {
        format!("child exited with code {exit_code} before replying")
    } else {
        format!("child exited with code {exit_code}; stderr tail: {stderr_tail}")
    };
    Ok(KoiTurnOutcome {
        handle,
        exit_kind: KoiTurnExit::Crashed,
        response_text: String::new(),
        error: Some(error),
        exit_code: Some(exit_code),
    })
}

fn dispatch_notification(
    method: &str,
    params: &Value,
    handle: &KoiTurnHandle,
    sink: Option<&dyn NotificationSink>,
) {
    match method {
        method::EVENT_AGENT => {
            if let Some(s) = sink {
                s.on_agent_event(&handle.turn_id, &handle.pool_id, &handle.koi_id, params);
            }
        }
        method::EVENT_POOL => {
            if let Some(s) = sink {
                s.on_pool_event(&handle.turn_id, params);
            }
        }
        method::LOG => {
            if let Some(s) = sink {
                s.on_log(&handle.turn_id, params);
            } else {
                dispatch_log_notification(params);
            }
        }
        other => {
            tracing::warn!(target: "subagent", turn_id = %handle.turn_id, "ignoring unknown notification `{other}`");
        }
    }
}

async fn write_frame(stdin: &Arc<Mutex<ChildStdin>>, value: &Value) -> anyhow::Result<()> {
    let mut text = serde_json::to_string(value)?;
    text.push('\n');
    let mut g = stdin.lock().await;
    g.write_all(text.as_bytes()).await?;
    g.flush().await?;
    Ok(())
}

// ─── StubSubagentRuntime (kernel unit tests) ──────────────────────────

/// In-memory runtime that synthesises outcomes from a user-supplied
/// closure. Tests pass `StubSubagentRuntime::new(|req| Ok(outcome))` to
/// avoid spawning real subprocesses.
pub struct StubSubagentRuntime {
    outcomes: Arc<Mutex<HashMap<String, StubSlot>>>,
    builder: Arc<dyn Fn(&KoiTurnRequest) -> StubOutcome + Send + Sync>,
}

/// What the stub should produce for a given request.
#[derive(Clone)]
pub enum StubOutcome {
    /// Immediate completion.
    Completed(String),
    /// Ignore cancel requests and return the given `exit_kind`.
    Explicit {
        exit_kind: KoiTurnExit,
        response_text: String,
        error: Option<String>,
    },
    /// Sleep for `delay` then complete. `wait_koi_turn` honours
    /// `cancel_koi_turn` by short-circuiting to
    /// `KoiTurnExit::Cancelled`.
    Delayed {
        delay: Duration,
        response_text: String,
    },
}

struct StubSlot {
    handle: KoiTurnHandle,
    outcome: StubOutcome,
    cancel_flag: Arc<tokio::sync::Notify>,
}

impl StubSubagentRuntime {
    pub fn new<F>(builder: F) -> Self
    where
        F: Fn(&KoiTurnRequest) -> StubOutcome + Send + Sync + 'static,
    {
        Self {
            outcomes: Arc::new(Mutex::new(HashMap::new())),
            builder: Arc::new(builder),
        }
    }

    pub fn always_complete(response_text: impl Into<String>) -> Self {
        let text = response_text.into();
        Self::new(move |_| StubOutcome::Completed(text.clone()))
    }

    pub fn always_fail(message: impl Into<String>) -> Self {
        let msg = message.into();
        Self::new(move |_| StubOutcome::Explicit {
            exit_kind: KoiTurnExit::Crashed,
            response_text: String::new(),
            error: Some(msg.clone()),
        })
    }
}

#[async_trait]
impl SubagentRuntime for StubSubagentRuntime {
    async fn spawn_koi_turn(&self, request: KoiTurnRequest) -> anyhow::Result<KoiTurnHandle> {
        let turn_id = Uuid::new_v4().to_string();
        let handle = KoiTurnHandle {
            turn_id: turn_id.clone(),
            pool_id: request.pool_id.clone(),
            koi_id: request.koi_id.clone(),
        };
        let outcome = (self.builder)(&request);
        let slot = StubSlot {
            handle: handle.clone(),
            outcome,
            cancel_flag: Arc::new(tokio::sync::Notify::new()),
        };
        self.outcomes.lock().await.insert(turn_id, slot);
        Ok(handle)
    }

    async fn cancel_koi_turn(&self, handle: &KoiTurnHandle) -> anyhow::Result<()> {
        if let Some(slot) = self.outcomes.lock().await.get(&handle.turn_id) {
            slot.cancel_flag.notify_waiters();
        }
        Ok(())
    }

    async fn wait_koi_turn(&self, handle: &KoiTurnHandle) -> anyhow::Result<KoiTurnOutcome> {
        // Keep the slot in the map so that a concurrent `cancel_koi_turn`
        // can still look up the notify handle. We remove it only after
        // the outcome is resolved.
        let (outcome, cancel_flag, stored_handle) = {
            let map = self.outcomes.lock().await;
            let slot = map.get(&handle.turn_id).ok_or_else(|| {
                anyhow::anyhow!("stub: no turn registered for {}", handle.turn_id)
            })?;
            (
                slot.outcome.clone(),
                slot.cancel_flag.clone(),
                slot.handle.clone(),
            )
        };
        let result = match outcome {
            StubOutcome::Completed(text) => KoiTurnOutcome {
                handle: stored_handle,
                exit_kind: KoiTurnExit::Completed,
                response_text: text,
                error: None,
                exit_code: Some(0),
            },
            StubOutcome::Explicit {
                exit_kind,
                response_text,
                error,
            } => KoiTurnOutcome {
                handle: stored_handle,
                exit_kind,
                response_text,
                error,
                exit_code: Some(0),
            },
            StubOutcome::Delayed {
                delay,
                response_text,
            } => {
                tokio::select! {
                    _ = tokio::time::sleep(delay) => KoiTurnOutcome {
                        handle: stored_handle,
                        exit_kind: KoiTurnExit::Completed,
                        response_text,
                        error: None,
                        exit_code: Some(0),
                    },
                    _ = cancel_flag.notified() => KoiTurnOutcome {
                        handle: stored_handle,
                        exit_kind: KoiTurnExit::Cancelled,
                        response_text: String::new(),
                        error: Some("stub: cancelled via notify".into()),
                        exit_code: Some(0),
                    },
                }
            }
        };
        self.outcomes.lock().await.remove(&handle.turn_id);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pisci_core::host::KoiTurnRequest;

    fn sample_request() -> KoiTurnRequest {
        KoiTurnRequest {
            pool_id: "pool-1".into(),
            koi_id: "koi-alpha".into(),
            session_id: "sess-1".into(),
            todo_id: Some("todo-1".into()),
            system_prompt: "sys".into(),
            user_prompt: "do the thing".into(),
            workspace: None,
            task_timeout_secs: None,
            extra_tool_profile: Vec::new(),
            extra_system_context: None,
        }
    }

    #[tokio::test]
    async fn stub_completed_flow() {
        let rt = StubSubagentRuntime::always_complete("hi");
        let handle = rt.spawn_koi_turn(sample_request()).await.unwrap();
        let outcome = rt.wait_koi_turn(&handle).await.unwrap();
        assert!(matches!(outcome.exit_kind, KoiTurnExit::Completed));
        assert_eq!(outcome.response_text, "hi");
    }

    #[tokio::test]
    async fn stub_cancel_short_circuits_delayed() {
        let rt = StubSubagentRuntime::new(|_| StubOutcome::Delayed {
            delay: Duration::from_secs(60),
            response_text: "never".into(),
        });
        let handle = rt.spawn_koi_turn(sample_request()).await.unwrap();

        let rt2 = &rt;
        let h2 = handle.clone();
        // Concurrently wait + cancel.
        let waiter = async move { rt2.wait_koi_turn(&h2).await.unwrap() };
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            rt.cancel_koi_turn(&handle).await.unwrap();
        };
        let (outcome, _) = tokio::join!(waiter, canceller);
        assert!(matches!(outcome.exit_kind, KoiTurnExit::Cancelled));
    }

    #[tokio::test]
    async fn stub_fail_reports_crashed() {
        let rt = StubSubagentRuntime::always_fail("boom");
        let handle = rt.spawn_koi_turn(sample_request()).await.unwrap();
        let outcome = rt.wait_koi_turn(&handle).await.unwrap();
        assert!(matches!(outcome.exit_kind, KoiTurnExit::Crashed));
        assert_eq!(outcome.error.as_deref(), Some("boom"));
    }

    #[test]
    fn rpc_frame_roundtrip_request() {
        let frame = RpcFrame {
            jsonrpc: "2.0".into(),
            id: Some(1),
            method: Some(method::KOI_TURN.into()),
            params: Some(serde_json::to_value(sample_request()).unwrap()),
            result: None,
            error: None,
        };
        let s = serde_json::to_string(&frame).unwrap();
        let back: RpcFrame = serde_json::from_str(&s).unwrap();
        assert_eq!(back.id, Some(1));
        assert_eq!(back.method.as_deref(), Some("koi.turn"));
    }

    #[test]
    fn rpc_frame_roundtrip_error_response() {
        let frame = RpcFrame {
            jsonrpc: "2.0".into(),
            id: Some(7),
            method: None,
            params: None,
            result: None,
            error: Some(RpcError {
                code: -32000,
                message: "turn failed".into(),
                data: None,
            }),
        };
        let s = serde_json::to_string(&frame).unwrap();
        let back: RpcFrame = serde_json::from_str(&s).unwrap();
        assert_eq!(back.error.as_ref().map(|e| e.code), Some(-32000));
    }
}
