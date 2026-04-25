//! Child-side JSON-RPC server for the subprocess subagent protocol.
//!
//! `openpisci-headless rpc` spawns this loop to let a parent
//! (`SubprocessSubagentRuntime` in `pisci-kernel::pool::subagent`) drive
//! Koi turns over stdin / stdout. See `pisci-kernel::pool::subagent`'s
//! module docs for the wire format (newline-delimited JSON-RPC 2.0).
//!
//! Scope of this first implementation:
//!
//! * `koi.turn`  — bridge into `pisci_kernel::headless::run_pisci_turn`
//!   and wrap the response as a `KoiTurnOutcome`. The child process is
//!   single-turn: one in-flight `koi.turn` at a time.
//! * `koi.cancel` — flip a shared cancel flag (best effort; real
//!   cancellation today is the hard-kill that `SubprocessSubagentRuntime`
//!   fires after the grace period).
//! * `shutdown` — reply `null` and exit 0.
//!
//! Unsupported methods surface as JSON-RPC `method not found` errors
//! rather than crashing the loop so the parent can drive protocol
//! discovery without killing the child.

use std::io::{BufRead as _, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pisci_core::host::{
    HeadlessCliMode, HeadlessCliRequest, KoiTurnExit, KoiTurnHandle, KoiTurnOutcome, KoiTurnRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::runner::run_pisci_once_with_stderr_events;

/// Entry point for the `openpisci-headless rpc` subcommand. Blocks on
/// stdin until the parent either sends `shutdown` or closes the pipe.
pub fn run_rpc_loop() -> Result<(), String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut line = String::new();
    let reader = stdin.lock();
    let mut reader = std::io::BufReader::new(reader);
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(()), // parent closed stdin
            Ok(_) => {}
            Err(e) => {
                return Err(format!("rpc stdin read failed: {e}"));
            }
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let frame: IncomingFrame = match serde_json::from_str(trimmed) {
            Ok(f) => f,
            Err(e) => {
                let err = json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32700, "message": format!("parse error: {e}")},
                });
                write_line(&mut stdout, &err);
                continue;
            }
        };

        match frame.method.as_deref().unwrap_or("") {
            "koi.turn" => {
                let params = frame.params.unwrap_or(Value::Null);
                let id = frame.id;
                cancel.store(false, Ordering::SeqCst);
                let outcome = handle_koi_turn(params, cancel.clone());
                if let Some(rid) = id {
                    let response = match outcome {
                        Ok(outcome) => json!({
                            "jsonrpc": "2.0",
                            "id": rid,
                            "result": outcome,
                        }),
                        Err(e) => json!({
                            "jsonrpc": "2.0",
                            "id": rid,
                            "error": {"code": -32000, "message": e},
                        }),
                    };
                    write_line(&mut stdout, &response);
                }
            }
            "koi.cancel" => {
                cancel.store(true, Ordering::SeqCst);
                // koi.cancel is sent as a notification by SubprocessSubagentRuntime
                // (no id), but we reply if an id is provided for robustness.
                if let Some(rid) = frame.id {
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": rid,
                        "result": Value::Null,
                    });
                    write_line(&mut stdout, &response);
                }
            }
            "shutdown" => {
                if let Some(rid) = frame.id {
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": rid,
                        "result": Value::Null,
                    });
                    write_line(&mut stdout, &response);
                }
                return Ok(());
            }
            other => {
                if let Some(rid) = frame.id {
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": rid,
                        "error": {
                            "code": -32601,
                            "message": format!("method not found: {other}"),
                        },
                    });
                    write_line(&mut stdout, &response);
                }
            }
        }
    }
}

fn handle_koi_turn(params: Value, cancel: Arc<AtomicBool>) -> Result<KoiTurnOutcome, String> {
    let request: KoiTurnRequest = serde_json::from_value(params)
        .map_err(|e| format!("invalid KoiTurnRequest params: {e}"))?;

    let handle = KoiTurnHandle {
        turn_id: String::new(), // filled in by parent from its own map
        pool_id: request.pool_id.clone(),
        koi_id: request.koi_id.clone(),
    };

    // Map the Koi turn onto a pisci-mode headless request. This is the
    // MVP bridge: the child reuses `run_pisci_turn` so we immediately get
    // event streaming + the full neutral tool set. Richer semantics
    // (bespoke Koi system prompt, org-spec injection, etc.) will come
    // once `pisci_kernel::pool::coordinator::execute_todo_turn` is fully
    // bridged through here.
    let cli_request = HeadlessCliRequest {
        prompt: request.user_prompt.clone(),
        workspace: request.workspace.clone(),
        mode: HeadlessCliMode::Pisci,
        session_id: Some(request.session_id.clone()),
        session_title: None,
        channel: None,
        config_dir: None,
        pool_id: Some(request.pool_id.clone()),
        pool_name: None,
        pool_size: None,
        koi_ids: vec![request.koi_id.clone()],
        task_timeout_secs: request.task_timeout_secs,
        wait_for_completion: false,
        wait_timeout_secs: None,
        extra_system_context: Some(
            request
                .extra_system_context
                .clone()
                .unwrap_or_else(|| request.system_prompt.clone()),
        ),
        context_toggles: Default::default(),
        output: None,
    };

    // We honour cancel via a best-effort poll: if the flag flipped before
    // we even got to dispatch, short-circuit to Cancelled. The richer
    // per-iteration cancel gets wired once run_pisci_turn takes an
    // explicit cancel token.
    if cancel.load(Ordering::SeqCst) {
        return Ok(KoiTurnOutcome {
            handle,
            exit_kind: KoiTurnExit::Cancelled,
            response_text: String::new(),
            error: Some("cancelled before dispatch".into()),
            exit_code: Some(0),
        });
    }

    match run_pisci_once_with_stderr_events(cli_request) {
        Ok(resp) => Ok(KoiTurnOutcome {
            handle,
            exit_kind: if resp.ok {
                KoiTurnExit::Completed
            } else {
                KoiTurnExit::Crashed
            },
            response_text: resp.response_text,
            error: None,
            exit_code: Some(0),
        }),
        Err(e) => Ok(KoiTurnOutcome {
            handle,
            exit_kind: KoiTurnExit::Crashed,
            response_text: String::new(),
            error: Some(e),
            exit_code: Some(1),
        }),
    }
}

fn write_line<W: Write>(out: &mut W, value: &Value) {
    let mut text = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(_) => return,
    };
    text.push('\n');
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IncomingFrame {
    #[serde(default)]
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
}
