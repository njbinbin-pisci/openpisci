//! Desktop in-process Koi runtime.
//!
//! The desktop product should not depend on `openpisci-headless` for normal
//! Koi collaboration. This runtime implements the kernel `SubagentRuntime`
//! contract by running Koi turns inside the already-running Tauri process,
//! while still reusing the shared headless/Koi scene path.

use async_trait::async_trait;
use pisci_core::host::{
    KoiTurnExit, KoiTurnHandle, KoiTurnOutcome, KoiTurnRequest, SubagentRuntime,
};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tauri::Manager;
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

use crate::commands::chat::{run_agent_headless, HeadlessRunOptions, SESSION_SOURCE_PISCI_POOL};
use crate::commands::config::scene::SceneKind;
use crate::headless_cli::HeadlessContextToggles;
use crate::store::AppState;

#[derive(Clone)]
pub struct DesktopInProcessSubagentRuntime {
    app: tauri::AppHandle,
    inflight: Arc<Mutex<HashMap<String, Arc<InflightTurn>>>>,
}

struct InflightTurn {
    session_id: String,
    cancel: Arc<AtomicBool>,
    outcome_rx: Mutex<Option<oneshot::Receiver<anyhow::Result<KoiTurnOutcome>>>>,
}

impl DesktopInProcessSubagentRuntime {
    pub fn new(app: tauri::AppHandle) -> Self {
        Self {
            app,
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl SubagentRuntime for DesktopInProcessSubagentRuntime {
    async fn spawn_koi_turn(&self, request: KoiTurnRequest) -> anyhow::Result<KoiTurnHandle> {
        let turn_id = Uuid::new_v4().to_string();
        let handle = KoiTurnHandle {
            turn_id: turn_id.clone(),
            pool_id: request.pool_id.clone(),
            koi_id: request.koi_id.clone(),
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let (outcome_tx, outcome_rx) = oneshot::channel();
        let turn = Arc::new(InflightTurn {
            session_id: request.session_id.clone(),
            cancel: cancel.clone(),
            outcome_rx: Mutex::new(Some(outcome_rx)),
        });
        self.inflight.lock().await.insert(turn_id, turn);

        let app = self.app.clone();
        let inflight = self.inflight.clone();
        let cleanup_turn_id = handle.turn_id.clone();
        let handle_for_task = handle.clone();
        tokio::spawn(async move {
            let outcome = run_in_process_koi_turn(app, handle_for_task, request, cancel).await;
            let _ = outcome_tx.send(outcome);
            cleanup_unclaimed_outcome(inflight, cleanup_turn_id).await;
        });

        Ok(handle)
    }

    async fn cancel_koi_turn(&self, handle: &KoiTurnHandle) -> anyhow::Result<()> {
        let turn = {
            let inflight = self.inflight.lock().await;
            inflight.get(&handle.turn_id).cloned()
        };
        let Some(turn) = turn else {
            return Ok(());
        };
        turn.cancel.store(true, Ordering::SeqCst);

        if let Some(state) = self.app.try_state::<AppState>() {
            let flags = state.cancel_flags.lock().await;
            if let Some(flag) = flags.get(&turn.session_id) {
                flag.store(true, Ordering::SeqCst);
            }
        }
        Ok(())
    }

    async fn wait_koi_turn(&self, handle: &KoiTurnHandle) -> anyhow::Result<KoiTurnOutcome> {
        let turn = {
            let inflight = self.inflight.lock().await;
            inflight
                .get(&handle.turn_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unknown Koi turn: {}", handle.turn_id))?
        };

        let rx =
            turn.outcome_rx.lock().await.take().ok_or_else(|| {
                anyhow::anyhow!("wait_koi_turn called twice for {}", handle.turn_id)
            })?;
        let outcome = rx.await.unwrap_or_else(|_| {
            Ok(KoiTurnOutcome {
                handle: handle.clone(),
                exit_kind: KoiTurnExit::Crashed,
                response_text: String::new(),
                error: Some("in-process Koi task dropped before completion".into()),
                exit_code: None,
            })
        })?;
        self.inflight.lock().await.remove(&handle.turn_id);
        Ok(outcome)
    }
}

async fn cleanup_unclaimed_outcome(
    inflight: Arc<Mutex<HashMap<String, Arc<InflightTurn>>>>,
    turn_id: String,
) {
    tokio::time::sleep(Duration::from_secs(60)).await;
    let turn = {
        let inflight = inflight.lock().await;
        inflight.get(&turn_id).cloned()
    };
    let Some(turn) = turn else {
        return;
    };
    if turn.outcome_rx.lock().await.is_some() {
        inflight.lock().await.remove(&turn_id);
    }
}

async fn run_in_process_koi_turn(
    app: tauri::AppHandle,
    handle: KoiTurnHandle,
    request: KoiTurnRequest,
    cancel: Arc<AtomicBool>,
) -> anyhow::Result<KoiTurnOutcome> {
    if cancel.load(Ordering::SeqCst) {
        return Ok(KoiTurnOutcome {
            handle,
            exit_kind: KoiTurnExit::Cancelled,
            response_text: String::new(),
            error: Some("cancelled before dispatch".into()),
            exit_code: Some(0),
        });
    }

    let state = app.state::<AppState>();
    let options = HeadlessRunOptions {
        pool_session_id: if request.pool_id.trim().is_empty() {
            None
        } else {
            Some(request.pool_id.clone())
        },
        extra_system_context: Some(
            request
                .extra_system_context
                .clone()
                .unwrap_or_else(|| request.system_prompt.clone()),
        ),
        session_title: None,
        session_source: Some(SESSION_SOURCE_PISCI_POOL.to_string()),
        scene_kind: Some(SceneKind::KoiTask),
        workspace_root_override: request.workspace.clone(),
        builtin_tool_overrides: HashMap::new(),
        context_toggles: HeadlessContextToggles::default(),
    };

    match run_agent_headless(
        &state,
        &request.session_id,
        &request.user_prompt,
        None,
        "internal",
        Some(options),
    )
    .await
    {
        Ok((response_text, _, _)) => {
            let was_cancelled = cancel.load(Ordering::SeqCst);
            Ok(KoiTurnOutcome {
                handle,
                exit_kind: if was_cancelled {
                    KoiTurnExit::Cancelled
                } else {
                    KoiTurnExit::Completed
                },
                response_text,
                error: if was_cancelled {
                    Some("cancelled".into())
                } else {
                    None
                },
                exit_code: Some(0),
            })
        }
        Err(error) => Ok(KoiTurnOutcome {
            handle,
            exit_kind: if cancel.load(Ordering::SeqCst) {
                KoiTurnExit::Cancelled
            } else {
                KoiTurnExit::Crashed
            },
            response_text: String::new(),
            error: Some(error),
            exit_code: Some(1),
        }),
    }
}
