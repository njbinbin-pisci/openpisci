//! `KoiRuntime` — thin desktop harness retained for the in-process
//! `call_koi` tool path.
//!
//! Historically this module owned the entire multi-agent coordinator
//! (execute_todo / handle_mention / resume_todo / replace_todo / watchdog
//! / activate_pending / …). Those responsibilities now live in
//! [`pisci_kernel::pool::coordinator`], and production desktop commands
//! reach the kernel via [`crate::pool::bridge`].
//!
//! What remains here is deliberately scoped to the in-process Koi path
//! Pisci still uses when it invokes a Koi as a tool call
//! (`call_koi` → [`execute_koi_agent`]), and the soft-fence retry that
//! [`reconcile_managed_pool_completion`] hands off to after such a run
//! exits with unreconciled claimed todos on the board.
//!
//! The module keeps two kinds of state:
//! * `ACTIVE_KOI_RUNS` / `IN_FLIGHT_SOFT_FENCE` — slot/recursion guards
//!   the `call_koi` tool uses to avoid running the same Koi twice.
//! * `KOI_SESSIONS` / `PENDING_KOI_NOTIFICATIONS` — notification channels
//!   so a currently-running in-process Koi can be nudged mid-turn.

use crate::pool::{KoiDefinition, KoiTodo};
use crate::store::Database;
use crate::tools::call_koi::event_bus::EventBus;
use once_cell::sync::Lazy;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, Mutex};

/// Global registry of running in-process Koi sessions. Maps
/// `koi_id::pool_session_id` → notification sender channel used to
/// inject `@mention` notifications into a busy Koi's AgentLoop.
pub static KOI_SESSIONS: Lazy<Mutex<HashMap<String, mpsc::Sender<String>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

static ACTIVE_KOI_RUNS: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

static PENDING_KOI_NOTIFICATIONS: Lazy<Mutex<HashMap<String, Vec<String>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Soft-fence in-flight set (keyed by `koi_id::pool_session_id`). When
/// [`reconcile_managed_pool_completion`] finds unreconciled todos after a
/// successful run, it synchronously re-engages the Koi once before
/// applying the hard fence (`needs_review` + `protocol_reminder`). The
/// retry itself ends with another call to the same function; this set
/// lets the nested call recognise itself and skip straight to the hard
/// fence rather than recursing.
static IN_FLIGHT_SOFT_FENCE: Lazy<Mutex<HashSet<String>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

fn soft_fence_key(koi_id: &str, pool_session_id: &str) -> String {
    format!("{}::{}", koi_id, pool_session_id)
}

fn managed_run_slot_key(koi_id: &str, pool_session_id: Option<&str>) -> String {
    format!("{}:{}", koi_id, pool_session_id.unwrap_or("default"))
}

async fn refresh_managed_koi_status(app: &AppHandle, db_arc: &Arc<Mutex<Database>>, koi_id: &str) {
    let prefix = format!("{}:", koi_id);
    let is_busy = {
        let active = ACTIVE_KOI_RUNS.lock().await;
        active.iter().any(|key| key.starts_with(&prefix))
    };
    let new_status = if is_busy { "busy" } else { "idle" };
    {
        let db = db_arc.lock().await;
        let _ = db.update_koi_status(koi_id, new_status);
    }
    let _ = app.emit(
        "koi_status_changed",
        json!({ "id": koi_id, "status": new_status }),
    );
}

#[derive(Clone)]
pub struct KoiRuntime {
    bus: Arc<dyn EventBus>,
}

// ─── Managed run-slot helpers (used by the `call_koi` tool) ────────────

pub(crate) async fn try_acquire_managed_run_slot(
    app: &AppHandle,
    db_arc: &Arc<Mutex<Database>>,
    koi_id: &str,
    pool_session_id: Option<&str>,
) -> bool {
    let key = managed_run_slot_key(koi_id, pool_session_id);
    let inserted = {
        let mut active = ACTIVE_KOI_RUNS.lock().await;
        active.insert(key)
    };
    if inserted {
        refresh_managed_koi_status(app, db_arc, koi_id).await;
    }
    inserted
}

pub(crate) async fn release_managed_run_slot(
    app: &AppHandle,
    db_arc: &Arc<Mutex<Database>>,
    koi_id: &str,
    pool_session_id: Option<&str>,
) {
    let key = managed_run_slot_key(koi_id, pool_session_id);
    {
        let mut active = ACTIVE_KOI_RUNS.lock().await;
        active.remove(&key);
    }
    refresh_managed_koi_status(app, db_arc, koi_id).await;

    // Auto-pickup: now that the Koi is idle, check for queued todos
    // in the same pool and dispatch the next one immediately.
    if let Some(psid) = pool_session_id {
        let psid_owned = psid.to_string();
        let koi_id_owned = koi_id.to_string();
        let app_cl = app.clone();
        let db_cl = db_arc.clone();
        tokio::spawn(async move {
            match crate::pool::bridge::activate_pending_todos_arc(&app_cl, db_cl, Some(&psid_owned))
                .await
            {
                Ok(n) if n > 0 => tracing::info!(
                    target: "pool::runtime",
                    koi_id = %koi_id_owned,
                    pool_id = %psid_owned,
                    activated = n,
                    "auto-pickup: dispatched queued todo(s) after Koi became idle"
                ),
                Ok(_) => {} // no pending todos
                Err(e) => tracing::warn!(
                    target: "pool::runtime",
                    koi_id = %koi_id_owned,
                    pool_id = %psid_owned,
                    "auto-pickup activate_pending_todos failed: {e}"
                ),
            }
        });
    }
}

pub(crate) async fn is_koi_run_slot_active(koi_id: &str, pool_session_id: Option<&str>) -> bool {
    let key = managed_run_slot_key(koi_id, pool_session_id);
    let active = ACTIVE_KOI_RUNS.lock().await;
    active.contains(&key)
}

/// Post-run reconciliation for managed (in-process) Koi runs triggered
/// by `call_koi`. Verbatim from the pre-Phase-4 implementation — the
/// soft-fence retry still uses [`KoiRuntime::run_soft_fence_reconcile_for`].
pub(crate) async fn reconcile_managed_pool_completion(
    app: &AppHandle,
    db_arc: &Arc<Mutex<Database>>,
    pool_session_id: &str,
    koi_id: &str,
    koi_name: &str,
    reply: &str,
    success: bool,
) {
    async fn fetch_claimed_todos(
        db_arc: &Arc<Mutex<Database>>,
        pool_session_id: &str,
        koi_id: &str,
    ) -> Result<Vec<KoiTodo>, String> {
        let db = db_arc.lock().await;
        db.list_active_todos_by_pool(pool_session_id)
            .map(|todos| {
                todos
                    .into_iter()
                    .filter(|todo| {
                        todo.status == "in_progress" && todo.claimed_by.as_deref() == Some(koi_id)
                    })
                    .collect::<Vec<_>>()
            })
            .map_err(|err| err.to_string())
    }

    let mut claimed_todos = match fetch_claimed_todos(db_arc, pool_session_id, koi_id).await {
        Ok(t) => t,
        Err(err) => {
            tracing::warn!(
                "managed runtime: failed to inspect claimed todos for koi='{}' pool='{}': {}",
                koi_name,
                pool_session_id,
                err
            );
            return;
        }
    };

    if claimed_todos.is_empty() {
        return;
    }

    let flight_key = soft_fence_key(koi_id, pool_session_id);
    let entered_soft_fence = if success {
        let mut flight = IN_FLIGHT_SOFT_FENCE.lock().await;
        if flight.contains(&flight_key) {
            false
        } else {
            flight.insert(flight_key.clone());
            true
        }
    } else {
        false
    };

    if entered_soft_fence {
        tracing::info!(
            "reconcile_managed: soft-fence entry koi='{}' pool='{}' pending={}",
            koi_name,
            pool_session_id,
            claimed_todos.len()
        );
        let koi_def_opt = {
            let db = db_arc.lock().await;
            db.resolve_koi_identifier(koi_id).ok().flatten()
        };
        if let Some(koi_def) = koi_def_opt {
            let runtime = KoiRuntime::from_tauri(app.clone(), db_arc.clone());
            runtime
                .run_soft_fence_reconcile_for(&koi_def, pool_session_id, &claimed_todos)
                .await;
        } else {
            tracing::warn!(
                "reconcile_managed: soft fence could not resolve koi_def for id='{}' pool='{}'; falling through",
                koi_id,
                pool_session_id
            );
        }
        {
            let mut flight = IN_FLIGHT_SOFT_FENCE.lock().await;
            flight.remove(&flight_key);
        }
        claimed_todos = match fetch_claimed_todos(db_arc, pool_session_id, koi_id).await {
            Ok(t) => t,
            Err(err) => {
                tracing::warn!(
                    "managed runtime: failed to re-inspect claimed todos post soft-fence for koi='{}' pool='{}': {}",
                    koi_name,
                    pool_session_id,
                    err
                );
                return;
            }
        };
        if claimed_todos.is_empty() {
            return;
        }
        tracing::info!(
            "reconcile_managed: soft fence did not fully reconcile koi='{}' pool='{}' remaining={}",
            koi_name,
            pool_session_id,
            claimed_todos.len()
        );
    }

    let reply_preview = if reply.chars().count() > 5000 {
        format!("{}...", reply.chars().take(5000).collect::<String>())
    } else {
        reply.trim().to_string()
    };

    for todo in claimed_todos {
        let mut emitted_messages = Vec::new();
        let todo_action = {
            let db = db_arc.lock().await;
            if success {
                if !reply_preview.is_empty() {
                    match db.insert_pool_message_ext(
                        pool_session_id,
                        koi_id,
                        &reply_preview,
                        "status_update",
                        &json!({
                            "todo_id": todo.id,
                            "auto_captured": true,
                            "managed_externally": true
                        })
                        .to_string(),
                        Some(&todo.id),
                        None,
                        Some("task_progress"),
                    ) {
                        Ok(msg) => {
                            emitted_messages.push(serde_json::to_value(&msg).unwrap_or_default())
                        }
                        Err(err) => tracing::warn!(
                            "managed runtime: failed to capture output for todo='{}': {}",
                            todo.id,
                            err
                        ),
                    }
                }

                let reminder = format!(
                    "[ProtocolReminder] {} finished executing on '{}' without calling complete_todo. The task output has been captured above if any. Todo status set to needs_review.",
                    koi_name,
                    todo.title
                );
                match db.insert_pool_message_ext(
                    pool_session_id,
                    "system",
                    &reminder,
                    "status_update",
                    &json!({
                        "todo_id": todo.id,
                        "protocol_reminder": "missing_complete_todo",
                        "managed_externally": true
                    })
                    .to_string(),
                    Some(&todo.id),
                    None,
                    Some("protocol_reminder"),
                ) {
                    Ok(msg) => {
                        emitted_messages.push(serde_json::to_value(&msg).unwrap_or_default())
                    }
                    Err(err) => tracing::warn!(
                        "managed runtime: failed to insert protocol reminder for todo='{}': {}",
                        todo.id,
                        err
                    ),
                }

                if let Err(err) = db.mark_koi_todo_needs_review(
                    &todo.id,
                    "Agent finished without calling complete_todo",
                ) {
                    tracing::warn!(
                        "managed runtime: failed to mark todo='{}' needs_review: {}",
                        todo.id,
                        err
                    );
                }
                "needs_review"
            } else {
                let failure_summary = if reply_preview.is_empty() {
                    format!(
                        "Koi '{}' failed without a structured error message.",
                        koi_name
                    )
                } else {
                    reply_preview.clone()
                };
                match db.insert_pool_message_ext(
                    pool_session_id,
                    koi_id,
                    &failure_summary,
                    "status_update",
                    &json!({
                        "todo_id": todo.id,
                        "success": false,
                        "managed_externally": true
                    })
                    .to_string(),
                    Some(&todo.id),
                    None,
                    Some("task_failed"),
                ) {
                    Ok(msg) => {
                        emitted_messages.push(serde_json::to_value(&msg).unwrap_or_default())
                    }
                    Err(err) => tracing::warn!(
                        "managed runtime: failed to insert failure message for todo='{}': {}",
                        todo.id,
                        err
                    ),
                }

                if let Err(err) = db.block_koi_todo(&todo.id, &failure_summary) {
                    tracing::warn!(
                        "managed runtime: failed to block todo='{}': {}",
                        todo.id,
                        err
                    );
                }
                "blocked"
            }
        };

        for payload in emitted_messages {
            let _ = app.emit(&format!("pool_message_{}", pool_session_id), payload);
        }
        let _ = app.emit(
            "koi_todo_updated",
            json!({
                "id": todo.id,
                "action": todo_action,
                "by": koi_id
            }),
        );
    }
}

impl KoiRuntime {
    /// Convenience constructor: build from a Tauri `AppHandle`.
    pub fn from_tauri(app: AppHandle, db: Arc<Mutex<Database>>) -> Self {
        let bus = Arc::new(crate::tools::call_koi::event_bus::TauriEventBus { app, db_ref: db });
        Self { bus }
    }

    fn db(&self) -> &Arc<Mutex<Database>> {
        self.bus.db()
    }

    fn try_get_app_handle(&self) -> Option<&AppHandle> {
        self.bus.app_handle()
    }

    fn default_pool_session_key(koi_id: &str, pool_session_id: Option<&str>) -> String {
        format!("{}:{}", koi_id, pool_session_id.unwrap_or("default"))
    }

    async fn system_default_timeout_secs(&self) -> u64 {
        if let Some(app) = self.try_get_app_handle() {
            let state = app.state::<crate::store::AppState>();
            let secs = state.settings.lock().await.koi_timeout_secs as u64;
            secs
        } else {
            600
        }
    }

    async fn resolve_task_timeout_secs(
        &self,
        koi_def: &KoiDefinition,
        pool_session_id: Option<&str>,
        todo_timeout_secs: Option<u32>,
    ) -> u64 {
        if let Some(timeout_secs) = todo_timeout_secs.filter(|value| *value > 0) {
            return timeout_secs as u64;
        }

        if let Some(psid) = pool_session_id {
            let db = self.db().lock().await;
            if let Ok(Some(pool)) = db.get_pool_session(psid) {
                if pool.task_timeout_secs > 0 {
                    return pool.task_timeout_secs as u64;
                }
            }
        }

        if koi_def.task_timeout_secs > 0 {
            return koi_def.task_timeout_secs as u64;
        }

        self.system_default_timeout_secs().await
    }

    /// Drive a single in-process Koi turn through the `call_koi` tool.
    /// Registers the Koi in [`KOI_SESSIONS`] so sibling
    /// `pool_chat` `@mention`s can be injected mid-turn, and drains
    /// any [`PENDING_KOI_NOTIFICATIONS`] that were queued while no
    /// receiver existed.
    async fn execute_koi_agent(
        &self,
        koi_def: &KoiDefinition,
        task: &str,
        pool_session_id: Option<&str>,
        workspace_override: Option<&str>,
        await_completion: bool,
    ) -> anyhow::Result<String> {
        use pisci_kernel::agent::tool::{Tool, ToolContext, ToolSettings};

        if let Some(app) = self.try_get_app_handle() {
            let state = app.state::<crate::store::AppState>();
            let (workspace_root, allow_outside, tool_settings_data) = {
                let settings = state.settings.lock().await;
                let ws = workspace_override
                    .map(String::from)
                    .unwrap_or_else(|| settings.workspace_root.clone());
                let allow_out = if workspace_override.is_some() {
                    false
                } else {
                    settings.allow_outside_workspace
                };
                (
                    ws,
                    allow_out,
                    Arc::new(ToolSettings::from_settings(&settings)),
                )
            };
            let loop_max_iterations = {
                let settings = state.settings.lock().await;
                if koi_def.max_iterations > 0 {
                    Some(koi_def.max_iterations)
                } else if settings.max_iterations > 0 {
                    Some(settings.max_iterations)
                } else {
                    None
                }
            };

            let session_key = Self::default_pool_session_key(&koi_def.id, pool_session_id);
            let (notif_tx, notif_rx) = mpsc::channel::<String>(32);
            {
                let mut sessions = KOI_SESSIONS.lock().await;
                sessions.insert(session_key.clone(), notif_tx.clone());
            }
            let pending_notifications = {
                let mut pending = PENDING_KOI_NOTIFICATIONS.lock().await;
                pending.remove(&session_key).unwrap_or_default()
            };
            for notification in pending_notifications {
                let _ = notif_tx.send(notification).await;
            }

            let koi_tool = crate::tools::call_koi::CallKoiTool {
                app: app.clone(),
                caller_koi_id: None,
                depth: 0,
                managed_externally: true,
                notification_rx: std::sync::Mutex::new(Some(notif_rx)),
                await_completion,
            };

            let cancel_key = format!(
                "koi_runtime_{}_{}",
                koi_def.id,
                pool_session_id.unwrap_or("default")
            );
            let cancel = Arc::new(AtomicBool::new(false));
            {
                let state = app.state::<crate::store::AppState>();
                let mut flags = state.cancel_flags.lock().await;
                flags.insert(cancel_key.clone(), cancel.clone());
            }

            let ctx = ToolContext {
                session_id: cancel_key.clone(),
                workspace_root: std::path::PathBuf::from(&workspace_root),
                bypass_permissions: false,
                settings: tool_settings_data,
                max_iterations: loop_max_iterations,
                memory_owner_id: koi_def.id.clone(),
                pool_session_id: pool_session_id.map(String::from),
                cancel: cancel.clone(),
            };

            let task_with_env = if workspace_root.trim().is_empty() {
                task.to_string()
            } else {
                let outside_note = if allow_outside {
                    " (you may also access files outside this directory when needed)"
                } else {
                    " (keep file operations within this directory)"
                };
                format!(
                    "[Environment] Workspace: `{}`{}\n\n{}",
                    workspace_root, outside_note, task
                )
            };

            let input = json!({
                "action": "call",
                "koi_id": koi_def.id,
                "task": task_with_env,
                "pool_session_id": pool_session_id,
            });

            let result = koi_tool.call(input, &ctx).await;

            {
                let state = app.state::<crate::store::AppState>();
                let mut flags = state.cancel_flags.lock().await;
                flags.remove(&cancel_key);
            }

            {
                let mut sessions = KOI_SESSIONS.lock().await;
                sessions.remove(&session_key);
            }

            let result = result?;
            if result.is_error {
                Err(anyhow::anyhow!("{}", result.content))
            } else {
                Ok(result.content)
            }
        } else {
            Ok(format!(
                "[TestMode] {} ({}) processed task: {}",
                koi_def.name, koi_def.icon, task
            ))
        }
    }

    /// Soft fence (one-shot). Re-engage a Koi whose `call_koi` run
    /// exited with unreconciled claimed todos on the board, giving it
    /// exactly one more turn to reconcile (`complete_todo` / `blocked`
    /// / `cancelled`) before the hard fence applies `needs_review`.
    ///
    /// Invoked exclusively by [`reconcile_managed_pool_completion`],
    /// which uses [`KoiRuntime::from_tauri`] to obtain a runtime.
    async fn run_soft_fence_reconcile_for(
        &self,
        koi_def: &KoiDefinition,
        pool_session_id: &str,
        pending: &[KoiTodo],
    ) {
        tracing::info!(
            "soft fence: ENTRY koi='{}' (id={}) pool='{}' pending={}",
            koi_def.name,
            koi_def.id,
            pool_session_id,
            pending.len()
        );
        if pending.is_empty() {
            return;
        }

        let pending_lines: Vec<String> = pending
            .iter()
            .map(|t| {
                format!(
                    "  - id=\"{}\" status=\"{}\" title=\"{}\"",
                    t.id, t.status, t.title
                )
            })
            .collect();

        {
            let db = self.db().lock().await;
            let notice = format!(
                "[SoftFence] {} exited with {} unreconciled claimed todo(s) on the board. \
                 Granting one more turn to reconcile (complete / blocked / cancelled).",
                koi_def.name,
                pending.len()
            );
            if let Ok(msg) = db.insert_pool_message_ext(
                pool_session_id,
                "system",
                &notice,
                "status_update",
                &json!({
                    "soft_fence": "reconcile_retry",
                    "koi_id": koi_def.id,
                    "pending_todo_ids": pending.iter().map(|t| &t.id).collect::<Vec<_>>(),
                })
                .to_string(),
                None,
                None,
                Some("soft_fence"),
            ) {
                self.bus.emit_event(
                    &format!("pool_message_{}", pool_session_id),
                    serde_json::to_value(&msg).unwrap_or_default(),
                );
            }
        }

        let task = format!(
            "You previously ran in pool \"{pool_id}\" and exited, but `pool_org` shows \
             the following claimed todo(s) of yours are still unreconciled on the board:\n\n\
             {pending_block}\n\n\
             Per the Run Shape in your system prompt, the run is NOT Done while a claimed \
             todo of yours sits in `todo` or `in_progress`. You are still in the Reconciling \
             phase for each todo above.\n\n\
             For EACH unreconciled todo, choose ONE option and execute it now. Do NOT default \
             to (a) if (b) or (c) is the truth \u{2014} the board should reflect reality.\n\n\
             (a) DONE \u{2014} the deliverable is real and is observable in pool_chat. Action: \
             `pool_org(action=\"complete_todo\", todo_id=\"<id>\", summary=\"<one-line summary>\")`. \
             If the deliverable is NOT yet visible in pool_chat, post it FIRST via \
             `pool_chat(action=\"send\")` (include file path(s) and a brief summary), then \
             call complete_todo. If a follow-up by another agent is needed, the same post must \
             include `[ProjectStatus] follow_up_needed` and an `@!mention` of the next \
             responsible party (identify them per the Coordination Protocol \u{2014} from \
             `org_spec`, the task description, or the @mention chain; never default to a \
             fixed role name).\n\n\
             (b) BLOCKED \u{2014} you genuinely cannot proceed (real blocker, missing upstream \
             evidence, ambiguous requirement that needs clarification, etc.). Action: \
             `pool_org(action=\"update_todo_status\", todo_id=\"<id>\", status=\"blocked\")`, \
             then post a `pool_chat(action=\"send\")` message naming the blocker so another \
             agent can act on it.\n\n\
             (c) CANCELLED \u{2014} the work turned out unnecessary, wrongly scoped, or \
             superseded. Action: `pool_org(action=\"cancel_todo\", todo_id=\"<id>\", \
             reason=\"<why>\")`.\n\n\
             After every todo above is in {{done, blocked, cancelled}}, you may stop. The \
             harness will check the board one last time after this turn; if anything is still \
             in `todo` or `in_progress`, it will be force-rewritten to `needs_review` with a \
             permanent `protocol_reminder` event under your name. Take this turn seriously.",
            pool_id = pool_session_id,
            pending_block = pending_lines.join("\n")
        );

        let timeout_secs = self
            .resolve_task_timeout_secs(koi_def, Some(pool_session_id), None)
            .await;

        match tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            self.execute_koi_agent(koi_def, &task, Some(pool_session_id), None, true),
        )
        .await
        {
            Ok(Ok(_)) => {
                tracing::info!(
                    "soft fence: koi='{}' pool='{}' completed reconcile retry ({} pending)",
                    koi_def.name,
                    pool_session_id,
                    pending.len()
                );
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    "soft fence: koi='{}' pool='{}' reconcile retry errored: {}",
                    koi_def.name,
                    pool_session_id,
                    err
                );
            }
            Err(_) => {
                tracing::warn!(
                    "soft fence: koi='{}' pool='{}' reconcile retry timed out after {}s",
                    koi_def.name,
                    pool_session_id,
                    timeout_secs
                );
            }
        }
    }
}
