/// KoiRuntime — centralized dispatch layer for Koi agent collaboration.
///
/// Responsibilities:
/// - Activate Koi agents based on task assignments, @mentions, and scheduled events
/// - Maintain Koi busy/idle/offline state
/// - Translate pool_assign_task, @mention, and HostAgent routing into real Koi execution
/// - Post events to pool_messages and update koi_todos
///
/// This module does NOT contain UI logic; it is purely a backend orchestrator.
/// It depends on the `EventBus` trait (not on tauri::AppHandle directly),
/// so it can run in both the real app and headless test environments.
use crate::koi::event_bus::EventBus;
use crate::koi::{KoiDefinition, KoiTodo};
use crate::store::Database;
use chrono::Utc;
use once_cell::sync::Lazy;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, Mutex};

/// Global registry of running Koi sessions.
/// Maps koi_id -> notification sender channel.
/// Used to inject @mention notifications into a busy Koi's AgentLoop.
pub static KOI_SESSIONS: Lazy<Mutex<HashMap<String, mpsc::Sender<String>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static ACTIVE_KOI_RUNS: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));
static PENDING_KOI_NOTIFICATIONS: Lazy<Mutex<HashMap<String, Vec<String>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
/// Soft-fence in-flight set (keyed by `koi_id::pool_session_id`).
/// When `reconcile_managed_pool_completion` finds unreconciled todos on a
/// successful run, it synchronously re-engages the Koi once before applying
/// the hard fence (needs_review + protocol_reminder). The retry itself ends
/// with another call to `reconcile_managed_pool_completion`; this set lets the
/// nested call recognize itself and apply the hard fence immediately rather
/// than recursing into another soft-fence retry.
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

struct KoiRunSlotGuard {
    runtime: KoiRuntime,
    koi_id: String,
    pool_session_id: Option<String>,
    active: bool,
}

impl KoiRunSlotGuard {
    async fn acquire(
        runtime: &KoiRuntime,
        koi_id: &str,
        pool_session_id: Option<&str>,
    ) -> Option<Self> {
        if !runtime.acquire_koi_run_slot(koi_id, pool_session_id).await {
            return None;
        }
        runtime.refresh_koi_status(koi_id).await;
        Some(Self {
            runtime: runtime.clone(),
            koi_id: koi_id.to_string(),
            pool_session_id: pool_session_id.map(str::to_string),
            active: true,
        })
    }

    async fn release(mut self) {
        if !self.active {
            return;
        }
        self.runtime
            .release_koi_run_slot(&self.koi_id, self.pool_session_id.as_deref())
            .await;
        self.active = false;
    }
}

impl Drop for KoiRunSlotGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let runtime = self.runtime.clone();
        let koi_id = self.koi_id.clone();
        let pool_session_id = self.pool_session_id.clone();
        tokio::spawn(async move {
            runtime
                .release_koi_run_slot(&koi_id, pool_session_id.as_deref())
                .await;
        });
    }
}

/// Result of a Koi task execution
pub struct KoiExecResult {
    pub success: bool,
    pub reply: String,
    pub result_message_id: Option<i64>,
}

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
}

pub(crate) async fn is_koi_run_slot_active(koi_id: &str, pool_session_id: Option<&str>) -> bool {
    let key = managed_run_slot_key(koi_id, pool_session_id);
    let active = ACTIVE_KOI_RUNS.lock().await;
    active.contains(&key)
}

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

    // Soft fence: successful runs get ONE synchronous retry to reconcile their
    // own claimed todos before the hard fence below applies needs_review. The
    // retry itself finishes with another call to this function; we use an
    // in-flight set to detect that recursive call and let it fall through to
    // the hard fence without retrying again.
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
        // Re-inspect the board. If the retry's agent honestly reconciled its
        // todos (or the retry's own nested reconcile already applied the hard
        // fence on them), there is nothing left to do. Otherwise (e.g. the
        // retry timed out before its nested reconcile could run), fall
        // through to the hard fence below on the still-unreconciled subset.
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
    fn koi_pool_session_key(koi_id: &str, pool_session_id: &str) -> String {
        format!("{}:{}", koi_id, pool_session_id)
    }

    fn default_pool_session_key(koi_id: &str, pool_session_id: Option<&str>) -> String {
        Self::koi_pool_session_key(koi_id, pool_session_id.unwrap_or("default"))
    }

    fn build_pool_notification(koi_name: &str, sender_name: &str, content: &str) -> String {
        format!(
            "[Pool Notification] @{} from {} (in pool chat):\n{}\n\n\
             You can use pool_chat to respond. \
             Not every mention is immediate work: future plans, acknowledgements, and FYI messages may mention you without requiring action now. \
             If this is actionable work, create a todo for yourself via pool_org so it is tracked. \
             If you cannot handle it right now, still create the todo — it will be picked up on your next activation.",
            koi_name, sender_name, content
        )
    }

    async fn acquire_koi_run_slot(&self, koi_id: &str, pool_session_id: Option<&str>) -> bool {
        let key = Self::default_pool_session_key(koi_id, pool_session_id);
        let mut active = ACTIVE_KOI_RUNS.lock().await;
        active.insert(key)
    }

    async fn is_koi_run_active(&self, koi_id: &str, pool_session_id: Option<&str>) -> bool {
        let key = Self::default_pool_session_key(koi_id, pool_session_id);
        let active = ACTIVE_KOI_RUNS.lock().await;
        active.contains(&key)
    }

    async fn queue_pending_notification(&self, session_key: &str, notification: String) {
        let mut pending = PENDING_KOI_NOTIFICATIONS.lock().await;
        pending
            .entry(session_key.to_string())
            .or_default()
            .push(notification);
    }

    async fn release_koi_run_slot(&self, koi_id: &str, pool_session_id: Option<&str>) {
        let key = Self::default_pool_session_key(koi_id, pool_session_id);
        {
            let mut active = ACTIVE_KOI_RUNS.lock().await;
            active.remove(&key);
        }
        self.refresh_koi_status(koi_id).await;
    }

    async fn refresh_koi_status(&self, koi_id: &str) {
        let prefix = format!("{}:", koi_id);
        let is_busy = {
            let active = ACTIVE_KOI_RUNS.lock().await;
            active.iter().any(|key| key.starts_with(&prefix))
        };
        self.set_koi_status(koi_id, if is_busy { "busy" } else { "idle" })
            .await;
    }

    pub fn new(bus: Arc<dyn EventBus>) -> Self {
        Self { bus }
    }

    /// Convenience: build from a Tauri AppHandle (backward compat for commands).
    pub fn from_tauri(app: tauri::AppHandle, db: Arc<Mutex<Database>>) -> Self {
        let bus = Arc::new(crate::koi::event_bus::TauriEventBus { app, db_ref: db });
        Self { bus }
    }

    fn db(&self) -> &Arc<Mutex<Database>> {
        self.bus.db()
    }

    fn ensure_pool_allows_runtime_work(
        pool: &crate::koi::PoolSession,
        action: &str,
    ) -> anyhow::Result<()> {
        if pool.status == "active" {
            return Ok(());
        }
        Err(anyhow::anyhow!(
            "Pool '{}' is {} and cannot {} until it is resumed",
            pool.name,
            pool.status,
            action
        ))
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

    /// Load the org spec for a pool session (if any).
    pub async fn load_org_spec(&self, pool_session_id: Option<&str>) -> String {
        if let Some(psid) = pool_session_id {
            let db = self.db().lock().await;
            if let Ok(Some(session)) = db.get_pool_session(psid) {
                if !session.org_spec.is_empty() {
                    return format!("\n\n## Project Organization\n{}", session.org_spec);
                }
            }
        }
        String::new()
    }

    /// Create a todo and post assignment message to pool. Does NOT execute.
    /// Returns (todo, assign_msg_id) so the caller can trigger execution separately.
    /// Only Pisci/user should call this directly (they have authority to assign tasks).
    pub async fn assign_task(
        &self,
        koi_id: &str,
        task: &str,
        assigned_by: &str,
        pool_session_id: Option<&str>,
        priority: &str,
        task_timeout_secs: Option<u32>,
    ) -> anyhow::Result<(crate::koi::KoiTodo, Option<i64>)> {
        let (koi_def, pool_session_id) = {
            let db = self.db().lock().await;
            let koi_def = db
                .resolve_koi_identifier(koi_id)?
                .ok_or_else(|| anyhow::anyhow!("Koi '{}' not found", koi_id))?;
            let pool_session_id = match pool_session_id {
                Some(value) => Some({
                    let pool = db
                        .resolve_pool_session_identifier(value)?
                        .ok_or_else(|| anyhow::anyhow!("Pool '{}' not found", value))?;
                    Self::ensure_pool_allows_runtime_work(&pool, "accept new task assignments")?;
                    pool.id
                }),
                None => None,
            };
            (koi_def, pool_session_id)
        };

        let todo = {
            let db = self.db().lock().await;
            db.create_koi_todo(
                &koi_def.id,
                task,
                "",
                priority,
                assigned_by,
                pool_session_id.as_deref(),
                assigned_by,
                None,
                task_timeout_secs.unwrap_or(0),
            )?
        };

        let assign_msg_id = if let Some(psid) = pool_session_id.as_deref() {
            let db = self.db().lock().await;
            let msg = db.insert_pool_message_ext(
                psid,
                assigned_by,
                &format!("@{} {}", koi_def.name, task),
                "task_assign",
                &json!({ "koi_id": &koi_def.id, "priority": priority, "timeout_secs": task_timeout_secs }).to_string(),
                Some(&todo.id),
                None,
                Some("task_assigned"),
            )?;
            self.bus.emit_event(
                &format!("pool_message_{}", psid),
                serde_json::to_value(&msg).unwrap_or_default(),
            );
            Some(msg.id)
        } else {
            None
        };

        Ok((todo, assign_msg_id))
    }

    /// Execute an already-created todo: claim it, run the Koi agent, post results.
    pub async fn execute_todo(
        &self,
        koi_id: &str,
        todo: &crate::koi::KoiTodo,
        assign_msg_id: Option<i64>,
        pool_session_id: Option<&str>,
    ) -> anyhow::Result<KoiExecResult> {
        let (koi_def, canonical_pool_session_id) = {
            let db = self.db().lock().await;
            let koi_def = match db.resolve_koi_identifier(koi_id)? {
                Some(koi) => koi,
                None => db
                    .resolve_koi_identifier(&todo.owner_id)?
                    .ok_or_else(|| anyhow::anyhow!("Koi '{}' not found", koi_id))?,
            };
            let pool_value = pool_session_id.or(todo.pool_session_id.as_deref());
            let pool_session_id = match pool_value {
                Some(value) => Some({
                    let pool = db
                        .resolve_pool_session_identifier(value)?
                        .ok_or_else(|| anyhow::anyhow!("Pool '{}' not found", value))?;
                    Self::ensure_pool_allows_runtime_work(&pool, "run Koi work")?;
                    pool.id
                }),
                None => None,
            };
            (koi_def, pool_session_id)
        };
        let koi_id = koi_def.id.as_str();
        let task = todo.title.clone();
        let run_guard =
            KoiRunSlotGuard::acquire(self, koi_id, canonical_pool_session_id.as_deref())
                .await
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Koi '{}' already has an active run in pool '{}'",
                        koi_def.name,
                        canonical_pool_session_id.as_deref().unwrap_or("default")
                    )
                })?;

        // Claim the todo
        {
            let db = self.db().lock().await;
            db.claim_koi_todo(&todo.id, koi_id)?;
        }

        // Post "claimed" event to pool
        if let Some(psid) = canonical_pool_session_id.as_deref() {
            let db = self.db().lock().await;
            let msg = db.insert_pool_message_ext(
                psid,
                koi_id,
                &format!("{} 接受了任务: {}", koi_def.name, task),
                "task_claimed",
                "{}",
                Some(&todo.id),
                assign_msg_id,
                Some("task_claimed"),
            )?;
            self.bus.emit_event(
                &format!("pool_message_{}", psid),
                serde_json::to_value(&msg).unwrap_or_default(),
            );
        }

        // Set up Git worktree if the pool has a project_dir
        let worktree_path = if let Some(psid) = canonical_pool_session_id.as_deref() {
            let db = self.db().lock().await;
            db.get_pool_session(psid)
                .ok()
                .flatten()
                .and_then(|s| s.project_dir)
                .and_then(|dir| self.setup_worktree(&dir, &koi_def.name, &todo.id))
        } else {
            None
        };

        let todo_id_short = &todo.id[..8.min(todo.id.len())];
        let task_with_meta = include_str!("../../prompts/koi_execute_todo.txt")
            .replace("{task}", &task)
            .replace("{name}", &koi_def.name)
            .replace("{todo_id}", todo_id_short);

        let koi_timeout_secs = self
            .resolve_task_timeout_secs(
                &koi_def,
                canonical_pool_session_id.as_deref(),
                Some(todo.task_timeout_secs),
            )
            .await;
        let exec_result = match tokio::time::timeout(
            std::time::Duration::from_secs(koi_timeout_secs),
            self.execute_koi_agent(
                &koi_def,
                &task_with_meta,
                canonical_pool_session_id.as_deref(),
                worktree_path.as_deref(),
                false,
            ),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!(
                "Koi '{}' timed out after {} seconds on task: {}",
                koi_def.name,
                koi_timeout_secs,
                task
            )),
        };

        let (run_success, raw_reply) = match &exec_result {
            Ok(reply) => (true, reply.clone()),
            Err(e) => (false, format!("Execution error: {}", e)),
        };

        // Determine completion status.
        //  - Agent explicitly called complete_todo → already marked "done" in DB
        //  - Agent returned without error but didn't call complete_todo → needs_review
        //  - Agent returned with error → blocked
        let explicitly_completed = if let Some(psid) = canonical_pool_session_id.as_deref() {
            let db = self.db().lock().await;
            if run_success && raw_reply.trim().is_empty() {
                db.get_latest_unlinked_result_message_id(psid, koi_id)
                    .unwrap_or_default()
                    .is_some()
            } else {
                false
            }
        } else {
            false
        };
        let test_mode_completed = run_success
            && canonical_pool_session_id.is_none()
            && raw_reply.starts_with("[TestMode]");
        let completion_recorded = explicitly_completed || test_mode_completed;

        // Check if todo was already marked done by the agent via complete_todo tool.
        // Note: execute_todo's post-agent reminder path at L761 is analogous to
        // reconcile_managed_pool_completion's path. The soft fence for this
        // execute_todo path is not yet wired; the trial's primary worker
        // dispatch goes through call_koi background + reconcile_managed, which
        // is where the soft fence actually fires.
        let todo_already_done = {
            let db = self.db().lock().await;
            db.get_koi_todo(&todo.id)
                .ok()
                .flatten()
                .map(|t| t.status == "done")
                .unwrap_or(false)
        };

        let result_msg_id = if let Some(psid) = canonical_pool_session_id.as_deref() {
            let db = self.db().lock().await;

            if run_success && raw_reply.trim().is_empty() && explicitly_completed {
                let existing_id = db
                    .get_latest_unlinked_result_message_id(psid, koi_id)
                    .unwrap_or_default();
                if let Some(msg_id) = existing_id {
                    let _ = db.link_pool_message_to_todo(msg_id, &todo.id);
                    if let Ok(Some(msg)) = db.get_pool_message_by_id(msg_id) {
                        drop(db);
                        self.bus.emit_event(
                            &format!("pool_message_{}", psid),
                            serde_json::to_value(&msg).unwrap_or_default(),
                        );
                    } else {
                        drop(db);
                    }
                    Some(msg_id)
                } else {
                    drop(db);
                    None
                }
            } else if !run_success {
                let summary = if raw_reply.chars().count() > 5000 {
                    raw_reply.chars().take(5000).collect::<String>()
                } else {
                    raw_reply.clone()
                };
                let msg = db.insert_pool_message_ext(
                    psid,
                    koi_id,
                    &summary,
                    "status_update",
                    &json!({
                        "todo_id": todo.id,
                        "success": false,
                    })
                    .to_string(),
                    Some(&todo.id),
                    assign_msg_id,
                    Some("task_failed"),
                )?;
                self.bus.emit_event(
                    &format!("pool_message_{}", psid),
                    serde_json::to_value(&msg).unwrap_or_default(),
                );
                drop(db);
                Some(msg.id)
            } else if !todo_already_done && !completion_recorded {
                // Agent finished without error but didn't call complete_todo.
                // 1) Post the Koi's actual output so its work is visible in the pool.
                let koi_output = if raw_reply.chars().count() > 5000 {
                    format!("{}...", raw_reply.chars().take(5000).collect::<String>())
                } else {
                    raw_reply.clone()
                };
                if !koi_output.trim().is_empty() {
                    let output_msg = db.insert_pool_message_ext(
                        psid,
                        koi_id,
                        &koi_output,
                        "status_update",
                        &json!({
                            "todo_id": todo.id,
                            "auto_captured": true
                        })
                        .to_string(),
                        Some(&todo.id),
                        assign_msg_id,
                        Some("task_progress"),
                    )?;
                    self.bus.emit_event(
                        &format!("pool_message_{}", psid),
                        serde_json::to_value(&output_msg).unwrap_or_default(),
                    );
                }
                // 2) Post a system protocol reminder so Pisci knows to review.
                let reminder = format!(
                    "[ProtocolReminder] {} finished executing on '{}' without calling complete_todo. \
                     The task output has been captured above. Todo status set to needs_review.",
                    koi_def.name,
                    todo.title,
                );
                let msg = db.insert_pool_message_ext(
                    psid,
                    "system",
                    &reminder,
                    "status_update",
                    &json!({
                        "todo_id": todo.id,
                        "protocol_reminder": "missing_complete_todo"
                    })
                    .to_string(),
                    Some(&todo.id),
                    assign_msg_id,
                    Some("protocol_reminder"),
                )?;
                self.bus.emit_event(
                    &format!("pool_message_{}", psid),
                    serde_json::to_value(&msg).unwrap_or_default(),
                );
                drop(db);
                Some(msg.id)
            } else {
                drop(db);
                None
            }
        } else {
            None
        };

        let success = run_success;

        // Update todo status based on outcome
        if !todo_already_done {
            let db = self.db().lock().await;
            if !run_success {
                db.block_koi_todo(&todo.id, &raw_reply)?;
            } else if test_mode_completed {
                db.complete_koi_todo(&todo.id, result_msg_id)?;
            } else if !completion_recorded {
                db.mark_koi_todo_needs_review(
                    &todo.id,
                    "Agent finished without calling complete_todo",
                )?;
            }
        }
        let todo_action = if todo_already_done || completion_recorded {
            "completed"
        } else if !run_success {
            "blocked"
        } else {
            "needs_review"
        };
        self.bus.emit_event(
            "koi_todo_updated",
            json!({
                "id": todo.id, "action": todo_action
            }),
        );

        // Clean up Git worktree if one was created
        if let Some(ref wt) = worktree_path {
            self.cleanup_worktree(wt, &koi_def.name, &task);
        }
        run_guard.release().await;
        Ok(KoiExecResult {
            success,
            reply: raw_reply,
            result_message_id: result_msg_id,
        })
    }

    fn todo_source_type(actor_id: &str) -> &'static str {
        match actor_id {
            "pisci" => "pisci",
            "user" => "user",
            "system" => "system",
            _ => "koi",
        }
    }

    pub async fn resume_todo(&self, todo_id: &str, triggered_by: &str) -> anyhow::Result<()> {
        let (todo, owner, pool_session_id) = {
            let db = self.db().lock().await;
            let todo = db
                .get_koi_todo(todo_id)?
                .ok_or_else(|| anyhow::anyhow!("Todo '{}' not found", todo_id))?;
            if !matches!(todo.status.as_str(), "blocked" | "needs_review") {
                return Err(anyhow::anyhow!(
                    "Todo '{}' is '{}' and cannot be resumed. Only blocked/needs_review todos are resumable.",
                    todo.id,
                    todo.status
                ));
            }
            let owner = db
                .resolve_koi_identifier(&todo.owner_id)?
                .ok_or_else(|| anyhow::anyhow!("Owner '{}' not found", todo.owner_id))?;
            let pool_session_id = match todo.pool_session_id.as_deref() {
                Some(value) => Some({
                    let pool = db
                        .resolve_pool_session_identifier(value)?
                        .ok_or_else(|| anyhow::anyhow!("Pool '{}' not found", value))?;
                    Self::ensure_pool_allows_runtime_work(&pool, "resume Koi work")?;
                    pool.id
                }),
                None => None,
            };
            (todo, owner, pool_session_id)
        };

        if self
            .is_koi_run_active(&owner.id, pool_session_id.as_deref())
            .await
        {
            return Err(anyhow::anyhow!(
                "Koi '{}' already has an active run for this task context.",
                owner.name
            ));
        }

        {
            let db = self.db().lock().await;
            db.resume_koi_todo(&todo.id, &owner.id)?;
            if let Some(psid) = pool_session_id.as_deref() {
                let msg = db.insert_pool_message_ext(
                    psid,
                    triggered_by,
                    &format!(
                        "[Task Resumed] {} resumed '{}' for {}.",
                        triggered_by, todo.title, owner.name
                    ),
                    "status_update",
                    &json!({
                        "todo_id": todo.id,
                        "resumed_by": triggered_by,
                        "owner_id": owner.id,
                    })
                    .to_string(),
                    Some(&todo.id),
                    None,
                    Some("task_resumed"),
                )?;
                self.bus.emit_event(
                    &format!("pool_message_{}", psid),
                    serde_json::to_value(&msg).unwrap_or_default(),
                );
            }
        }

        self.bus.emit_event(
            "koi_todo_updated",
            json!({
                "id": todo.id,
                "action": "resumed",
                "by": triggered_by,
            }),
        );

        let mut todo_for_run = todo.clone();
        todo_for_run.status = "in_progress".into();
        todo_for_run.claimed_by = Some(owner.id.clone());
        todo_for_run.claimed_at = Some(Utc::now());
        todo_for_run.blocked_reason = None;
        todo_for_run.updated_at = Utc::now();

        let runtime = self.clone();
        let owner_id = owner.id.clone();
        let todo_id_owned = todo.id.clone();
        tokio::spawn(async move {
            if let Err(error) = runtime
                .execute_todo(&owner_id, &todo_for_run, None, pool_session_id.as_deref())
                .await
            {
                tracing::warn!(
                    "resume_todo execution failed for '{}': {}",
                    todo_id_owned,
                    error
                );
            }
        });

        Ok(())
    }

    pub async fn replace_todo(
        &self,
        todo_id: &str,
        new_owner_id: &str,
        task: &str,
        reason: &str,
        triggered_by: &str,
        task_timeout_secs: Option<u32>,
    ) -> anyhow::Result<KoiTodo> {
        let task = task.trim();
        let reason = reason.trim();
        if task.is_empty() {
            return Err(anyhow::anyhow!("Replacement task cannot be empty."));
        }
        if reason.is_empty() {
            return Err(anyhow::anyhow!("Replacement reason cannot be empty."));
        }

        let (original, new_owner, pool_session_id) = {
            let db = self.db().lock().await;
            let original = db
                .get_koi_todo(todo_id)?
                .ok_or_else(|| anyhow::anyhow!("Todo '{}' not found", todo_id))?;
            if matches!(original.status.as_str(), "done" | "cancelled") {
                return Err(anyhow::anyhow!(
                    "Todo '{}' is '{}' and cannot be replaced.",
                    original.id,
                    original.status
                ));
            }
            let new_owner = db
                .resolve_koi_identifier(new_owner_id)?
                .ok_or_else(|| anyhow::anyhow!("Koi '{}' not found", new_owner_id))?;
            let pool_session_id = match original.pool_session_id.as_deref() {
                Some(value) => Some({
                    let pool = db
                        .resolve_pool_session_identifier(value)?
                        .ok_or_else(|| anyhow::anyhow!("Pool '{}' not found", value))?;
                    Self::ensure_pool_allows_runtime_work(&pool, "replace Koi todo")?;
                    pool.id
                }),
                None => None,
            };
            (original, new_owner, pool_session_id)
        };

        let replacement_description =
            format!("Replacement for '{}' because: {}", original.title, reason);
        let replacement = {
            let db = self.db().lock().await;
            db.replace_koi_todo(
                &original,
                &new_owner.id,
                task,
                &replacement_description,
                triggered_by,
                Self::todo_source_type(triggered_by),
                reason,
                task_timeout_secs,
            )?
        };

        self.bus.emit_event(
            "koi_todo_updated",
            json!({
                "id": original.id,
                "action": "replaced",
                "by": triggered_by,
                "replacement_todo_id": replacement.id,
            }),
        );
        self.bus.emit_event(
            "koi_todo_updated",
            json!({
                "id": replacement.id,
                "action": "created",
                "by": triggered_by,
            }),
        );

        if let Some(psid) = pool_session_id.as_deref() {
            {
                let db = self.db().lock().await;
                let msg = db.insert_pool_message_ext(
                    psid,
                    triggered_by,
                    &format!(
                        "[Task Replaced] '{}' was replaced by '{}' for {}. Reason: {}",
                        original.title, replacement.title, new_owner.name, reason
                    ),
                    "status_update",
                    &json!({
                        "todo_id": original.id,
                        "replacement_todo_id": replacement.id,
                        "new_owner_id": new_owner.id,
                    })
                    .to_string(),
                    Some(&replacement.id),
                    None,
                    Some("task_replaced"),
                )?;
                self.bus.emit_event(
                    &format!("pool_message_{}", psid),
                    serde_json::to_value(&msg).unwrap_or_default(),
                );

                let mention_content = format!(
                    "@{} [Priority: {}] {}",
                    new_owner.name, replacement.priority, task
                );
                let mention = db.insert_pool_message(
                    psid,
                    triggered_by,
                    &mention_content,
                    "mention",
                    &json!({
                        "target_koi": new_owner.id,
                        "priority": replacement.priority,
                        "replacement_for": original.id,
                        "todo_id": replacement.id,
                        "timeout_secs": replacement.task_timeout_secs,
                    })
                    .to_string(),
                )?;
                self.bus.emit_event(
                    &format!("pool_message_{}", psid),
                    serde_json::to_value(&mention).unwrap_or_default(),
                );
            }

            let runtime = self.clone();
            let psid = psid.to_string();
            let mention_content = format!(
                "@{} [Priority: {}] {}",
                new_owner.name, replacement.priority, task
            );
            let triggered_by = triggered_by.to_string();
            tokio::spawn(async move {
                if let Err(error) = runtime
                    .handle_mention(&triggered_by, &psid, &mention_content)
                    .await
                {
                    tracing::warn!("replace_todo mention dispatch failed: {}", error);
                }
            });
        }

        Ok(replacement)
    }

    /// Combined assign + execute (backward compat for heartbeat patrol and tests).
    pub async fn assign_and_execute(
        &self,
        koi_id: &str,
        task: &str,
        assigned_by: &str,
        pool_session_id: Option<&str>,
        priority: &str,
        task_timeout_secs: Option<u32>,
    ) -> anyhow::Result<KoiExecResult> {
        let (todo, assign_msg_id) = self
            .assign_task(
                koi_id,
                task,
                assigned_by,
                pool_session_id,
                priority,
                task_timeout_secs,
            )
            .await?;
        self.execute_todo(koi_id, &todo, assign_msg_id, pool_session_id)
            .await
    }

    /// Activate a Koi to check pool messages and respond autonomously.
    /// No todo is created — the Koi reads the pool and decides its own actions.
    /// Used when another Koi @mentions this Koi (peer request, not a command).
    pub async fn activate_for_messages(
        &self,
        koi_id: &str,
        pool_session_id: &str,
    ) -> anyhow::Result<KoiExecResult> {
        let (koi_def, pool_session_id) = {
            let db = self.db().lock().await;
            let koi_def = db
                .resolve_koi_identifier(koi_id)?
                .ok_or_else(|| anyhow::anyhow!("Koi '{}' not found", koi_id))?;
            let pool_session = db
                .resolve_pool_session_identifier(pool_session_id)?
                .ok_or_else(|| anyhow::anyhow!("Pool '{}' not found", pool_session_id))?;
            Self::ensure_pool_allows_runtime_work(&pool_session, "activate Koi message handling")?;
            (koi_def, pool_session.id)
        };
        let koi_id = koi_def.id.as_str();
        let run_guard = match KoiRunSlotGuard::acquire(self, koi_id, Some(&pool_session_id)).await {
            Some(guard) => guard,
            None => {
                return Ok(KoiExecResult {
                    success: true,
                    reply: format!("{} is already processing work in this pool", koi_def.name),
                    result_message_id: None,
                });
            }
        };

        let task = include_str!("../../prompts/koi_activate_for_messages.txt")
            .replace("{name}", &koi_def.name)
            .replace("{pool_id}", &pool_session_id);

        let koi_timeout_secs = self
            .resolve_task_timeout_secs(&koi_def, Some(&pool_session_id), None)
            .await;
        let exec_result = match tokio::time::timeout(
            std::time::Duration::from_secs(koi_timeout_secs),
            self.execute_koi_agent(&koi_def, &task, Some(&pool_session_id), None, false),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!(
                "Koi '{}' timed out after {} seconds checking messages",
                koi_def.name,
                koi_timeout_secs
            )),
        };

        let is_timeout = exec_result
            .as_ref()
            .err()
            .map(|e| e.to_string().contains("timed out"))
            .unwrap_or(false);

        let (success, reply) = match &exec_result {
            Ok(reply) => (true, reply.clone()),
            Err(e) => (false, format!("Error: {}", e)),
        };

        // Note: the soft fence is embedded inside reconcile_managed_pool_completion
        // so that it triggers AFTER the agent loop actually finishes (since
        // call_koi background-spawns the agent and returns early; reconcile is
        // the only checkpoint that runs post-agent-completion).
        if let Some(app) = self.try_get_app_handle() {
            reconcile_managed_pool_completion(
                app,
                self.db(),
                &pool_session_id,
                koi_id,
                &koi_def.name,
                &reply,
                success,
            )
            .await;
        }

        let result_msg_id = {
            let db = self.db().lock().await;
            let summary = if reply.chars().count() > 5000 {
                reply.chars().take(5000).collect::<String>()
            } else {
                reply.clone()
            };
            let event_type = if !success {
                Some("task_failed")
            } else if summary.trim().is_empty() {
                None
            } else {
                Some("task_progress")
            };
            let msg_type = "status_update";

            match event_type {
                Some(event_type) => match db.insert_pool_message_ext(
                    &pool_session_id,
                    koi_id,
                    &summary,
                    msg_type,
                    &json!({
                        "activation_source": "mention",
                        "success": success
                    })
                    .to_string(),
                    None,
                    None,
                    Some(event_type),
                ) {
                    Ok(msg) => {
                        self.bus.emit_event(
                            &format!("pool_message_{}", pool_session_id),
                            serde_json::to_value(&msg).unwrap_or_default(),
                        );
                        Some(msg.id)
                    }
                    Err(_) => None,
                },
                None => None,
            }
        };

        // On timeout: block any in_progress todos owned by this Koi and post a
        // @pisci mention so the heartbeat cursor sees a fresh attention event.
        if is_timeout {
            let block_reason = format!(
                "Koi '{}' timed out after {} seconds while checking messages. Needs Pisci intervention.",
                koi_def.name,
                koi_timeout_secs
            );
            let db = self.db().lock().await;
            if let Ok(todos) = db.list_active_todos_by_pool(&pool_session_id) {
                for todo in todos.iter().filter(|t| {
                    t.claimed_by.as_deref() == Some(koi_id) && t.status == "in_progress"
                }) {
                    let _ = db.block_koi_todo(&todo.id, &block_reason);
                }
            }
            let pisci_notice = format!(
                "[ProjectStatus] follow_up_needed @pisci — Koi '{}' timed out while checking messages. \
                 Please inspect the pool and reassign or unblock the stalled work.",
                koi_def.name
            );
            if let Ok(notice_msg) = db.insert_pool_message_ext(
                &pool_session_id,
                koi_id,
                &pisci_notice,
                "status_update",
                "{}",
                None,
                None,
                Some("task_blocked"),
            ) {
                self.bus.emit_event(
                    &format!("pool_message_{}", pool_session_id),
                    serde_json::to_value(&notice_msg).unwrap_or_default(),
                );
            }
        }
        run_guard.release().await;
        Ok(KoiExecResult {
            success,
            reply,
            result_message_id: result_msg_id,
        })
    }

    /// Execute the Koi agent. This is the only part that needs Tauri for now
    /// (because CallKoiTool and AgentLoop need AppHandle/settings).
    /// In tests, override this via a simpler mock.
    ///
    /// When `await_completion` is true, the call returns only after the agent
    /// has finished running AND the post-agent reconcile has executed. This
    /// is required for the soft-fence retry path: we must know the retry's
    /// outcome before deciding whether the hard fence should fire.
    async fn execute_koi_agent(
        &self,
        koi_def: &KoiDefinition,
        task: &str,
        pool_session_id: Option<&str>,
        workspace_override: Option<&str>,
        await_completion: bool,
    ) -> anyhow::Result<String> {
        use crate::agent::tool::{Tool, ToolContext, ToolSettings};
        use tauri::Manager;

        if let Some(app) = self.try_get_app_handle() {
            let state = app.state::<crate::store::AppState>();
            let (workspace_root, allow_outside, tool_settings_data) = {
                let settings = state.settings.lock().await;
                let ws = workspace_override
                    .map(String::from)
                    .unwrap_or_else(|| settings.workspace_root.clone());
                // Koi working inside a project worktree must be confined to that worktree.
                // allow_outside_workspace is a Pisci (general assistant) setting — Koi should
                // never inherit it when a worktree is active, to prevent accidental writes
                // to the main project dir or other parts of the filesystem.
                let allow_out = if workspace_override.is_some() {
                    false // worktree is active: strictly confined
                } else {
                    settings.allow_outside_workspace // no project context: inherit global
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

            // Create notification channel and register in global session registry.
            // Key includes pool_session_id so the same Koi can run in multiple projects concurrently.
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
                // Include pool_session_id in session_id so each project gets an isolated AgentLoop context
                session_id: cancel_key.clone(),
                workspace_root: std::path::PathBuf::from(&workspace_root),
                bypass_permissions: false,
                settings: tool_settings_data,
                // Koi-specific max_iterations overrides the user-configurable system default.
                max_iterations: loop_max_iterations,
                memory_owner_id: koi_def.id.clone(),
                pool_session_id: pool_session_id.map(String::from),
                cancel: cancel.clone(),
            };

            // Prepend workspace environment info so Koi knows where to work
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

            // Always unregister from session registry
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
            // Headless / test mode: simulate execution
            Ok(format!(
                "[TestMode] {} ({}) processed task: {}",
                koi_def.name, koi_def.icon, task
            ))
        }
    }

    /// Try to extract a Tauri AppHandle if the EventBus is TauriEventBus.
    fn try_get_app_handle(&self) -> Option<&tauri::AppHandle> {
        // Safety: we know the concrete types. Use a helper on EventBus.
        // For now, we add a method to EventBus for this.
        self.bus.app_handle()
    }

    /// Soft fence (one-shot). Re-engage this Koi with an explicit "you are
    /// still in Reconciling" task and give it exactly one more turn. The
    /// retry task spells out the three legitimate outcomes (complete_todo /
    /// blocked / cancelled) so the model can mark `failed` work honestly
    /// instead of being force-pushed to `needs_review`.
    ///
    /// This is the runtime-side entry. The standalone function callers
    /// (notably `reconcile_managed_pool_completion`) use
    /// `KoiRuntime::from_tauri` to obtain a runtime and then invoke this.
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

        // Visible audit trail: post a [SoftFence] notice so future readers
        // (and Pisci) can see that the harness re-engaged the Koi. This is
        // NOT the hard-fence protocol_reminder; it explicitly says "another
        // turn was granted".
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
             include `[ProjectStatus] follow_up_needed` and an `@mention` of the next \
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

    async fn peer_handoff_open_todo_count(
        &self,
        sender_koi_id: &str,
        pool_session_id: &str,
    ) -> usize {
        let db = self.db().lock().await;
        let todos = match db.list_koi_todos(Some(sender_koi_id)) {
            Ok(todos) => todos,
            Err(_) => return 0,
        };
        todos
            .into_iter()
            .filter(|todo| {
                todo.pool_session_id.as_deref() == Some(pool_session_id)
                    && matches!(todo.status.as_str(), "todo" | "in_progress")
            })
            .count()
    }

    async fn emit_peer_handoff_warning_if_needed(
        &self,
        sender_koi_id: &str,
        sender_name: &str,
        pool_session_id: &str,
        content: &str,
    ) {
        let is_handoff = matches!(
            crate::pisci::project_state::extract_project_status_signal(content),
            Some(crate::pisci::project_state::STATUS_FOLLOW_UP)
        );
        if !is_handoff {
            return;
        }
        let open_todo_count = self
            .peer_handoff_open_todo_count(sender_koi_id, pool_session_id)
            .await;
        if open_todo_count == 0 {
            return;
        }
        let warning = format!(
            "[ProtocolWarning] {} posted `[ProjectStatus] follow_up_needed` while {} of their todo(s) in this pool are still open. The handoff is allowed, but the sender may need to finish, block, or explicitly reconcile those tasks.",
            sender_name, open_todo_count
        );
        let db = self.db().lock().await;
        if let Ok(msg) = db.insert_pool_message_ext(
            pool_session_id,
            "system",
            &warning,
            "status_update",
            &json!({
                "sender_koi_id": sender_koi_id,
                "open_todo_count": open_todo_count,
                "protocol_warning": "handoff_with_open_todos"
            })
            .to_string(),
            None,
            None,
            Some("protocol_warning"),
        ) {
            self.bus.emit_event(
                &format!("pool_message_{}", pool_session_id),
                serde_json::to_value(&msg).unwrap_or_default(),
            );
        }
    }

    /// Handle an @mention or @all in the chat pool.
    ///
    /// Unified wake-up semantics: every mentioned Koi is activated via
    /// `activate_for_messages` (or notified if already busy). The activated
    /// agent reads pool context and autonomously decides whether to create
    /// a todo, respond, or ignore the message. The runtime never creates
    /// todos or assigns tasks on behalf of agents.
    ///
    /// @all targets every non-offline Koi (excluding sender).
    pub async fn handle_mention(
        &self,
        sender_id: &str,
        pool_session_id: &str,
        content: &str,
    ) -> anyhow::Result<Vec<KoiExecResult>> {
        let mut results = Vec::new();

        let kois = {
            let db = self.db().lock().await;
            db.list_kois().unwrap_or_default()
        };

        let sender_is_koi = kois.iter().any(|k| k.id == sender_id);
        let mention_all = content.contains("@all");
        let sender_name = kois
            .iter()
            .find(|k| k.id == sender_id)
            .map(|k| k.name.as_str())
            .unwrap_or(sender_id);

        if sender_is_koi {
            self.emit_peer_handoff_warning_if_needed(
                sender_id,
                sender_name,
                pool_session_id,
                content,
            )
            .await;
        }

        for koi in &kois {
            if koi.status == "offline" || koi.id == sender_id {
                continue;
            }
            let mention = format!("@{}", koi.name);
            let mentioned = content.contains(&mention);
            if !mention_all && !mentioned {
                continue;
            }

            let koi_session_key = Self::koi_pool_session_key(&koi.id, pool_session_id);
            let notification = Self::build_pool_notification(&koi.name, sender_name, content);

            let existing_tx = {
                let sessions = KOI_SESSIONS.lock().await;
                sessions.get(&koi_session_key).cloned()
            };
            if let Some(tx) = existing_tx {
                let _ = tx.send(notification).await;
                tracing::info!("Injected @mention notification to busy Koi '{}'", koi.name);
                results.push(KoiExecResult {
                    success: true,
                    reply: format!("Notification sent to busy Koi '{}'", koi.name),
                    result_message_id: None,
                });
            } else if self.is_koi_run_active(&koi.id, Some(pool_session_id)).await {
                self.queue_pending_notification(&koi_session_key, notification)
                    .await;
                results.push(KoiExecResult {
                    success: true,
                    reply: format!(
                        "Queued notification for Koi '{}' while its pool run is starting",
                        koi.name
                    ),
                    result_message_id: None,
                });
            } else {
                let result = self.activate_for_messages(&koi.id, pool_session_id).await?;
                results.push(result);
            }
        }

        Ok(results)
    }

    /// Activate pending todos that haven't been claimed yet.
    pub async fn activate_pending_todos(
        &self,
        pool_session_id: Option<&str>,
    ) -> anyhow::Result<u32> {
        let (todos, active_pool_ids) = {
            let db = self.db().lock().await;
            let todos = db.list_koi_todos(None)?;
            let active_pool_ids = db
                .list_pool_sessions()?
                .into_iter()
                .filter(|pool| pool.status == "active")
                .map(|pool| pool.id)
                .collect::<HashSet<_>>();
            (todos, active_pool_ids)
        };

        let pending: Vec<&KoiTodo> = todos
            .iter()
            .filter(|t| {
                if t.status != "todo" || t.claimed_by.is_some() {
                    return false;
                }
                match pool_session_id {
                    Some(psid) => {
                        if t.pool_session_id.as_deref() != Some(psid) {
                            return false;
                        }
                        active_pool_ids.contains(psid)
                    }
                    None => {
                        // Global patrol must never reach into pool-scoped work.
                        // Pool todos may only be activated when an explicit pool_id
                        // is provided, preserving hard isolation between pools.
                        t.pool_session_id.is_none()
                    }
                }
            })
            .collect();

        let mut activated = 0u32;
        for todo in pending {
            let koi_status = {
                let db = self.db().lock().await;
                db.get_koi(&todo.owner_id)?
                    .map(|k| k.status)
                    .unwrap_or_else(|| "offline".to_string())
            };

            if koi_status != "idle" {
                continue;
            }

            // This patrol path should resume the existing todo, not create a fresh duplicate.
            let result = self
                .execute_todo(&todo.owner_id, todo, None, todo.pool_session_id.as_deref())
                .await;

            if result.is_ok() {
                activated += 1;
            }
        }

        Ok(activated)
    }

    /// Create a Git worktree for a Koi task, returning the worktree directory path.
    fn setup_worktree(&self, project_dir: &str, koi_name: &str, todo_id: &str) -> Option<String> {
        let dir = std::path::Path::new(project_dir);
        if !dir.join(".git").exists() {
            return None;
        }
        let short_id = &todo_id[..8.min(todo_id.len())];
        let safe_name =
            koi_name.replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "_");
        let branch_name = format!("koi/{}-{}", safe_name, short_id);
        let wt_dir = dir
            .parent()
            .unwrap_or(dir)
            .join(".koi-worktrees")
            .join(format!("{}-{}", safe_name, short_id));

        let wt_str = wt_dir.to_string_lossy().to_string();
        let output = std::process::Command::new("git")
            .args(["worktree", "add", &wt_str, "-b", &branch_name])
            .current_dir(dir)
            .output();

        match output {
            Ok(o) if o.status.success() => {
                tracing::info!("Worktree created at {} (branch {})", wt_str, branch_name);
                Some(wt_str)
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                tracing::warn!("Failed to create worktree: {}", stderr);
                None
            }
            Err(e) => {
                tracing::warn!("Git worktree command failed: {}", e);
                None
            }
        }
    }

    /// Commit changes and remove a Git worktree after task completion.
    fn cleanup_worktree(&self, worktree_path: &str, koi_name: &str, task: &str) {
        let wt = std::path::Path::new(worktree_path);
        if !wt.exists() {
            return;
        }

        let task_preview = if task.chars().count() > 72 {
            task.chars().take(72).collect::<String>()
        } else {
            task.to_string()
        };
        let commit_msg = format!("koi/{}: {}", koi_name, task_preview);

        let _ = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(wt)
            .output();
        let _ = std::process::Command::new("git")
            .args(["commit", "-m", &commit_msg, "--allow-empty"])
            .current_dir(wt)
            .output();

        // Find parent repo dir from worktree to remove it
        let output = std::process::Command::new("git")
            .args(["worktree", "remove", worktree_path, "--force"])
            .current_dir(wt.parent().unwrap_or(wt))
            .output();
        match output {
            Ok(o) if o.status.success() => {
                tracing::info!("Worktree removed: {}", worktree_path);
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                tracing::warn!("Failed to remove worktree {}: {}", worktree_path, stderr);
            }
            Err(e) => {
                tracing::warn!("Git worktree remove command failed: {}", e);
            }
        }
    }

    async fn set_koi_status(&self, koi_id: &str, status: &str) {
        let db = self.db().lock().await;
        let _ = db.update_koi_status(koi_id, status);
        drop(db);
        self.bus.emit_event(
            "koi_status_changed",
            json!({ "id": koi_id, "status": status }),
        );
    }

    /// Recover Koi that appear "busy" in the DB but have neither an active todo
    /// nor a live AgentLoop session. These are usually orphaned states left behind
    /// by failed or interrupted mention-driven activations.
    async fn recover_detached_busy_kois(&self, max_age_secs: i64) -> u32 {
        if max_age_secs <= 0 {
            return 0;
        }

        let active_session_owners: HashSet<String> = {
            let sessions = KOI_SESSIONS.lock().await;
            sessions
                .keys()
                .filter_map(|key| key.split(':').next().map(str::to_string))
                .collect()
        };

        let stale_ids: Vec<String> = {
            let db = self.db().lock().await;
            let kois = db.list_kois().unwrap_or_default();
            let todos = db.list_koi_todos(None).unwrap_or_default();
            let active_todo_owners: HashSet<String> = todos
                .into_iter()
                .filter(|todo| !matches!(todo.status.as_str(), "done" | "cancelled"))
                .map(|todo| todo.owner_id)
                .collect();
            let now = Utc::now();

            kois.into_iter()
                .filter(|koi| {
                    koi.status == "busy"
                        && !active_session_owners.contains(&koi.id)
                        && !active_todo_owners.contains(&koi.id)
                        && (now - koi.updated_at).num_seconds() >= max_age_secs
                })
                .map(|koi| koi.id)
                .collect()
        };

        if stale_ids.is_empty() {
            return 0;
        }

        let db = self.db().lock().await;
        for koi_id in &stale_ids {
            let _ = db.update_koi_status(koi_id, "idle");
        }
        drop(db);

        for koi_id in &stale_ids {
            self.bus.emit_event(
                "koi_status_changed",
                json!({ "id": koi_id, "status": "idle" }),
            );
        }

        stale_ids.len() as u32
    }

    /// List available (idle) Koi agents.
    pub async fn list_available_kois(&self) -> Vec<KoiDefinition> {
        let db = self.db().lock().await;
        db.list_kois()
            .unwrap_or_default()
            .into_iter()
            .filter(|k| k.status != "offline")
            .collect()
    }

    /// Watchdog: detect and recover stale Koi states.
    /// Call this periodically (e.g., every 5 minutes from the heartbeat loop).
    /// - Resets Koi that have been "busy" for over `max_busy_secs` seconds back to "idle"
    /// - Resets "in_progress" todos older than `max_busy_secs` back to "todo"
    pub async fn watchdog_recover(&self, max_busy_secs: i64) -> (u32, u32) {
        let db = self.db().lock().await;
        let stale_koi_count = db.recover_stale_busy_kois(max_busy_secs).unwrap_or(0);
        let stale_todo_count = db
            .recover_stale_in_progress_todos(max_busy_secs)
            .unwrap_or(0);
        drop(db);
        let detached_koi_count = self
            .recover_detached_busy_kois(max_busy_secs.min(120))
            .await;
        let total_koi_count = stale_koi_count + detached_koi_count;
        if total_koi_count > 0 || stale_todo_count > 0 {
            tracing::warn!(
                "Watchdog: recovered {} stale Koi, {} stale todos (threshold {}s)",
                total_koi_count,
                stale_todo_count,
                max_busy_secs
            );
            self.bus.emit_event(
                "koi_stale_recovered",
                serde_json::json!({
                    "koi_count": total_koi_count,
                    "todo_count": stale_todo_count,
                }),
            );
        }
        (total_koi_count, stale_todo_count)
    }
}
