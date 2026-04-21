//! Kernel-owned coordinator for Koi turns.
//!
//! This module is the Phase 2 replacement for the old desktop
//! [`KoiRuntime::execute_todo`] / `resume_todo` / `replace_todo` /
//! `handle_mention` methods. It knows how to:
//!
//! 1. Claim a todo + post its `task_claimed` pool message.
//! 2. Set up a git worktree (best-effort) and resolve the final
//!    workspace path.
//! 3. Build a [`KoiTurnRequest`] from the todo + Koi definition + prompt
//!    template.
//! 4. Hand the request to a [`SubagentRuntime`] and await the outcome.
//! 5. Translate the outcome into pool messages, todo-status transitions
//!    (`done` / `blocked` / `needs_review`) and [`PoolEvent`]s.
//! 6. Cleanup the worktree.
//!
//! None of this logic talks to Tauri, Chromium, or any GUI — hosts inject
//! a concrete `SubagentRuntime` implementation (subprocess on desktop /
//! CLI; stub in tests) and the coordinator does the rest.
//!
//! # Entry points
//!
//! * [`execute_todo_turn`] — the happy path called by `assign_koi` and
//!   (indirectly) by `resume_blocked_todo` / `replace_blocked_todo`.
//! * [`handle_mention`] — fan-out triggered when a `pool_chat` message
//!   mentions one or more Kois. Each mentioned Koi (that isn't the
//!   sender and isn't offline) gets its own fresh subagent turn.
//! * [`resume_blocked_todo`] — reactivate a `blocked` / `needs_review`
//!   todo.
//! * [`replace_blocked_todo`] — cancel the original and spawn a fresh
//!   todo for a different owner, then dispatch it immediately.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use pisci_core::host::{
    KoiTurnExit, KoiTurnOutcome, KoiTurnRequest, PoolEvent, PoolEventSink, SubagentRuntime,
    TodoChangeAction,
};
use pisci_core::models::{KoiDefinition, KoiTodo, PoolMessage, PoolSession};
use serde_json::{json, Value};

use super::git;
use super::store::PoolStore;

/// Default per-task timeout when no Koi-, pool-, or settings-level
/// override applies. 10 minutes mirrors the desktop default. Callers
/// that want a different global default thread it through
/// [`CoordinatorConfig::default_task_timeout_secs`].
pub const DEFAULT_TASK_TIMEOUT_SECS: u32 = 600;

/// Embedded prompt template — same content as the old desktop
/// `prompts/koi_execute_todo.txt` but owned by the kernel so any host
/// (subprocess or in-process) produces the same system message.
///
/// Placeholders:
/// * `{task}`    — the brief the agent was assigned.
/// * `{name}`    — Koi display name (`KoiDefinition::name`).
/// * `{todo_id}` — short 8-char prefix so the agent can quote it back.
pub const KOI_EXECUTE_TODO_PROMPT: &str = include_str!("./koi_execute_todo.tmpl");

/// Knobs every host injects when building a coordinator call. Defaults
/// match the legacy desktop behaviour so the CLI host can use
/// `CoordinatorConfig::default()` until it grows its own settings.
#[derive(Debug, Clone)]
pub struct CoordinatorConfig {
    /// Absolute fallback timeout when neither the Koi, the pool, nor
    /// the per-todo override says otherwise.
    pub default_task_timeout_secs: u32,
    /// Whether to set up and tear down a git worktree per turn when
    /// the pool has a `project_dir`. CLI hosts that test prompts
    /// outside a git repo set this to `false`.
    pub use_worktrees: bool,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            default_task_timeout_secs: DEFAULT_TASK_TIMEOUT_SECS,
            use_worktrees: true,
        }
    }
}

/// Outcome surfaced by [`execute_todo_turn`]. Mirrors the shape of the
/// old desktop `KoiExecResult` so `call_koi` / `assign_koi` can format
/// responses unchanged.
#[derive(Debug, Clone)]
pub struct KoiExecResult {
    pub success: bool,
    pub reply: String,
    pub result_message_id: Option<i64>,
    pub exit_kind: KoiTurnExit,
}

impl KoiExecResult {
    pub fn to_json(&self) -> Value {
        json!({
            "success": self.success,
            "reply": self.reply,
            "result_message_id": self.result_message_id,
            "exit_kind": match self.exit_kind {
                KoiTurnExit::Completed => "completed",
                KoiTurnExit::Cancelled => "cancelled",
                KoiTurnExit::TimedOut => "timed_out",
                KoiTurnExit::Crashed => "crashed",
            },
        })
    }
}

// ─── execute_todo_turn ────────────────────────────────────────────────

/// Input arguments for a single Koi turn. All identifiers are already
/// resolved by the caller (kernel services have `resolve_pool` etc.);
/// the coordinator trusts them.
#[derive(Debug, Clone)]
pub struct ExecuteTodoArgs {
    /// Short Koi identifier (will be looked up via
    /// [`resolve_koi_identifier`](crate::store::Database::resolve_koi_identifier)
    /// so both UUID and display name are accepted).
    pub koi_id: String,
    /// Todo the Koi is about to work on. If its `claimed_by` is `None`
    /// the coordinator claims it under `koi_id`; if it's already
    /// claimed by someone else the call is rejected.
    pub todo_id: String,
    /// When the turn was triggered by a `@mention`, the id of that
    /// mention message — used to link the result back in the pool chat.
    pub assign_msg_id: Option<i64>,
    /// Session id the parent wants propagated to the subagent; usually
    /// `format!("koi_runtime_{}_{}", koi_id, pool_id_or_default)`.
    pub session_id: String,
    /// Extra tool-profile hints forwarded verbatim to the subagent.
    pub extra_tool_profile: Vec<String>,
    /// Optional `extra_system_context` (continuity / memory snippets
    /// already assembled by the host).
    pub extra_system_context: Option<String>,
}

pub async fn execute_todo_turn(
    store: &PoolStore,
    sink: Arc<dyn PoolEventSink>,
    subagent: Arc<dyn SubagentRuntime>,
    cfg: &CoordinatorConfig,
    args: ExecuteTodoArgs,
) -> anyhow::Result<KoiExecResult> {
    let (koi, todo, pool_session) = resolve_turn_context(store, &args).await?;
    if let Some(pool) = &pool_session {
        ensure_pool_allows_runtime_work(pool, "run Koi work")?;
    }
    let canonical_pool_id = pool_session.as_ref().map(|p| p.id.clone());

    claim_and_announce(
        store,
        sink.as_ref(),
        &koi,
        &todo,
        canonical_pool_id.as_deref(),
        args.assign_msg_id,
    )
    .await?;

    let workspace =
        maybe_setup_worktree(store, cfg, canonical_pool_id.as_deref(), &koi, &todo).await;

    let task = todo.title.clone();
    let prompt = render_execute_prompt(&koi, &todo);
    let timeout_secs = cfg.default_task_timeout_secs.max(koi_timeout_for_todo(
        &koi,
        todo.task_timeout_secs,
        cfg.default_task_timeout_secs,
    ));

    let request = KoiTurnRequest {
        pool_id: canonical_pool_id.clone().unwrap_or_default(),
        koi_id: koi.id.clone(),
        session_id: args.session_id.clone(),
        todo_id: Some(todo.id.clone()),
        system_prompt: koi.system_prompt.clone(),
        user_prompt: prompt,
        workspace: workspace.as_ref().map(|p| p.to_string_lossy().into_owned()),
        task_timeout_secs: Some(timeout_secs),
        extra_tool_profile: args.extra_tool_profile.clone(),
        extra_system_context: args.extra_system_context.clone(),
    };

    let outcome = run_subagent_turn(subagent, request, timeout_secs).await?;

    let (success, raw_reply) = match outcome.exit_kind {
        KoiTurnExit::Completed => (true, outcome.response_text.clone()),
        KoiTurnExit::Cancelled => (
            false,
            outcome
                .error
                .clone()
                .unwrap_or_else(|| "Koi turn cancelled".into()),
        ),
        KoiTurnExit::TimedOut => (
            false,
            format!(
                "Koi '{}' timed out after {timeout_secs} seconds on task: {}",
                koi.name, task
            ),
        ),
        KoiTurnExit::Crashed => (
            false,
            outcome
                .error
                .clone()
                .unwrap_or_else(|| format!("Koi '{}' subagent crashed", koi.name)),
        ),
    };

    let result_msg_id = record_turn_outcome(
        store,
        sink.as_ref(),
        canonical_pool_id.as_deref(),
        &koi,
        &todo,
        args.assign_msg_id,
        success,
        &raw_reply,
    )
    .await?;

    if cfg.use_worktrees {
        if let Some(wt) = workspace.as_ref() {
            git::cleanup_worktree(wt, &koi.name, &task);
        }
    }

    Ok(KoiExecResult {
        success,
        reply: raw_reply,
        result_message_id: result_msg_id,
        exit_kind: outcome.exit_kind,
    })
}

async fn run_subagent_turn(
    subagent: Arc<dyn SubagentRuntime>,
    request: KoiTurnRequest,
    timeout_secs: u32,
) -> anyhow::Result<KoiTurnOutcome> {
    let handle = subagent.spawn_koi_turn(request).await?;
    let wait = subagent.clone();
    let h2 = handle.clone();

    let outcome = tokio::select! {
        res = wait.wait_koi_turn(&h2) => res?,
        _ = tokio::time::sleep(Duration::from_secs(timeout_secs as u64)) => {
            tracing::warn!(
                target: "pool::coordinator",
                koi_id = %h2.koi_id,
                pool_id = %h2.pool_id,
                "host-side timeout after {timeout_secs}s — cancelling subagent"
            );
            // Best-effort cancel; then synthesise a TimedOut outcome so
            // the caller doesn't have to await the cancel round-trip.
            let _ = subagent.cancel_koi_turn(&h2).await;
            KoiTurnOutcome {
                handle: h2.clone(),
                exit_kind: KoiTurnExit::TimedOut,
                response_text: String::new(),
                error: Some(format!("host timeout after {timeout_secs}s")),
                exit_code: None,
            }
        }
    };
    Ok(outcome)
}

async fn resolve_turn_context(
    store: &PoolStore,
    args: &ExecuteTodoArgs,
) -> anyhow::Result<(KoiDefinition, KoiTodo, Option<PoolSession>)> {
    let koi_lookup = args.koi_id.clone();
    let todo_lookup = args.todo_id.clone();

    let (koi_opt, todo_opt) = store
        .read(move |db| {
            let koi = db.resolve_koi_identifier(&koi_lookup)?;
            let todo = db.get_koi_todo(&todo_lookup)?;
            Ok::<_, anyhow::Error>((koi, todo))
        })
        .await?;
    let todo = todo_opt.ok_or_else(|| anyhow::anyhow!("Todo '{}' not found", args.todo_id))?;
    let koi = match koi_opt {
        Some(k) => k,
        None => {
            let fallback = todo.owner_id.clone();
            store
                .read(move |db| db.resolve_koi_identifier(&fallback))
                .await?
                .ok_or_else(|| anyhow::anyhow!("Koi '{}' not found", args.koi_id))?
        }
    };

    let pool_session = match todo.pool_session_id.as_deref() {
        Some(pid) => {
            let pid = pid.to_string();
            store
                .read(move |db| db.resolve_pool_session_identifier(&pid))
                .await?
        }
        None => None,
    };

    Ok((koi, todo, pool_session))
}

fn ensure_pool_allows_runtime_work(pool: &PoolSession, action: &str) -> anyhow::Result<()> {
    if pool.status == "active" {
        return Ok(());
    }
    anyhow::bail!(
        "Pool '{}' is {} and cannot {} until it is resumed",
        pool.name,
        pool.status,
        action
    )
}

async fn claim_and_announce(
    store: &PoolStore,
    sink: &dyn PoolEventSink,
    koi: &KoiDefinition,
    todo: &KoiTodo,
    pool_id: Option<&str>,
    assign_msg_id: Option<i64>,
) -> anyhow::Result<()> {
    let claim_id = todo.id.clone();
    let claim_by = koi.id.clone();
    store
        .write(move |db| db.claim_koi_todo(&claim_id, &claim_by))
        .await?;

    // Re-read so the TodoChanged snapshot reflects the claimed_by /
    // claimed_at / status update.
    let todo_id = todo.id.clone();
    if let Some(refreshed) = store.read(move |db| db.get_koi_todo(&todo_id)).await? {
        sink.emit_pool(&PoolEvent::TodoChanged {
            pool_id: pool_id.map(String::from).unwrap_or_default(),
            action: TodoChangeAction::Claimed,
            todo: (&refreshed).into(),
        });
    }

    if let Some(psid) = pool_id {
        let content = format!("{} 接受了任务: {}", koi.name, todo.title);
        let psid_owned = psid.to_string();
        let koi_id = koi.id.clone();
        let todo_id = todo.id.clone();
        let msg = store
            .write(move |db| {
                db.insert_pool_message_ext(
                    &psid_owned,
                    &koi_id,
                    &content,
                    "task_claimed",
                    "{}",
                    Some(&todo_id),
                    assign_msg_id,
                    Some("task_claimed"),
                )
            })
            .await?;
        sink.emit_pool(&PoolEvent::MessageAppended {
            pool_id: psid.to_string(),
            message: (&msg).into(),
        });
    }
    Ok(())
}

async fn maybe_setup_worktree(
    store: &PoolStore,
    cfg: &CoordinatorConfig,
    pool_id: Option<&str>,
    koi: &KoiDefinition,
    todo: &KoiTodo,
) -> Option<PathBuf> {
    if !cfg.use_worktrees {
        return None;
    }
    let psid = pool_id?.to_string();
    let project_dir = store
        .read(move |db| db.get_pool_session(&psid))
        .await
        .ok()
        .flatten()
        .and_then(|s| s.project_dir)?;
    let dir = PathBuf::from(&project_dir);
    git::setup_worktree(&dir, &koi.name, &todo.id)
}

fn render_execute_prompt(koi: &KoiDefinition, todo: &KoiTodo) -> String {
    let short: String = todo.id.chars().take(8).collect();
    KOI_EXECUTE_TODO_PROMPT
        .replace("{task}", &todo.title)
        .replace("{name}", &koi.name)
        .replace("{todo_id}", &short)
}

fn koi_timeout_for_todo(koi: &KoiDefinition, todo_timeout_secs: u32, default_secs: u32) -> u32 {
    if todo_timeout_secs > 0 {
        todo_timeout_secs
    } else if koi.task_timeout_secs > 0 {
        koi.task_timeout_secs
    } else {
        default_secs
    }
}

#[allow(clippy::too_many_arguments)]
async fn record_turn_outcome(
    store: &PoolStore,
    sink: &dyn PoolEventSink,
    pool_id: Option<&str>,
    koi: &KoiDefinition,
    todo: &KoiTodo,
    assign_msg_id: Option<i64>,
    run_success: bool,
    raw_reply: &str,
) -> anyhow::Result<Option<i64>> {
    // Re-read the todo to see if the subagent already called
    // `pool_org(action="complete_todo")` — that tool sets `status="done"`
    // directly, so we must not overwrite it with `needs_review`.
    let todo_id = todo.id.clone();
    let current = store.read(move |db| db.get_koi_todo(&todo_id)).await?;
    let todo_already_done = current
        .as_ref()
        .map(|t| t.status == "done")
        .unwrap_or(false);

    let explicitly_completed = if let Some(psid) = pool_id {
        if run_success && raw_reply.trim().is_empty() {
            let psid_owned = psid.to_string();
            let koi_id = koi.id.clone();
            store
                .read(move |db| db.get_latest_unlinked_result_message_id(&psid_owned, &koi_id))
                .await?
                .is_some()
        } else {
            false
        }
    } else {
        false
    };
    let test_mode_completed =
        run_success && pool_id.is_none() && raw_reply.starts_with("[TestMode]");
    let completion_recorded = explicitly_completed || test_mode_completed;

    let result_msg_id = if let Some(psid) = pool_id {
        if run_success && raw_reply.trim().is_empty() && explicitly_completed {
            let psid_owned = psid.to_string();
            let koi_id = koi.id.clone();
            let todo_id = todo.id.clone();
            let existing = store
                .write(move |db| {
                    let id = db.get_latest_unlinked_result_message_id(&psid_owned, &koi_id)?;
                    if let Some(msg_id) = id {
                        let _ = db.link_pool_message_to_todo(msg_id, &todo_id);
                    }
                    Ok::<_, anyhow::Error>(id)
                })
                .await?;
            if let Some(msg_id) = existing {
                let psid_lookup = psid.to_string();
                if let Some(msg) = store
                    .read(move |db| db.get_pool_message_by_id(msg_id))
                    .await
                    .ok()
                    .flatten()
                {
                    sink.emit_pool(&PoolEvent::MessageAppended {
                        pool_id: psid_lookup,
                        message: (&msg).into(),
                    });
                }
                Some(msg_id)
            } else {
                None
            }
        } else if !run_success {
            let summary = truncate_chars(raw_reply, 5000);
            let metadata = json!({
                "todo_id": todo.id,
                "success": false,
            })
            .to_string();
            let msg = insert_ext_message(
                store,
                psid,
                &koi.id,
                &summary,
                "status_update",
                &metadata,
                Some(&todo.id),
                assign_msg_id,
                Some("task_failed"),
            )
            .await?;
            sink.emit_pool(&PoolEvent::MessageAppended {
                pool_id: psid.to_string(),
                message: (&msg).into(),
            });
            Some(msg.id)
        } else if !todo_already_done && !completion_recorded {
            let koi_output = if raw_reply.chars().count() > 5000 {
                format!("{}...", truncate_chars(raw_reply, 5000))
            } else {
                raw_reply.to_string()
            };
            if !koi_output.trim().is_empty() {
                let meta = json!({
                    "todo_id": todo.id,
                    "auto_captured": true,
                })
                .to_string();
                let output_msg = insert_ext_message(
                    store,
                    psid,
                    &koi.id,
                    &koi_output,
                    "status_update",
                    &meta,
                    Some(&todo.id),
                    assign_msg_id,
                    Some("task_progress"),
                )
                .await?;
                sink.emit_pool(&PoolEvent::MessageAppended {
                    pool_id: psid.to_string(),
                    message: (&output_msg).into(),
                });
            }
            let reminder = format!(
                "[ProtocolReminder] {} finished executing on '{}' without calling complete_todo. \
                 The task output has been captured above. Todo status set to needs_review.",
                koi.name, todo.title
            );
            let meta = json!({
                "todo_id": todo.id,
                "protocol_reminder": "missing_complete_todo",
            })
            .to_string();
            let msg = insert_ext_message(
                store,
                psid,
                "system",
                &reminder,
                "status_update",
                &meta,
                Some(&todo.id),
                assign_msg_id,
                Some("protocol_reminder"),
            )
            .await?;
            sink.emit_pool(&PoolEvent::MessageAppended {
                pool_id: psid.to_string(),
                message: (&msg).into(),
            });
            Some(msg.id)
        } else {
            None
        }
    } else {
        None
    };

    if !todo_already_done {
        let todo_id = todo.id.clone();
        let reply_for_block = raw_reply.to_string();
        let rmid = result_msg_id;
        store
            .write(move |db| {
                if !run_success {
                    db.block_koi_todo(&todo_id, &reply_for_block)?;
                } else if test_mode_completed {
                    db.complete_koi_todo(&todo_id, rmid)?;
                } else if !completion_recorded {
                    db.mark_koi_todo_needs_review(
                        &todo_id,
                        "Agent finished without calling complete_todo",
                    )?;
                }
                Ok::<_, anyhow::Error>(())
            })
            .await?;
    }

    let action = if todo_already_done || completion_recorded {
        TodoChangeAction::Completed
    } else if !run_success {
        TodoChangeAction::Blocked
    } else {
        TodoChangeAction::Updated
    };

    let todo_id = todo.id.clone();
    if let Some(refreshed) = store.read(move |db| db.get_koi_todo(&todo_id)).await? {
        sink.emit_pool(&PoolEvent::TodoChanged {
            pool_id: pool_id.map(String::from).unwrap_or_default(),
            action,
            todo: (&refreshed).into(),
        });
    }

    Ok(result_msg_id)
}

fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect()
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_ext_message(
    store: &PoolStore,
    pool_id: &str,
    sender_id: &str,
    content: &str,
    msg_type: &str,
    metadata: &str,
    todo_id: Option<&str>,
    reply_to: Option<i64>,
    event_type: Option<&str>,
) -> anyhow::Result<PoolMessage> {
    let pool_id = pool_id.to_string();
    let sender_id = sender_id.to_string();
    let content = content.to_string();
    let msg_type = msg_type.to_string();
    let metadata = metadata.to_string();
    let todo_id = todo_id.map(|s| s.to_string());
    let event_type = event_type.map(|s| s.to_string());
    store
        .write(move |db| {
            db.insert_pool_message_ext(
                &pool_id,
                &sender_id,
                &content,
                &msg_type,
                &metadata,
                todo_id.as_deref(),
                reply_to,
                event_type.as_deref(),
            )
        })
        .await
}

// ─── resume_blocked_todo / replace_blocked_todo ──────────────────────

/// Reactivate a `blocked` / `needs_review` todo by running a fresh
/// subagent turn for the original owner. Emits a `TodoChanged{Resumed}`
/// event plus the usual `task_claimed` / result messages.
pub async fn resume_blocked_todo(
    store: &PoolStore,
    sink: Arc<dyn PoolEventSink>,
    subagent: Arc<dyn SubagentRuntime>,
    cfg: &CoordinatorConfig,
    todo_id: &str,
    triggered_by: &str,
) -> anyhow::Result<KoiTodo> {
    let todo_lookup = todo_id.to_string();
    let todo = store
        .read(move |db| db.get_koi_todo(&todo_lookup))
        .await?
        .ok_or_else(|| anyhow::anyhow!("Todo '{}' not found", todo_id))?;

    if !matches!(todo.status.as_str(), "blocked" | "needs_review") {
        anyhow::bail!(
            "Todo '{}' is '{}' and cannot be resumed. Only blocked/needs_review todos are resumable.",
            todo.id,
            todo.status
        );
    }

    let owner_lookup = todo.owner_id.clone();
    let owner = store
        .read(move |db| db.resolve_koi_identifier(&owner_lookup))
        .await?
        .ok_or_else(|| anyhow::anyhow!("Owner '{}' not found", todo.owner_id))?;

    let pool_session = match todo.pool_session_id.as_deref() {
        Some(pid) => {
            let pid = pid.to_string();
            let lookup = pid.clone();
            let pool = store
                .read(move |db| db.resolve_pool_session_identifier(&lookup))
                .await?
                .ok_or_else(|| anyhow::anyhow!("Pool '{}' not found", pid))?;
            ensure_pool_allows_runtime_work(&pool, "resume Koi work")?;
            Some(pool)
        }
        None => None,
    };

    // Flip the todo back to `in_progress` and (best-effort) post a
    // resumed marker before dispatching the subagent turn.
    {
        let todo_id = todo.id.clone();
        let owner_id = owner.id.clone();
        store
            .write(move |db| db.resume_koi_todo(&todo_id, &owner_id))
            .await?;
    }

    if let Some(pool) = &pool_session {
        let reminder = format!(
            "[Task Resumed] {} resumed '{}' for {}.",
            triggered_by, todo.title, owner.name
        );
        let meta = json!({
            "todo_id": todo.id,
            "resumed_by": triggered_by,
            "owner_id": owner.id,
        })
        .to_string();
        let msg = insert_ext_message(
            store,
            &pool.id,
            triggered_by,
            &reminder,
            "status_update",
            &meta,
            Some(&todo.id),
            None,
            Some("task_resumed"),
        )
        .await?;
        sink.as_ref().emit_pool(&PoolEvent::MessageAppended {
            pool_id: pool.id.clone(),
            message: (&msg).into(),
        });
    }

    // Fire-and-forget subagent turn. We return the freshly-resumed
    // todo without waiting for the execution to finish so the UI snaps
    // back immediately — the result arrives via PoolEvents later.
    let todo_for_run = {
        let mut t = todo.clone();
        t.status = "in_progress".into();
        t.claimed_by = Some(owner.id.clone());
        t.claimed_at = Some(Utc::now());
        t.blocked_reason = None;
        t.updated_at = Utc::now();
        t
    };
    sink.as_ref().emit_pool(&PoolEvent::TodoChanged {
        pool_id: pool_session
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_default(),
        action: TodoChangeAction::Resumed,
        todo: (&todo_for_run).into(),
    });

    let store_cl = store.clone();
    let sink_cl = sink.clone();
    let subagent_cl = subagent.clone();
    let cfg_cl = cfg.clone();
    let owner_id = owner.id.clone();
    let todo_id = todo.id.clone();
    let session_id = format!(
        "koi_runtime_{}_{}",
        owner.id,
        pool_session
            .as_ref()
            .map(|p| p.id.as_str())
            .unwrap_or("default")
    );
    let pool_id_for_task = pool_session.as_ref().map(|p| p.id.clone());
    tokio::spawn(async move {
        let args = ExecuteTodoArgs {
            koi_id: owner_id,
            todo_id: todo_id.clone(),
            assign_msg_id: None,
            session_id,
            extra_tool_profile: Vec::new(),
            extra_system_context: None,
        };
        if let Err(e) = execute_todo_turn(&store_cl, sink_cl, subagent_cl, &cfg_cl, args).await {
            tracing::warn!(
                target: "pool::coordinator",
                pool_id = ?pool_id_for_task,
                todo_id = %todo_id,
                "resume_blocked_todo execution failed: {e}"
            );
        }
    });

    let todo_id = todo.id.clone();
    store
        .read(move |db| db.get_koi_todo(&todo_id))
        .await?
        .ok_or_else(|| anyhow::anyhow!("Todo '{}' disappeared after resume", todo.id))
}

/// Cancel the original todo and create a fresh one for a different
/// owner, then dispatch that fresh todo immediately. Returns the
/// replacement `KoiTodo` so the caller can post a pool message about
/// the swap.
#[allow(clippy::too_many_arguments)]
pub async fn replace_blocked_todo(
    store: &PoolStore,
    sink: Arc<dyn PoolEventSink>,
    subagent: Arc<dyn SubagentRuntime>,
    cfg: &CoordinatorConfig,
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
        anyhow::bail!("Replacement task cannot be empty.");
    }
    if reason.is_empty() {
        anyhow::bail!("Replacement reason cannot be empty.");
    }

    let todo_lookup = todo_id.to_string();
    let original = store
        .read(move |db| db.get_koi_todo(&todo_lookup))
        .await?
        .ok_or_else(|| anyhow::anyhow!("Todo '{}' not found", todo_id))?;
    if matches!(original.status.as_str(), "done" | "cancelled") {
        anyhow::bail!(
            "Todo '{}' is '{}' and cannot be replaced.",
            original.id,
            original.status
        );
    }

    let owner_lookup = new_owner_id.to_string();
    let new_owner = store
        .read(move |db| db.resolve_koi_identifier(&owner_lookup))
        .await?
        .ok_or_else(|| anyhow::anyhow!("Koi '{}' not found", new_owner_id))?;

    let pool_session = match original.pool_session_id.as_deref() {
        Some(pid) => {
            let pid = pid.to_string();
            let lookup = pid.clone();
            let pool = store
                .read(move |db| db.resolve_pool_session_identifier(&lookup))
                .await?
                .ok_or_else(|| anyhow::anyhow!("Pool '{}' not found", pid))?;
            ensure_pool_allows_runtime_work(&pool, "replace Koi todo")?;
            Some(pool)
        }
        None => None,
    };

    let replacement_description =
        format!("Replacement for '{}' because: {}", original.title, reason);
    let replacement = {
        let original = original.clone();
        let new_owner_id = new_owner.id.clone();
        let task = task.to_string();
        let desc = replacement_description.clone();
        let triggered_by = triggered_by.to_string();
        let source_type = todo_source_type(&triggered_by);
        let reason = reason.to_string();
        store
            .write(move |db| {
                db.replace_koi_todo(
                    &original,
                    &new_owner_id,
                    &task,
                    &desc,
                    &triggered_by,
                    source_type,
                    &reason,
                    task_timeout_secs,
                )
            })
            .await?
    };

    sink.as_ref().emit_pool(&PoolEvent::TodoChanged {
        pool_id: pool_session
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_default(),
        action: TodoChangeAction::Cancelled,
        todo: (&original).into(),
    });
    sink.as_ref().emit_pool(&PoolEvent::TodoChanged {
        pool_id: pool_session
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_default(),
        action: TodoChangeAction::Replaced,
        todo: (&replacement).into(),
    });

    if let Some(pool) = &pool_session {
        let content = format!(
            "[Task Replaced] '{}' was replaced by '{}' for {}. Reason: {}",
            original.title, replacement.title, new_owner.name, reason
        );
        let meta = json!({
            "todo_id": original.id,
            "replacement_todo_id": replacement.id,
            "new_owner_id": new_owner.id,
        })
        .to_string();
        let msg = insert_ext_message(
            store,
            &pool.id,
            triggered_by,
            &content,
            "status_update",
            &meta,
            Some(&original.id),
            None,
            Some("task_replaced"),
        )
        .await?;
        sink.as_ref().emit_pool(&PoolEvent::MessageAppended {
            pool_id: pool.id.clone(),
            message: (&msg).into(),
        });
    }

    let store_cl = store.clone();
    let sink_cl = sink.clone();
    let subagent_cl = subagent.clone();
    let cfg_cl = cfg.clone();
    let owner_id = new_owner.id.clone();
    let replacement_id = replacement.id.clone();
    let session_id = format!(
        "koi_runtime_{}_{}",
        new_owner.id,
        pool_session
            .as_ref()
            .map(|p| p.id.as_str())
            .unwrap_or("default")
    );
    let pool_id_for_task = pool_session.as_ref().map(|p| p.id.clone());
    tokio::spawn(async move {
        let args = ExecuteTodoArgs {
            koi_id: owner_id,
            todo_id: replacement_id.clone(),
            assign_msg_id: None,
            session_id,
            extra_tool_profile: Vec::new(),
            extra_system_context: None,
        };
        if let Err(e) = execute_todo_turn(&store_cl, sink_cl, subagent_cl, &cfg_cl, args).await {
            tracing::warn!(
                target: "pool::coordinator",
                pool_id = ?pool_id_for_task,
                todo_id = %replacement_id,
                "replace_blocked_todo execution failed: {e}"
            );
        }
    });

    Ok(replacement)
}

fn todo_source_type(actor: &str) -> &'static str {
    match actor {
        "pisci" => "pisci",
        "user" => "user",
        "system" => "system",
        _ => "koi",
    }
}

// ─── handle_mention ───────────────────────────────────────────────────

/// Fan-out when a pool chat message mentions one or more Kois. Creates
/// a todo for each targeted Koi (if they don't already have one for
/// this pool) and fires `execute_todo_turn` for each so they wake up.
///
/// In Phase 1 this code lived in `KoiRuntime::handle_mention` and
/// reached for in-memory mpsc session channels to inject notifications
/// into busy Koi runs. Because Phase 2 turns each Koi run into a
/// one-shot subprocess, there are no persistent sessions to inject
/// into; every mention simply spawns a fresh subagent turn (deduped
/// against existing `todo` / `in_progress` todos for the same Koi).
pub async fn handle_mention(
    store: &PoolStore,
    sink: Arc<dyn PoolEventSink>,
    subagent: Arc<dyn SubagentRuntime>,
    cfg: &CoordinatorConfig,
    sender_id: &str,
    pool_session_id: &str,
    content: &str,
) -> anyhow::Result<()> {
    let kois = store.read(|db| db.list_kois()).await.unwrap_or_default();

    let mention_all = content.contains("@all");
    let sender_id_owned = sender_id.to_string();
    for koi in &kois {
        if koi.status == "offline" || koi.id == sender_id_owned {
            continue;
        }
        let mention = format!("@{}", koi.name);
        if !mention_all && !content.contains(&mention) {
            continue;
        }

        // Ensure there's an active todo for this Koi. Reuse an existing
        // `todo` / `in_progress` one; otherwise synthesise a todo from
        // the mention content.
        let existing = find_active_todo_for_koi(store, &koi.id, pool_session_id).await;
        let (todo, synthesised) = match existing {
            Some(t) => (t, false),
            None => {
                let owner = koi.id.clone();
                let title: String = content.chars().take(120).collect();
                let desc = content.to_string();
                let assigned_by = sender_id.to_string();
                let pool_id = pool_session_id.to_string();
                let todo = store
                    .write(move |db| {
                        db.create_koi_todo(
                            &owner,
                            &title,
                            &desc,
                            "medium",
                            &assigned_by,
                            Some(&pool_id),
                            "mention",
                            None,
                            0,
                        )
                    })
                    .await?;
                sink.as_ref().emit_pool(&PoolEvent::TodoChanged {
                    pool_id: pool_session_id.to_string(),
                    action: TodoChangeAction::Created,
                    todo: (&todo).into(),
                });
                (todo, true)
            }
        };

        // Only spawn a turn when the todo is not already being worked
        // on — if it's `in_progress` somebody else is already on it.
        if !synthesised && todo.status == "in_progress" {
            continue;
        }

        let store_cl = store.clone();
        let subagent_cl = subagent.clone();
        let cfg_cl = cfg.clone();
        let owner_id = koi.id.clone();
        let todo_id = todo.id.clone();
        let session_id = format!("koi_runtime_{}_{}", koi.id, pool_session_id);
        let sink_cl = sink.clone();
        tokio::spawn(async move {
            let args = ExecuteTodoArgs {
                koi_id: owner_id.clone(),
                todo_id: todo_id.clone(),
                assign_msg_id: None,
                session_id,
                extra_tool_profile: Vec::new(),
                extra_system_context: None,
            };
            if let Err(e) = execute_todo_turn(&store_cl, sink_cl, subagent_cl, &cfg_cl, args).await
            {
                tracing::warn!(
                    target: "pool::coordinator",
                    koi_id = %owner_id,
                    todo_id = %todo_id,
                    "handle_mention dispatch failed: {e}"
                );
            }
        });
    }

    Ok(())
}

async fn find_active_todo_for_koi(
    store: &PoolStore,
    koi_id: &str,
    pool_session_id: &str,
) -> Option<KoiTodo> {
    let koi_id = koi_id.to_string();
    let psid = pool_session_id.to_string();
    store
        .read(move |db| db.list_koi_todos(Some(&koi_id)))
        .await
        .ok()?
        .into_iter()
        .find(|t| {
            t.pool_session_id.as_deref() == Some(psid.as_str())
                && matches!(t.status.as_str(), "todo" | "in_progress")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pisci_core::host::NullPoolEventSink;

    #[test]
    fn render_execute_prompt_substitutes_all_fields() {
        let koi = KoiDefinition {
            id: "koi-alpha".into(),
            name: "Alpha".into(),
            role: "worker".into(),
            icon: "🐟".into(),
            color: "#abc".into(),
            system_prompt: "sys".into(),
            description: "".into(),
            status: "idle".into(),
            llm_provider_id: None,
            max_iterations: 0,
            task_timeout_secs: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let todo = KoiTodo {
            id: "aaaaaaaa-bbbb".into(),
            owner_id: koi.id.clone(),
            title: "fix the thing".into(),
            description: "".into(),
            status: "todo".into(),
            priority: "medium".into(),
            assigned_by: "pisci".into(),
            pool_session_id: None,
            claimed_by: None,
            claimed_at: None,
            depends_on: None,
            blocked_reason: None,
            result_message_id: None,
            source_type: "koi".into(),
            task_timeout_secs: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let text = render_execute_prompt(&koi, &todo);
        assert!(text.contains("fix the thing"));
        assert!(text.contains("Alpha"));
        assert!(text.contains("aaaaaaaa"));
        // Short form is the first 8 chars only.
        assert!(!text.contains("aaaaaaaa-bbbb"));
    }

    #[test]
    fn timeout_picks_most_specific_override() {
        let mut koi = KoiDefinition {
            id: "koi".into(),
            name: "K".into(),
            role: "".into(),
            icon: "".into(),
            color: "".into(),
            system_prompt: "".into(),
            description: "".into(),
            status: "idle".into(),
            llm_provider_id: None,
            max_iterations: 0,
            task_timeout_secs: 90,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert_eq!(koi_timeout_for_todo(&koi, 0, 600), 90);
        assert_eq!(koi_timeout_for_todo(&koi, 30, 600), 30);
        koi.task_timeout_secs = 0;
        assert_eq!(koi_timeout_for_todo(&koi, 0, 600), 600);
    }

    #[test]
    fn null_sink_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NullPoolEventSink>();
    }

    #[test]
    fn source_type_buckets() {
        assert_eq!(todo_source_type("pisci"), "pisci");
        assert_eq!(todo_source_type("user"), "user");
        assert_eq!(todo_source_type("system"), "system");
        assert_eq!(todo_source_type("koi-alpha"), "koi");
    }
}
