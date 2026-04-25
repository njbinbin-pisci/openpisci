//! Pool business logic.
//!
//! Every mutating service follows the same shape:
//!
//! 1. Resolve the pool (if the action needs one).
//! 2. Validate caller permissions.
//! 3. Persist the change through [`PoolStore`].
//! 4. Emit zero or more [`PoolEvent`]s through the supplied sink.
//! 5. Return a `Value` the tool layer can format into a user-facing
//!    string (`ToolResult::ok(...)`).
//!
//! Services NEVER touch the filesystem, subprocesses, or host-specific
//! state directly — git operations go through [`crate::pool::git`],
//! and Koi-turn orchestration goes through
//! [`crate::pool::coordinator`] (backed by a host-supplied
//! [`pisci_core::host::SubagentRuntime`]).

use super::coordinator::{self, CoordinatorConfig};
use super::git::{self, GitInitOutcome, MergeOutcome};
use super::metadata;
use super::model::*;
use super::session_source;
use super::store::PoolStore;

use pisci_core::host::{PoolEvent, PoolEventSink, SubagentRuntime, TodoChangeAction};
use pisci_core::models::{KoiTodo, PoolMessage, PoolSession};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

// ─── helpers ────────────────────────────────────────────────────────────

async fn resolve_pool(
    store: &PoolStore,
    caller: &CallerContext<'_>,
    pool_id_hint: &str,
    action: &str,
) -> anyhow::Result<PoolSession> {
    let requested = if !pool_id_hint.trim().is_empty() && pool_id_hint.trim() != "current" {
        Some(pool_id_hint.trim().to_string())
    } else {
        caller.pool_session_id.map(|s| s.to_string())
    };
    let id = match requested {
        Some(id) => id,
        None => anyhow::bail!("'pool_id' is required for action '{}'", action),
    };
    match store
        .read(move |db| db.resolve_pool_session_identifier(&id))
        .await?
    {
        Some(s) => Ok(s),
        None => anyhow::bail!("Pool '{}' not found", pool_id_hint),
    }
}

fn ensure_accepts_new_work(pool: &PoolSession, action: &str) -> anyhow::Result<()> {
    if pool.status == "active" {
        return Ok(());
    }
    anyhow::bail!(
        "Pool '{}' is {}. Action '{}' is disabled until the pool is resumed.",
        pool.name,
        pool.status,
        action
    )
}

async fn find_todo_by_prefix(store: &PoolStore, prefix: &str) -> anyhow::Result<Option<KoiTodo>> {
    let p = prefix.to_string();
    store
        .read(move |db| {
            let todos = db.list_koi_todos(None)?;
            Ok(todos.into_iter().find(|t| t.id.starts_with(&p)))
        })
        .await
}

fn emit_todo(
    sink: &dyn PoolEventSink,
    pool_id: Option<&str>,
    action: TodoChangeAction,
    todo: &KoiTodo,
) {
    let pool_id = pool_id
        .map(str::to_string)
        .or_else(|| todo.pool_session_id.clone())
        .unwrap_or_default();
    sink.emit_pool(&PoolEvent::TodoChanged {
        pool_id,
        action,
        todo: todo.into(),
    });
}

fn emit_message(sink: &dyn PoolEventSink, msg: &PoolMessage) {
    sink.emit_pool(&PoolEvent::MessageAppended {
        pool_id: msg.pool_session_id.clone(),
        message: msg.into(),
    });
}

fn check_todo_ownership(todo: &KoiTodo, caller: &CallerContext<'_>) -> anyhow::Result<()> {
    if caller.is_pisci() || todo.owner_id == caller.memory_owner_id {
        return Ok(());
    }
    anyhow::bail!(
        "Permission denied. You can only manage your own todos. This todo belongs to '{}'. \
         To cancel or modify another agent's task, @pisci in pool_chat to request it.",
        todo.owner_id
    )
}

fn check_pisci_only(caller: &CallerContext<'_>, action: &str) -> anyhow::Result<()> {
    if caller.is_pisci() {
        return Ok(());
    }
    anyhow::bail!(
        "Permission denied. Action '{}' is restricted to pisci because it permanently deletes shared board history.",
        action
    )
}

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

// ─── pool CRUD ──────────────────────────────────────────────────────────

/// Create a new pool. Returns a JSON object:
/// `{ "pool": PoolSessionSnapshot, "git_info": "...", "summary": "..." }`.
pub async fn create_pool(
    store: &PoolStore,
    sink: &dyn PoolEventSink,
    caller: &CallerContext<'_>,
    args: CreatePoolArgs,
) -> anyhow::Result<Value> {
    let name = args.name.trim();
    if name.is_empty() {
        anyhow::bail!("'name' is required for action 'create'");
    }
    let project_dir = args
        .project_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let org_spec = args.org_spec.unwrap_or_default();
    let org_spec_trimmed = org_spec.trim();

    let mut git_info = String::new();
    if let Some(dir) = project_dir.as_deref() {
        match git::ensure_git_repo(Path::new(dir), caller.cancel.clone()).await {
            Ok(GitInitOutcome::Initialised) => {
                git_info = format!("\nGit: initialized at {}", dir);
            }
            Ok(GitInitOutcome::AlreadyInitialised) => {
                git_info = format!("\nGit: existing repo at {}", dir);
            }
            Err(e) => {
                if e.to_string().contains("Operation cancelled") {
                    anyhow::bail!("已被用户取消");
                }
                anyhow::bail!("Failed to initialise git: {}", e);
            }
        }
    }

    let name_owned = name.to_string();
    let project_dir_clone = project_dir.clone();
    let task_timeout_secs = args.task_timeout_secs;
    let session = store
        .write(move |db| {
            db.create_pool_session_with_dir(
                &name_owned,
                project_dir_clone.as_deref(),
                task_timeout_secs,
            )
        })
        .await?;

    if !org_spec_trimmed.is_empty() {
        let id = session.id.clone();
        let spec = org_spec_trimmed.to_string();
        store
            .write(move |db| db.update_pool_org_spec(&id, &spec))
            .await?;
    }

    if let Some(binding_key) = args
        .origin_im_binding_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let id = session.id.clone();
        let key = binding_key.to_string();
        store
            .write(move |db| db.set_pool_origin_im_binding(&id, Some(&key)))
            .await?;
    }

    let welcome_text = format!(
        "项目池「{}」已创建。{}{}",
        name,
        if org_spec_trimmed.is_empty() {
            "尚未设定组织规范。"
        } else {
            "组织规范已就绪。"
        },
        if project_dir.is_some() {
            " Git 仓库已初始化，Koi 将使用独立 worktree 工作。"
        } else {
            ""
        }
    );
    let pool_id_for_msg = session.id.clone();
    let content = welcome_text.clone();
    let welcome = store
        .write(move |db| {
            db.insert_pool_message_ext(
                &pool_id_for_msg,
                "pisci",
                &content,
                "status_update",
                &json!({ "event": "pool_created" }).to_string(),
                None,
                None,
                Some("pool_created"),
            )
        })
        .await?;

    sink.emit_pool(&PoolEvent::PoolCreated {
        pool: (&session).into(),
    });
    emit_message(sink, &welcome);
    let _ = caller;

    Ok(json!({
        "pool": pisci_core::host::PoolSessionSnapshot::from(&session),
        "git_info": git_info,
        "summary": format!(
            "Project pool created.\nID: {}\nName: {}\nOrg Spec: {}{}",
            session.id,
            name,
            if org_spec_trimmed.is_empty() { "not set (use 'update' to add one)" } else { "set" },
            git_info
        ),
    }))
}

pub async fn read_org_spec(
    store: &PoolStore,
    caller: &CallerContext<'_>,
    pool_id_hint: &str,
) -> anyhow::Result<Value> {
    let session = resolve_pool(store, caller, pool_id_hint, "read").await?;
    Ok(json!({
        "pool_id": session.id,
        "name": session.name,
        "org_spec": session.org_spec,
    }))
}

pub async fn update_org_spec(
    store: &PoolStore,
    sink: &dyn PoolEventSink,
    caller: &CallerContext<'_>,
    args: UpdateOrgSpecArgs,
) -> anyhow::Result<Value> {
    let spec_trimmed = args.org_spec.as_deref().map(str::trim).unwrap_or("");
    let timeout = args.task_timeout_secs;
    if spec_trimmed.is_empty() && timeout.is_none() {
        anyhow::bail!(
            "Provide at least one field to update: 'org_spec' and/or 'task_timeout_secs'."
        );
    }

    let session = resolve_pool(store, caller, &args.pool_id, "update").await?;

    let id = session.id.clone();
    let spec = spec_trimmed.to_string();
    if !spec.is_empty() {
        store
            .write(move |db| db.update_pool_org_spec(&id, &spec))
            .await?;
    }
    if timeout.is_some() {
        let id = session.id.clone();
        store
            .write(move |db| db.update_pool_session_config(&id, timeout))
            .await?;
    }

    let id = session.id.clone();
    let msg = store
        .write(move |db| {
            db.insert_pool_message_ext(
                &id,
                "pisci",
                "组织规范已更新。",
                "status_update",
                &json!({ "event": "org_spec_updated" }).to_string(),
                None,
                None,
                Some("org_spec_updated"),
            )
        })
        .await?;

    let fresh = store
        .read({
            let id = session.id.clone();
            move |db| db.get_pool_session(&id)
        })
        .await?
        .unwrap_or(session);

    sink.emit_pool(&PoolEvent::PoolUpdated {
        pool: (&fresh).into(),
    });
    emit_message(sink, &msg);

    Ok(json!({
        "pool": pisci_core::host::PoolSessionSnapshot::from(&fresh),
        "summary": format!("Pool '{}' ({}) updated.", fresh.name, fresh.id),
    }))
}

pub async fn list_pools(store: &PoolStore) -> anyhow::Result<Value> {
    let (sessions, kois) = store
        .read(|db| {
            let sessions = db.list_pool_sessions()?;
            let kois = db.list_kois().unwrap_or_default();
            Ok((sessions, kois))
        })
        .await?;

    Ok(json!({
        "sessions": sessions
            .iter()
            .map(pisci_core::host::PoolSessionSnapshot::from)
            .collect::<Vec<_>>(),
        "raw_sessions": sessions,
        "kois": kois,
    }))
}

pub async fn find_related(store: &PoolStore, keywords: &str) -> anyhow::Result<Value> {
    let k = keywords.trim();
    if k.is_empty() {
        anyhow::bail!("'keywords' is required for action 'find_related'");
    }
    let keywords = k.to_string();
    let results = store
        .read(move |db| db.find_related_pool_sessions(&keywords))
        .await?;
    Ok(json!({ "sessions": results }))
}

// ─── status transitions ────────────────────────────────────────────────

/// Change a pool's status (pause / resume / archive). Returns
/// `{ "pool", "old_status", "new_status", "summary" }`.
///
/// The service does NOT cancel in-flight Koi runs itself — hosts that
/// need to do that subscribe to [`PoolEvent::PoolArchived`] /
/// [`PoolEvent::PoolPaused`] and terminate their own task handles.
pub async fn set_pool_status(
    store: &PoolStore,
    sink: &dyn PoolEventSink,
    caller: &CallerContext<'_>,
    pool_id_hint: &str,
    new_status: &str,
) -> anyhow::Result<Value> {
    let action = match new_status {
        "paused" => "pause",
        "active" => "resume",
        "archived" => "archive",
        other => anyhow::bail!("unsupported status transition: {}", other),
    };
    let session = resolve_pool(store, caller, pool_id_hint, action).await?;

    if session.status == new_status {
        return Ok(json!({
            "pool": pisci_core::host::PoolSessionSnapshot::from(&session),
            "no_op": true,
            "summary": format!("Project '{}' is already {}.", session.name, new_status),
        }));
    }

    if new_status == "archived" {
        if let Some(src) = caller.session_source {
            if session_source::is_heartbeat_like(src) {
                anyhow::bail!(
                    "Heartbeat sessions may not archive projects automatically. \
                     Leave the pool active and wait for explicit user confirmation."
                );
            }
        }
        let id = session.id.clone();
        let active = store
            .read(move |db| {
                let todos = db.list_koi_todos(None)?;
                Ok(todos
                    .into_iter()
                    .filter(|t| {
                        t.pool_session_id.as_deref() == Some(&id)
                            && !matches!(t.status.as_str(), "done" | "cancelled")
                    })
                    .collect::<Vec<_>>())
            })
            .await?;
        if !active.is_empty() {
            let preview = active
                .iter()
                .take(3)
                .map(|t| format!("{} [{}]", short(&t.id), t.status))
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!(
                "Pool '{}' still has {} active todo(s): {}. Finish, block, or cancel them first.",
                session.name,
                active.len(),
                preview
            );
        }
    }

    let old_status = session.status.clone();
    let id = session.id.clone();
    let status_owned = new_status.to_string();
    store
        .write(move |db| db.update_pool_session_status(&id, &status_owned))
        .await?;

    let id = session.id.clone();
    let old_status_owned = old_status.clone();
    let status_owned = new_status.to_string();
    let msg = store
        .write(move |db| {
            db.insert_pool_message_ext(
                &id,
                "pisci",
                &format!("项目状态变更: {} → {}", old_status_owned, status_owned),
                "status_update",
                &json!({
                    "event": "status_changed",
                    "old": old_status_owned,
                    "new": status_owned,
                })
                .to_string(),
                None,
                None,
                Some("status_changed"),
            )
        })
        .await?;

    let fresh = store
        .read({
            let id = session.id.clone();
            move |db| db.get_pool_session(&id)
        })
        .await?
        .unwrap_or(session.clone());
    let snap: pisci_core::host::PoolSessionSnapshot = (&fresh).into();

    let event = match new_status {
        "paused" => PoolEvent::PoolPaused { pool: snap.clone() },
        "active" => PoolEvent::PoolResumed { pool: snap.clone() },
        "archived" => PoolEvent::PoolArchived {
            pool_id: fresh.id.clone(),
        },
        _ => PoolEvent::PoolUpdated { pool: snap.clone() },
    };
    sink.emit_pool(&event);
    emit_message(sink, &msg);

    let label = match new_status {
        "paused" => "已暂停",
        "archived" => "已归档",
        "active" => "已恢复",
        _ => new_status,
    };
    Ok(json!({
        "pool": snap,
        "old_status": old_status,
        "new_status": new_status,
        "summary": format!(
            "Project '{}' {} (status: {} → {}).",
            fresh.name, label, old_status, new_status
        ),
    }))
}

// ─── messages ──────────────────────────────────────────────────────────

/// Append a new pool message. Enriches metadata with coordination
/// signals + `@pisci` mentions, inserts into DB, emits
/// `MessageAppended`, and—when `@` appears in the content and a
/// `SubagentRuntime` is available—fires
/// [`coordinator::handle_mention`] to wake the mentioned Kois as
/// subprocess turns.
pub async fn send_pool_message(
    store: &PoolStore,
    sink: Arc<dyn PoolEventSink>,
    subagent: Option<Arc<dyn SubagentRuntime>>,
    cfg: &CoordinatorConfig,
    caller: &CallerContext<'_>,
    args: SendPoolMessageArgs,
) -> anyhow::Result<PoolMessage> {
    let session = resolve_pool(store, caller, &args.pool_id, "send").await?;
    if session.status != "active" {
        anyhow::bail!(
            "Pool '{}' is {}. Messages are disabled until it is resumed.",
            session.name,
            session.status
        );
    }
    let content = args.content.trim();
    if content.is_empty() {
        anyhow::bail!("'content' must not be empty");
    }

    let metadata = metadata::enrich_as_json_string(json!({}), content);
    let event_type = metadata::coordination_event_type_for_content(content).map(str::to_string);

    let pool_id = session.id.clone();
    let sender = args.sender_id.clone();
    let body = content.to_string();
    let reply_to = args.reply_to_message_id;
    let msg = store
        .write(move |db| {
            db.insert_pool_message_ext(
                &pool_id,
                &sender,
                &body,
                "text",
                &metadata,
                None,
                reply_to,
                event_type.as_deref(),
            )
        })
        .await?;

    emit_message(sink.as_ref(), &msg);

    if content.contains('@') {
        if let Some(sub) = subagent {
            if let Err(e) = coordinator::handle_mention(
                store,
                sink.clone(),
                sub,
                cfg,
                &args.sender_id,
                &session.id,
                content,
            )
            .await
            {
                tracing::warn!(
                    target: "pool::services",
                    pool_id = %session.id,
                    sender = %args.sender_id,
                    "coordinator::handle_mention failed: {e}"
                );
            }
        }
    }
    Ok(msg)
}

pub async fn read_pool_messages(
    store: &PoolStore,
    caller: &CallerContext<'_>,
    pool_id_hint: &str,
    limit: i64,
) -> anyhow::Result<Value> {
    let session = resolve_pool(store, caller, pool_id_hint, "read").await?;
    let limit = if limit <= 0 { 20 } else { limit };
    let id = session.id.clone();
    let messages = store
        .read(move |db| db.get_pool_messages(&id, limit, 0))
        .await?;
    let kois = store.read(|db| db.list_kois()).await.unwrap_or_default();
    Ok(json!({
        "pool": pisci_core::host::PoolSessionSnapshot::from(&session),
        "messages": messages,
        "kois": kois,
    }))
}

pub async fn get_pool_messages(
    store: &PoolStore,
    caller: &CallerContext<'_>,
    pool_id_hint: &str,
    limit: i64,
) -> anyhow::Result<Value> {
    read_pool_messages(store, caller, pool_id_hint, limit).await
}

pub async fn get_pool_todos(
    store: &PoolStore,
    caller: &CallerContext<'_>,
    pool_id_hint: &str,
) -> anyhow::Result<Value> {
    let session = resolve_pool(store, caller, pool_id_hint, "get_todos").await?;
    let id = session.id.clone();
    let todos = store
        .read(move |db| {
            let all = db.list_koi_todos(None)?;
            Ok(all
                .into_iter()
                .filter(|t| t.pool_session_id.as_deref() == Some(&id))
                .collect::<Vec<_>>())
        })
        .await?;
    Ok(json!({
        "pool_id": session.id,
        "todos": todos,
    }))
}

// ─── assignment & todos ────────────────────────────────────────────────

pub async fn assign_koi(
    store: &PoolStore,
    sink: Arc<dyn PoolEventSink>,
    subagent: Option<Arc<dyn SubagentRuntime>>,
    cfg: &CoordinatorConfig,
    caller: &CallerContext<'_>,
    args: AssignKoiArgs,
) -> anyhow::Result<Value> {
    let session = resolve_pool(store, caller, &args.pool_id, "assign_koi").await?;
    ensure_accepts_new_work(&session, "assign_koi")?;

    let koi_id = args.koi_id.trim().to_string();
    let task = args.task.trim().to_string();
    if koi_id.is_empty() {
        anyhow::bail!("'koi_id' is required for action 'assign_koi'");
    }
    if task.is_empty() {
        anyhow::bail!("'task' is required for action 'assign_koi'");
    }
    let priority = if args.priority.trim().is_empty() {
        "medium".to_string()
    } else {
        args.priority.clone()
    };

    // Best-effort resolve the Koi display name so the mention looks
    // nice in the pool_chat log.
    let koi_name = {
        let lookup = koi_id.clone();
        store
            .read(move |db| db.resolve_koi_identifier(&lookup))
            .await
            .ok()
            .flatten()
            .map(|k| k.name)
            .unwrap_or_else(|| koi_id.clone())
    };

    let mention = if args.timeout_secs > 0 {
        format!(
            "@!{} [Priority: {}] [Execution timeout: {}s] {}",
            koi_name, priority, args.timeout_secs, task
        )
    } else {
        format!("@!{} [Priority: {}] {}", koi_name, priority, task)
    };

    // Pre-create the kanban todo so the board shows the work before
    // the Koi actually wakes up.
    let todo = {
        let owner = koi_id.clone();
        let assigned_by = "pisci".to_string();
        let pool_id = session.id.clone();
        let title: String = task.chars().take(120).collect();
        let desc = task.clone();
        let prio = priority.clone();
        let timeout = args.timeout_secs;
        store
            .write(move |db| {
                db.create_koi_todo(
                    &owner,
                    &title,
                    &desc,
                    &prio,
                    &assigned_by,
                    Some(&pool_id),
                    "koi",
                    None,
                    timeout,
                )
            })
            .await?
    };
    emit_todo(
        sink.as_ref(),
        Some(&session.id),
        TodoChangeAction::Created,
        &todo,
    );

    let meta = json!({
        "target_koi": &koi_id,
        "priority": &priority,
        "timeout_secs": args.timeout_secs,
        "todo_id": &todo.id,
    });
    let pool_id = session.id.clone();
    let mention_clone = mention.clone();
    let meta_str = meta.to_string();
    let msg = store
        .write(move |db| {
            db.insert_pool_message(&pool_id, "pisci", &mention_clone, "mention", &meta_str)
        })
        .await?;
    emit_message(sink.as_ref(), &msg);

    sink.as_ref().emit_pool(&PoolEvent::KoiAssigned {
        pool_id: session.id.clone(),
        koi_id: koi_id.clone(),
        todo_id: todo.id.clone(),
    });

    // Wake the target Koi via the subagent runtime (fire-and-forget;
    // the coordinator spawns its own tokio task so the mention path
    // doesn't stall the tool response).
    if let Some(sub) = subagent {
        if let Err(e) = coordinator::handle_mention(
            store,
            sink.clone(),
            sub,
            cfg,
            "pisci",
            &session.id,
            &mention,
        )
        .await
        {
            tracing::warn!(
                target: "pool::services",
                pool_id = %session.id,
                koi_id = %koi_id,
                "assign_koi handle_mention failed: {e}"
            );
        }
    }

    Ok(json!({
        "pool_id": session.id,
        "koi_id": koi_id,
        "koi_name": koi_name,
        "todo": pisci_core::host::TodoSnapshot::from(&todo),
        "mention_message": msg,
        "summary": format!(
            "Task posted to pool, kanban todo created, and @!{} has been delegated work.",
            koi_name
        ),
    }))
}

pub async fn create_todo(
    store: &PoolStore,
    sink: &dyn PoolEventSink,
    caller: &CallerContext<'_>,
    args: CreateTodoArgs,
) -> anyhow::Result<Value> {
    let pool_id_hint = args.pool_id.clone();
    let session = resolve_pool(store, caller, &pool_id_hint, "create_todo").await?;
    ensure_accepts_new_work(&session, "create_todo")?;

    let title = args.title.trim().to_string();
    if title.is_empty() {
        anyhow::bail!("'title' is required for action 'create_todo'");
    }
    let priority = if args.priority.trim().is_empty() {
        "medium".to_string()
    } else {
        args.priority.clone()
    };
    let description = args.description.clone();
    let owner = caller.memory_owner_id.to_string();
    let pool_id = session.id.clone();
    let timeout_secs = args.timeout_secs;

    let todo = store
        .write(move |db| {
            db.create_koi_todo(
                &owner,
                &title,
                &description,
                &priority,
                &owner,
                Some(&pool_id),
                "koi",
                None,
                timeout_secs,
            )
        })
        .await?;

    emit_todo(sink, Some(&session.id), TodoChangeAction::Created, &todo);

    Ok(json!({
        "todo": pisci_core::host::TodoSnapshot::from(&todo),
        "summary": format!(
            "Todo '{}' created with ID `{}`.",
            todo.title,
            short(&todo.id)
        ),
    }))
}

pub async fn claim_todo(
    store: &PoolStore,
    sink: &dyn PoolEventSink,
    caller: &CallerContext<'_>,
    todo_id: &str,
) -> anyhow::Result<Value> {
    let todo = find_todo_by_prefix(store, todo_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Todo '{}' not found", todo_id))?;

    if matches!(todo.status.as_str(), "done" | "cancelled") {
        anyhow::bail!("Cannot claim a todo with status '{}'.", todo.status);
    }
    if todo.claimed_by.is_some() && todo.claimed_by.as_deref() != Some(caller.memory_owner_id) {
        anyhow::bail!(
            "Todo '{}' is already claimed by '{}'.",
            short(&todo.id),
            todo.claimed_by.as_deref().unwrap_or("unknown")
        );
    }
    if !caller.is_pisci() && todo.owner_id != caller.memory_owner_id {
        anyhow::bail!(
            "Permission denied. You can only claim your own todos. This todo belongs to '{}'.",
            todo.owner_id
        );
    }

    let id = todo.id.clone();
    let claimed_by = caller.memory_owner_id.to_string();
    store
        .write(move |db| db.claim_koi_todo(&id, &claimed_by))
        .await?;

    if let Some(ref psid) = todo.pool_session_id {
        let psid = psid.clone();
        let owner = caller.memory_owner_id.to_string();
        let tid = todo.id.clone();
        let title = todo.title.clone();
        if let Ok(msg) = store
            .write(move |db| {
                db.insert_pool_message_ext(
                    &psid,
                    &owner,
                    &format!("接受了任务: {}", title),
                    "task_claimed",
                    "{}",
                    Some(&tid),
                    None,
                    Some("task_claimed"),
                )
            })
            .await
        {
            emit_message(sink, &msg);
        }
    }

    let refreshed = store
        .read({
            let id = todo.id.clone();
            move |db| db.get_koi_todo(&id)
        })
        .await?
        .unwrap_or(todo.clone());
    emit_todo(
        sink,
        refreshed.pool_session_id.as_deref(),
        TodoChangeAction::Claimed,
        &refreshed,
    );

    Ok(json!({
        "todo": pisci_core::host::TodoSnapshot::from(&refreshed),
        "summary": format!(
            "Todo '{}' ({}) claimed. Status is now in_progress.",
            short(&refreshed.id),
            refreshed.title
        ),
    }))
}

pub async fn complete_todo(
    store: &PoolStore,
    sink: &dyn PoolEventSink,
    caller: &CallerContext<'_>,
    todo_id: &str,
    summary: &str,
) -> anyhow::Result<Value> {
    if summary.trim().is_empty() {
        anyhow::bail!("'summary' is required for action 'complete_todo'");
    }
    let todo = find_todo_by_prefix(store, todo_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Todo '{}' not found", todo_id))?;

    if todo.status == "done" {
        return Ok(json!({
            "no_op": true,
            "summary": format!("Todo '{}' is already completed.", short(&todo.id)),
        }));
    }
    if todo.status == "cancelled" {
        anyhow::bail!("Cannot complete a cancelled todo.");
    }
    check_todo_ownership(&todo, caller)?;

    let result_msg_id = if let Some(ref psid) = todo.pool_session_id {
        let psid = psid.clone();
        let owner = caller.memory_owner_id.to_string();
        let tid = todo.id.clone();
        let owner_id_for_meta = todo.owner_id.clone();
        let summary_owned = summary.to_string();
        let summary_for_meta = summary_owned.clone();
        match store
            .write(move |db| {
                let metadata = metadata::enrich_pool_message_metadata(
                    json!({
                        "todo": {
                            "id": tid,
                            "owner_id": owner_id_for_meta,
                            "status": "done",
                        }
                    }),
                    &summary_for_meta,
                );
                db.insert_pool_message_ext(
                    &psid,
                    &owner,
                    &summary_owned,
                    "result",
                    &metadata.to_string(),
                    Some(&tid),
                    None,
                    Some("task_completed"),
                )
            })
            .await
        {
            Ok(msg) => {
                emit_message(sink, &msg);
                Some(msg.id)
            }
            Err(_) => None,
        }
    } else {
        None
    };

    let id = todo.id.clone();
    store
        .write(move |db| db.complete_koi_todo(&id, result_msg_id))
        .await?;

    let refreshed = store
        .read({
            let id = todo.id.clone();
            move |db| db.get_koi_todo(&id)
        })
        .await?
        .unwrap_or(todo.clone());
    emit_todo(
        sink,
        refreshed.pool_session_id.as_deref(),
        TodoChangeAction::Completed,
        &refreshed,
    );

    Ok(json!({
        "todo": pisci_core::host::TodoSnapshot::from(&refreshed),
        "summary": format!(
            "Todo '{}' ({}) marked as completed.",
            short(&refreshed.id),
            refreshed.title
        ),
    }))
}

pub async fn cancel_todo(
    store: &PoolStore,
    sink: &dyn PoolEventSink,
    caller: &CallerContext<'_>,
    todo_id: &str,
    reason: &str,
) -> anyhow::Result<Value> {
    let todo = find_todo_by_prefix(store, todo_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Todo '{}' not found", todo_id))?;

    if todo.status == "cancelled" {
        return Ok(json!({
            "no_op": true,
            "summary": format!("Todo '{}' is already cancelled.", short(&todo.id)),
        }));
    }
    if todo.status == "done" {
        anyhow::bail!("Cannot cancel a completed todo.");
    }
    check_todo_ownership(&todo, caller)?;

    let reason = if reason.trim().is_empty() {
        "Cancelled"
    } else {
        reason
    };
    let id = todo.id.clone();
    let reason_owned = reason.to_string();
    store
        .write(move |db| db.cancel_koi_todo(&id, &reason_owned))
        .await?;

    if let Some(ref psid) = todo.pool_session_id {
        let psid = psid.clone();
        let owner = caller.memory_owner_id.to_string();
        let tid = todo.id.clone();
        let owner_id_for_meta = todo.owner_id.clone();
        let reason_owned = reason.to_string();
        let title = todo.title.clone();
        if let Ok(msg) = store
            .write(move |db| {
                let metadata = json!({
                    "todo": {
                        "id": tid,
                        "owner_id": owner_id_for_meta,
                        "status": "cancelled",
                        "reason": reason_owned,
                    }
                });
                db.insert_pool_message_ext(
                    &psid,
                    &owner,
                    &format!("[Task Cancelled] \"{}\" — {}", title, reason_owned),
                    "system",
                    &metadata.to_string(),
                    Some(&tid),
                    None,
                    Some("task_cancelled"),
                )
            })
            .await
        {
            emit_message(sink, &msg);
        }
    }

    let refreshed = store
        .read({
            let id = todo.id.clone();
            move |db| db.get_koi_todo(&id)
        })
        .await?
        .unwrap_or(todo.clone());
    emit_todo(
        sink,
        refreshed.pool_session_id.as_deref(),
        TodoChangeAction::Cancelled,
        &refreshed,
    );

    Ok(json!({
        "todo": pisci_core::host::TodoSnapshot::from(&refreshed),
        "summary": format!(
            "Todo '{}' ({}) cancelled. Reason: {}",
            short(&refreshed.id),
            refreshed.title,
            reason
        ),
    }))
}

pub async fn delete_todo(
    store: &PoolStore,
    sink: &dyn PoolEventSink,
    caller: &CallerContext<'_>,
    args: DeleteTodoArgs,
) -> anyhow::Result<Value> {
    check_pisci_only(caller, "delete_todo")?;

    let todo_id = args
        .todo_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(todo_id) = todo_id {
        let todo = find_todo_by_prefix(store, &todo_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Todo '{}' not found", todo_id))?;
        let delete_id = todo.id.clone();
        store
            .write(move |db| db.delete_koi_todo(&delete_id))
            .await?;
        emit_todo(
            sink,
            todo.pool_session_id.as_deref(),
            TodoChangeAction::Deleted,
            &todo,
        );
        return Ok(json!({
            "deleted_count": 1,
            "deleted_todos": [pisci_core::host::TodoSnapshot::from(&todo)],
            "summary": format!(
                "Deleted todo '{}' ({}).",
                short(&todo.id),
                todo.title
            ),
        }));
    }

    let pool_id_hint = args
        .pool_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "'todo_id' is required for single delete, or provide 'pool_id' for batch delete."
            )
        })?
        .to_string();
    let status_filter = args
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let owner_hint = args
        .owner_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if status_filter.is_none() && owner_hint.is_none() {
        anyhow::bail!(
            "Batch delete requires at least one filter. Provide 'delete_status' and/or 'delete_owner_id'."
        );
    }

    let owner_filter = if let Some(owner_hint) = owner_hint {
        Some(
            store
                .read(move |db| {
                    Ok(db
                        .resolve_koi_identifier(&owner_hint)?
                        .map(|k| k.id)
                        .unwrap_or(owner_hint))
                })
                .await?,
        )
    } else {
        None
    };
    let session = resolve_pool(store, caller, &pool_id_hint, "delete_todo").await?;
    let resolved_pool_id = session.id.clone();
    let status_filter_cl = status_filter.clone();
    let owner_filter_cl = owner_filter.clone();
    let todos_to_delete = store
        .read(move |db| {
            let all = db.list_koi_todos(None)?;
            Ok(all
                .into_iter()
                .filter(|todo| todo.pool_session_id.as_deref() == Some(resolved_pool_id.as_str()))
                .filter(|todo| {
                    status_filter_cl
                        .as_deref()
                        .map(|status| todo.status == status)
                        .unwrap_or(true)
                })
                .filter(|todo| {
                    owner_filter_cl
                        .as_deref()
                        .map(|owner_id| todo.owner_id == owner_id)
                        .unwrap_or(true)
                })
                .collect::<Vec<_>>())
        })
        .await?;

    if todos_to_delete.is_empty() {
        return Ok(json!({
            "deleted_count": 0,
            "deleted_todos": [],
            "summary": format!(
                "No todos matched the delete filter in pool '{}'.",
                short(&session.id)
            ),
        }));
    }

    let delete_ids: Vec<String> = todos_to_delete.iter().map(|todo| todo.id.clone()).collect();
    store
        .write(move |db| {
            for id in delete_ids {
                db.delete_koi_todo(&id)?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await?;
    for todo in &todos_to_delete {
        emit_todo(
            sink,
            todo.pool_session_id.as_deref(),
            TodoChangeAction::Deleted,
            todo,
        );
    }

    Ok(json!({
        "deleted_count": todos_to_delete.len(),
        "deleted_todos": todos_to_delete
            .iter()
            .map(pisci_core::host::TodoSnapshot::from)
            .collect::<Vec<_>>(),
        "summary": format!(
            "Deleted {} todo(s) from pool '{}'{}{}.",
            todos_to_delete.len(),
            session.name,
            status_filter
                .as_deref()
                .map(|status| format!(" with status '{}'", status))
                .unwrap_or_default(),
            owner_filter
                .as_deref()
                .map(|owner_id| format!(" for owner '{}'", owner_id))
                .unwrap_or_default(),
        ),
    }))
}

pub async fn update_todo_status(
    store: &PoolStore,
    sink: &dyn PoolEventSink,
    caller: &CallerContext<'_>,
    args: UpdateTodoStatusArgs,
) -> anyhow::Result<Value> {
    if !matches!(args.new_status.as_str(), "todo" | "in_progress" | "blocked") {
        anyhow::bail!(
            "'status' must be one of: todo, in_progress, blocked. Use 'complete_todo' or 'cancel_todo' for terminal states."
        );
    }
    let todo = find_todo_by_prefix(store, &args.todo_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Todo '{}' not found", args.todo_id))?;

    if matches!(todo.status.as_str(), "done" | "cancelled") {
        anyhow::bail!("Cannot update status of a {} todo.", todo.status);
    }
    check_todo_ownership(&todo, caller)?;

    let id = todo.id.clone();
    let new_status = args.new_status.clone();
    let status_for_update = new_status.clone();
    store
        .write(move |db| db.update_koi_todo(&id, None, None, Some(&status_for_update), None))
        .await?;

    if let Some(ref psid) = todo.pool_session_id {
        let psid = psid.clone();
        let owner = caller.memory_owner_id.to_string();
        let tid = todo.id.clone();
        let owner_id_for_meta = todo.owner_id.clone();
        let new_status_owned = new_status.clone();
        let title = todo.title.clone();
        let event = if new_status == "blocked" {
            Some("task_blocked")
        } else {
            Some("task_status_changed")
        };
        if let Ok(msg) = store
            .write(move |db| {
                let metadata = json!({
                    "todo": {
                        "id": tid,
                        "owner_id": owner_id_for_meta,
                        "status": new_status_owned,
                    }
                });
                db.insert_pool_message_ext(
                    &psid,
                    &owner,
                    &format!("[Task Status] '{}' is now {}.", title, new_status_owned),
                    "status_update",
                    &metadata.to_string(),
                    Some(&tid),
                    None,
                    event,
                )
            })
            .await
        {
            emit_message(sink, &msg);
        }
    }

    let refreshed = store
        .read({
            let id = todo.id.clone();
            move |db| db.get_koi_todo(&id)
        })
        .await?
        .unwrap_or(todo.clone());
    let action = if new_status == "blocked" {
        TodoChangeAction::Blocked
    } else {
        TodoChangeAction::Updated
    };
    emit_todo(
        sink,
        refreshed.pool_session_id.as_deref(),
        action,
        &refreshed,
    );

    Ok(json!({
        "todo": pisci_core::host::TodoSnapshot::from(&refreshed),
        "summary": format!(
            "Todo '{}' ({}) status changed to '{}'.",
            short(&refreshed.id),
            refreshed.title,
            new_status
        ),
    }))
}

pub async fn resume_todo(
    store: &PoolStore,
    sink: Arc<dyn PoolEventSink>,
    subagent: Arc<dyn SubagentRuntime>,
    cfg: &CoordinatorConfig,
    caller: &CallerContext<'_>,
    todo_id: &str,
) -> anyhow::Result<Value> {
    if !caller.is_pisci() {
        anyhow::bail!("Only Pisci may decide whether a blocked task should be resumed.");
    }
    let todo = find_todo_by_prefix(store, todo_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Todo '{}' not found", todo_id))?;

    // The coordinator emits its own Resumed/MessageAppended events from
    // within the spawned turn, so we don't duplicate them here.
    let refreshed = coordinator::resume_blocked_todo(
        store,
        sink,
        subagent,
        cfg,
        &todo.id,
        caller.memory_owner_id,
    )
    .await?;

    Ok(json!({
        "todo": pisci_core::host::TodoSnapshot::from(&refreshed),
        "summary": format!(
            "Resume requested for todo '{}' ({}).",
            short(&refreshed.id),
            refreshed.title
        ),
    }))
}

pub async fn replace_todo(
    store: &PoolStore,
    sink: Arc<dyn PoolEventSink>,
    subagent: Arc<dyn SubagentRuntime>,
    cfg: &CoordinatorConfig,
    caller: &CallerContext<'_>,
    args: ReplaceTodoArgs,
) -> anyhow::Result<Value> {
    if !caller.is_pisci() {
        anyhow::bail!("Only Pisci may replace a task owner.");
    }
    if args.new_owner_id.trim().is_empty() {
        anyhow::bail!("'new_owner_id' is required for action 'replace_todo'");
    }
    if args.task.trim().is_empty() {
        anyhow::bail!("'task' is required for action 'replace_todo'");
    }
    if args.reason.trim().is_empty() {
        anyhow::bail!("'reason' is required for action 'replace_todo'");
    }

    let original = find_todo_by_prefix(store, &args.todo_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Todo '{}' not found", args.todo_id))?;

    // The coordinator emits Cancelled + Replaced events itself, so we
    // don't duplicate them here.
    let replacement = coordinator::replace_blocked_todo(
        store,
        sink,
        subagent,
        cfg,
        &original.id,
        args.new_owner_id.trim(),
        args.task.trim(),
        args.reason.trim(),
        caller.memory_owner_id,
        args.timeout_secs,
    )
    .await?;

    Ok(json!({
        "original": pisci_core::host::TodoSnapshot::from(&original),
        "replacement": pisci_core::host::TodoSnapshot::from(&replacement),
        "summary": format!(
            "Todo '{}' ({}) was replaced by '{}' ({}).",
            short(&original.id),
            original.title,
            short(&replacement.id),
            replacement.title
        ),
    }))
}

// ─── git / branch merge ────────────────────────────────────────────────

pub async fn merge_branches(
    store: &PoolStore,
    caller: &CallerContext<'_>,
    pool_id_hint: &str,
) -> anyhow::Result<Value> {
    let session = resolve_pool(store, caller, pool_id_hint, "merge_branches").await?;
    let project_dir = match session.project_dir.as_deref() {
        Some(d) => d.to_string(),
        None => anyhow::bail!(
            "This pool has no project_dir. merge_branches requires a git-backed project."
        ),
    };
    let dir = std::path::PathBuf::from(&project_dir);
    if !dir.join(".git").exists() {
        anyhow::bail!("No Git repo found at '{}'", project_dir);
    }

    let results = git::merge_koi_branches(&dir, caller.cancel.clone()).await?;
    let rendered: Vec<String> = results
        .iter()
        .map(|r| match &r.outcome {
            MergeOutcome::Merged => format!("  {} — merged OK", r.branch),
            MergeOutcome::Conflict { message } => {
                format!("  {} — CONFLICT (aborted): {}", r.branch, message)
            }
            MergeOutcome::Error { message } => {
                format!("  {} — error: {}", r.branch, message)
            }
        })
        .collect();

    let summary = if results.is_empty() {
        "No koi/* branches to merge.".to_string()
    } else {
        format!(
            "Merge results for '{}' ({} branches):\n{}",
            session.name,
            results.len(),
            rendered.join("\n")
        )
    };

    Ok(json!({
        "pool_id": session.id,
        "branches": results
            .iter()
            .map(|r| json!({
                "branch": r.branch,
                "outcome": match &r.outcome {
                    MergeOutcome::Merged => json!({ "kind": "merged" }),
                    MergeOutcome::Conflict { message } => json!({ "kind": "conflict", "message": message }),
                    MergeOutcome::Error { message } => json!({ "kind": "error", "message": message }),
                },
            }))
            .collect::<Vec<_>>(),
        "summary": summary,
    }))
}
