//! `pool_org` — platform-neutral thin wrapper around
//! [`crate::pool::services`].
//!
//! Input schema, action dispatch table, and user-facing messages are
//! preserved verbatim from the former desktop implementation so the
//! LLM contract is unchanged. All state mutations happen in the kernel
//! service layer; this struct's only job is:
//!
//!   1. Parse the tool JSON into the matching `*Args` / field(s).
//!   2. Build a [`CallerContext`] from [`ToolContext`] (+ optional
//!      session-source lookup via [`PoolStore`]).
//!   3. Dispatch to the right `services::*` function.
//!   4. Format the returned `Value` into a [`ToolResult`] string.

use crate::agent::tool::{Tool, ToolContext, ToolResult};
use crate::pool::coordinator::CoordinatorConfig;
use crate::pool::model::{
    AssignKoiArgs, CallerContext, CreatePoolArgs, CreateTodoArgs, DeleteTodoArgs, PostStatusArgs,
    ReplaceTodoArgs, UpdateOrgSpecArgs, UpdateTodoStatusArgs, WaitForKoiArgs,
};
use crate::pool::{services, PoolStore};
use async_trait::async_trait;
use pisci_core::host::{PoolEventSink, SubagentRuntime};
use serde_json::{json, Value};
use std::sync::Arc;

pub struct PoolOrgTool {
    pub store: PoolStore,
    pub sink: Arc<dyn PoolEventSink>,
    /// Host-supplied subagent runtime for fanning out Koi wake-ups
    /// (assign_koi mention, resume_todo, replace_todo). Hosts that
    /// don't wire one surface a clean "not available" error.
    pub subagent: Option<Arc<dyn SubagentRuntime>>,
    /// Coordinator configuration (task timeout, worktree usage).
    pub coordinator_cfg: CoordinatorConfig,
}

impl PoolOrgTool {
    /// Resolve the session source from the DB via [`PoolStore`] so that
    /// heartbeat-scoped sessions correctly block auto-archiving.
    async fn session_source(&self, session_id: &str) -> Option<String> {
        let sid = session_id.to_string();
        self.store
            .read(move |db| db.get_session(&sid))
            .await
            .ok()
            .flatten()
            .map(|s| s.source)
            .filter(|s| !s.is_empty())
    }

    /// If the caller's session is bound to an IM conversation, return
    /// the matching `binding_key`. Used to seed
    /// `pool_sessions.origin_im_binding_key` when an IM-driven Pisci
    /// session creates a pool, so pool-level events later fan out to
    /// the originating IM channel.
    async fn lookup_origin_im_binding(&self, session_id: &str) -> Option<String> {
        let sid = session_id.to_string();
        self.store
            .read(move |db| db.find_im_session_binding_for_session(&sid))
            .await
            .ok()
            .flatten()
            .map(|b| b.binding_key)
    }

    fn caller<'a>(&'a self, ctx: &'a ToolContext, source: Option<&'a str>) -> CallerContext<'a> {
        CallerContext {
            memory_owner_id: &ctx.memory_owner_id,
            session_id: &ctx.session_id,
            session_source: source,
            pool_session_id: ctx.pool_session_id.as_deref(),
            cancel: Some(ctx.cancel.clone()),
        }
    }

    fn summary_of(value: &Value) -> String {
        value
            .get("summary")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string())
    }
}

#[async_trait]
impl Tool for PoolOrgTool {
    fn name(&self) -> &str {
        "pool_org"
    }

    fn description(&self) -> &str {
        "Manage project pools, lifecycle, and organization specs. Use this to set up collaborative projects. \
         \
         Actions: \
         - 'create': Create a new project pool with a name and org_spec. Optionally provide 'project_dir' to bind a filesystem directory (auto-initializes Git repo for Koi worktree isolation). \
         - 'read': Read the org_spec for an existing pool. \
         - 'update': Update the org_spec for an existing pool. \
         - 'list': List all project pools with their status. \
         - 'assign_koi': Pisci's standard way to assign concrete work to a Koi. It creates a todo, posts the controlled assignment, and requests Koi execution. \
         - 'pause': Pause a project (freezes task scheduling). \
         - 'resume': Resume a paused or archived project. \
         - 'archive': Archive a project (read-only). \
         - 'find_related': Search for existing projects by keywords. \
         - 'get_messages': Read recent messages for a project pool (requires pool_id, optional limit). \
         - 'get_todos': Read koi_todos associated with a project pool (requires pool_id). \
         - 'post_status': Pisci-only controlled status message for supervisor notes, decisions, or waiting explanations. This does not trigger @! mention fan-out. \
         - 'wait_for_koi': Pisci-only real elapsed-time wait after assigning/resuming/replacing work. Uses host-side sleep/backoff and returns structured status counts. \
         - 'create_todo': Create a new todo for yourself (requires pool_id, title; optional description, priority). Use this when you receive real work via `@!mention` or self-identify a task. \
         - 'claim_todo': Claim an existing unclaimed todo (requires todo_id). Marks it in_progress and assigns it to you. \
         - 'complete_todo': Mark a todo as done (requires todo_id, summary). The summary is a concise description of what was accomplished — it becomes the visible result in the pool chat. Pisci can complete any todo; Koi can only complete their own. Completing a todo does NOT hand off the next step automatically. \
         - 'cancel_todo': Cancel a todo (requires todo_id, optional reason). Pisci can cancel any todo; Koi can only cancel their own — to cancel someone else's, @pisci in pool_chat. \
         - 'resume_todo': Resume a blocked or needs_review todo (requires todo_id). This restarts execution for the existing task from its current project context. Pisci should decide when to use this. \
         - 'replace_todo': Replace an existing todo with a new owner/task (requires todo_id, new_owner_id, task, reason). This cancels the original todo so it cannot be resumed, creates a replacement todo, and notifies the new owner. Pisci should decide when to use this. \
         - 'delete_todo': Permanently delete todo rows from the board. Use `todo_id` for a single delete, or `pool_id` plus `delete_status` / `delete_owner_id` for filtered batch cleanup (for example deleting all cancelled todos in one pool). Pisci-only. \
         - 'update_todo_status': Update a todo's status (requires todo_id, status). Pisci can change any; Koi can only change their own. Valid statuses: todo, in_progress, blocked. This changes task-board state, but teammates still need an explicit `pool_chat` update if they should react. \
         - 'merge_branches': Pisci-only supervisor closeout action. Merge all Koi worktree branches back into the main workspace after reviewing completed Koi results (requires pool_id with project_dir). \
         \
         Workflow: ALWAYS call 'list' first to see all existing pools. \
         Then use 'find_related' to search for related projects by keywords. \
         Only call 'create' if no existing pool covers the requested work — \
         if an active or paused pool is related, add tasks to it instead of creating a new pool. \
         After creating a new pool, Pisci must use 'assign_koi' to kick off work, then call 'wait_for_koi' before judging progress. Pisci does not use pool_chat directly. When todos are done, Pisci must explicitly review and either call 'merge_branches' or request rework; Koi completion alone is not final delivery. \
         During heartbeat/routine checks: NEVER create new pools — only manage existing ones."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "read", "update", "list", "assign_koi", "pause", "resume", "archive", "find_related", "get_messages", "get_todos", "post_status", "wait_for_koi", "create_todo", "claim_todo", "complete_todo", "cancel_todo", "resume_todo", "replace_todo", "delete_todo", "update_todo_status", "merge_branches"],
                    "description": "Action to perform"
                },
                "project_dir": {
                    "type": "string",
                    "description": "For create: optional filesystem directory for the project. A Git repo will be auto-initialized there."
                },
                "task_timeout_secs": {
                    "type": "integer",
                    "description": "For create/update: optional default execution timeout in seconds for todos in this project. 0 or omitted means inherit the global system default."
                },
                "keywords": {
                    "type": "string",
                    "description": "For find_related: space-separated keywords to search for in project names and org_specs"
                },
                "pool_id": {
                    "type": "string",
                    "description": "For read/update/assign_koi/get_messages/get_todos: the pool session ID"
                },
                "limit": {
                    "type": "integer",
                    "description": "For get_messages: max number of messages (default 50)"
                },
                "content": {
                    "type": "string",
                    "description": "For post_status: the supervisor status message to publish"
                },
                "event_type": {
                    "type": "string",
                    "description": "For post_status: optional structured event label (default pisci_status)"
                },
                "name": {
                    "type": "string",
                    "description": "For create: the project pool name"
                },
                "org_spec": {
                    "type": "string",
                    "description": "For create/update: the organization spec in Markdown. Should include:\n\
                     ## Project Goal\n## Koi Roles\n## Collaboration Rules\n## Activation Conditions\n## Success Metrics"
                },
                "koi_id": {
                    "type": "string",
                    "description": "For assign_koi: the Koi to assign"
                },
                "task": {
                    "type": "string",
                    "description": "For assign_koi: the initial task description"
                },
                "priority": {
                    "type": "string",
                    "enum": ["low", "medium", "high", "urgent"],
                    "description": "For assign_koi: task priority (default: medium)"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "For assign_koi/create_todo/replace_todo: optional single-task timeout in seconds. For wait_for_koi: max elapsed wait seconds."
                },
                "min_wait_secs": {
                    "type": "integer",
                    "description": "For wait_for_koi: minimum elapsed seconds to wait before returning a terminal result (default 0)"
                },
                "initial_backoff_ms": {
                    "type": "integer",
                    "description": "For wait_for_koi: initial polling backoff in milliseconds (default 250)"
                },
                "max_backoff_ms": {
                    "type": "integer",
                    "description": "For wait_for_koi: maximum polling backoff in milliseconds (default 2000)"
                },
                "title": {
                    "type": "string",
                    "description": "For create_todo: the todo title"
                },
                "description": {
                    "type": "string",
                    "description": "For create_todo: optional description of the work"
                },
                "todo_id": {
                    "type": "string",
                    "description": "For claim_todo/complete_todo/cancel_todo/update_todo_status: the todo ID (full or prefix)"
                },
                "status": {
                    "type": "string",
                    "enum": ["todo", "in_progress", "blocked"],
                    "description": "For update_todo_status: the new status"
                },
                "delete_status": {
                    "type": "string",
                    "description": "For delete_todo batch cleanup: optional status filter (for example cancelled, done, blocked, todo, in_progress, needs_review)"
                },
                "delete_owner_id": {
                    "type": "string",
                    "description": "For delete_todo batch cleanup: optional Koi owner filter (ID or name)"
                },
                "reason": {
                    "type": "string",
                    "description": "For cancel_todo/replace_todo: optional reason for cancellation, or the required reason explaining why a task is being replaced"
                },
                "summary": {
                    "type": "string",
                    "description": "For complete_todo: REQUIRED. A concise description of what was accomplished. This becomes the visible result message in the pool chat."
                },
                "new_owner_id": {
                    "type": "string",
                    "description": "For replace_todo: the replacement Koi who should take over the task"
                }
            },
            "required": ["action"]
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let action = input["action"].as_str().unwrap_or("list");

        // Resolve session source once per call for actions that might
        // need it; this is cheap (single DB row lookup) and keeps the
        // CallerContext uniform.
        let source = self.session_source(&ctx.session_id).await;
        let caller = self.caller(ctx, source.as_deref());

        match action {
            "create" => {
                let origin_binding = self.lookup_origin_im_binding(&ctx.session_id).await;
                let args = CreatePoolArgs {
                    name: input["name"].as_str().unwrap_or("").to_string(),
                    project_dir: input["project_dir"].as_str().map(str::to_string),
                    org_spec: input["org_spec"].as_str().map(str::to_string),
                    task_timeout_secs: input["task_timeout_secs"].as_u64().unwrap_or(0) as u32,
                    origin_im_binding_key: origin_binding,
                };
                match services::create_pool(&self.store, &*self.sink, &caller, args).await {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "read" => {
                let pool_id = input["pool_id"].as_str().unwrap_or("");
                match services::read_org_spec(&self.store, &caller, pool_id).await {
                    Ok(v) => {
                        let name = v
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let id = v
                            .get("pool_id")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let spec = v
                            .get("org_spec")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let text = if spec.is_empty() {
                            format!(
                                "Pool '{}' has no org_spec set yet.\n\
                                 Use 'update' to create one with project goals, Koi roles, \
                                 collaboration rules, and success metrics.",
                                name
                            )
                        } else {
                            format!("Pool: {} ({})\n\n---\n{}", name, id, spec)
                        };
                        Ok(ToolResult::ok(text))
                    }
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "update" => {
                let args = UpdateOrgSpecArgs {
                    pool_id: input["pool_id"].as_str().unwrap_or("").to_string(),
                    org_spec: input["org_spec"].as_str().map(str::to_string),
                    task_timeout_secs: input["task_timeout_secs"].as_u64().map(|v| v as u32),
                };
                match services::update_org_spec(&self.store, &*self.sink, &caller, args).await {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "list" => match services::list_pools(&self.store).await {
                Ok(v) => Ok(ToolResult::ok(render_list_pools(&v))),
                Err(e) => Ok(ToolResult::err(e.to_string())),
            },
            "assign_koi" => {
                let args = AssignKoiArgs {
                    pool_id: input["pool_id"].as_str().unwrap_or("").to_string(),
                    koi_id: input["koi_id"].as_str().unwrap_or("").to_string(),
                    task: input["task"].as_str().unwrap_or("").to_string(),
                    priority: input["priority"].as_str().unwrap_or("medium").to_string(),
                    timeout_secs: input["timeout_secs"].as_u64().unwrap_or(0) as u32,
                };
                match services::assign_koi(
                    &self.store,
                    self.sink.clone(),
                    self.subagent.clone(),
                    &self.coordinator_cfg,
                    &caller,
                    args,
                )
                .await
                {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "pause" => {
                let pool_id = input["pool_id"].as_str().unwrap_or("");
                match services::set_pool_status(&self.store, &*self.sink, &caller, pool_id, "paused").await {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "resume" => {
                let pool_id = input["pool_id"].as_str().unwrap_or("");
                match services::set_pool_status(&self.store, &*self.sink, &caller, pool_id, "active").await {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "archive" => {
                let pool_id = input["pool_id"].as_str().unwrap_or("");
                match services::set_pool_status(&self.store, &*self.sink, &caller, pool_id, "archived").await {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "find_related" => {
                let k = input["keywords"].as_str().unwrap_or("");
                match services::find_related(&self.store, k).await {
                    Ok(v) => Ok(ToolResult::ok(render_find_related(&v, k))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "get_messages" => {
                let pool_id = input["pool_id"].as_str().unwrap_or("");
                let limit = input["limit"].as_i64().unwrap_or(50);
                match services::get_pool_messages(&self.store, &caller, pool_id, limit).await {
                    Ok(v) => Ok(ToolResult::ok(render_messages(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "get_todos" => {
                let pool_id = input["pool_id"].as_str().unwrap_or("");
                match services::get_pool_todos(&self.store, &caller, pool_id).await {
                    Ok(v) => Ok(ToolResult::ok(render_todos(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "post_status" => {
                let args = PostStatusArgs {
                    pool_id: input["pool_id"].as_str().unwrap_or("").to_string(),
                    content: input["content"].as_str().unwrap_or("").to_string(),
                    event_type: input["event_type"].as_str().map(str::to_string),
                };
                match services::post_status(&self.store, &*self.sink, &caller, args).await {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "wait_for_koi" => {
                let args = WaitForKoiArgs {
                    pool_id: input["pool_id"].as_str().unwrap_or("").to_string(),
                    koi_id: input["koi_id"].as_str().map(str::to_string),
                    todo_id: input["todo_id"].as_str().map(str::to_string),
                    min_wait_secs: input["min_wait_secs"].as_u64().unwrap_or(0),
                    timeout_secs: input["timeout_secs"].as_u64().unwrap_or(60),
                    initial_backoff_ms: input["initial_backoff_ms"].as_u64().unwrap_or(250),
                    max_backoff_ms: input["max_backoff_ms"].as_u64().unwrap_or(2000),
                };
                match services::wait_for_koi(&self.store, &caller, args).await {
                    Ok(v) => Ok(ToolResult::ok(render_wait_for_koi(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "create_todo" => {
                let args = CreateTodoArgs {
                    pool_id: input["pool_id"].as_str().unwrap_or("").to_string(),
                    title: input["title"].as_str().unwrap_or("").to_string(),
                    description: input["description"].as_str().unwrap_or("").to_string(),
                    priority: input["priority"].as_str().unwrap_or("medium").to_string(),
                    timeout_secs: input["timeout_secs"].as_u64().unwrap_or(0) as u32,
                };
                match services::create_todo(&self.store, &*self.sink, &caller, args).await {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "claim_todo" => {
                let todo_id = input["todo_id"].as_str().unwrap_or("");
                if todo_id.is_empty() {
                    return Ok(ToolResult::err("'todo_id' is required for action 'claim_todo'"));
                }
                match services::claim_todo(&self.store, &*self.sink, &caller, todo_id).await {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "complete_todo" => {
                let todo_id = input["todo_id"].as_str().unwrap_or("");
                let summary = input["summary"].as_str().unwrap_or("");
                if todo_id.is_empty() {
                    return Ok(ToolResult::err(
                        "'todo_id' is required for action 'complete_todo'",
                    ));
                }
                if summary.trim().is_empty() {
                    return Ok(ToolResult::err(
                        "'summary' is required for action 'complete_todo'. Provide a concise description of what was accomplished.",
                    ));
                }
                match services::complete_todo(&self.store, &*self.sink, &caller, todo_id, summary).await {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "cancel_todo" => {
                let todo_id = input["todo_id"].as_str().unwrap_or("");
                let reason = input["reason"].as_str().unwrap_or("Cancelled");
                if todo_id.is_empty() {
                    return Ok(ToolResult::err(
                        "'todo_id' is required for action 'cancel_todo'",
                    ));
                }
                match services::cancel_todo(&self.store, &*self.sink, &caller, todo_id, reason).await {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "resume_todo" => {
                let todo_id = input["todo_id"].as_str().unwrap_or("");
                if todo_id.is_empty() {
                    return Ok(ToolResult::err(
                        "'todo_id' is required for action 'resume_todo'",
                    ));
                }
                let Some(subagent) = self.subagent.clone() else {
                    return Ok(ToolResult::err(
                        "resume_todo is not available in this host (no subagent runtime bound).",
                    ));
                };
                match services::resume_todo(
                    &self.store,
                    self.sink.clone(),
                    subagent,
                    &self.coordinator_cfg,
                    &caller,
                    todo_id,
                )
                .await
                {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "replace_todo" => {
                let args = ReplaceTodoArgs {
                    todo_id: input["todo_id"].as_str().unwrap_or("").to_string(),
                    new_owner_id: input["new_owner_id"].as_str().unwrap_or("").to_string(),
                    task: input["task"].as_str().unwrap_or("").to_string(),
                    reason: input["reason"].as_str().unwrap_or("").to_string(),
                    timeout_secs: input["timeout_secs"].as_u64().map(|v| v as u32),
                };
                let Some(subagent) = self.subagent.clone() else {
                    return Ok(ToolResult::err(
                        "replace_todo is not available in this host (no subagent runtime bound).",
                    ));
                };
                match services::replace_todo(
                    &self.store,
                    self.sink.clone(),
                    subagent,
                    &self.coordinator_cfg,
                    &caller,
                    args,
                )
                .await
                {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "delete_todo" => {
                let args = DeleteTodoArgs {
                    todo_id: input["todo_id"].as_str().map(str::to_string),
                    pool_id: input["pool_id"].as_str().map(str::to_string),
                    status: input["delete_status"].as_str().map(str::to_string),
                    owner_id: input["delete_owner_id"].as_str().map(str::to_string),
                };
                match services::delete_todo(&self.store, &*self.sink, &caller, args).await {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "update_todo_status" => {
                let args = UpdateTodoStatusArgs {
                    todo_id: input["todo_id"].as_str().unwrap_or("").to_string(),
                    new_status: input["status"].as_str().unwrap_or("").to_string(),
                };
                match services::update_todo_status(&self.store, &*self.sink, &caller, args).await {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            "merge_branches" => {
                let pool_id = input["pool_id"].as_str().unwrap_or("");
                match services::merge_branches(&self.store, &caller, pool_id).await {
                    Ok(v) => Ok(ToolResult::ok(Self::summary_of(&v))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            _ => Ok(ToolResult::err(format!(
                "Unknown action '{}'. Use: create, read, update, list, assign_koi, pause, resume, archive, find_related, get_messages, get_todos, post_status, wait_for_koi, create_todo, claim_todo, complete_todo, cancel_todo, resume_todo, replace_todo, delete_todo, update_todo_status, merge_branches",
                action
            ))),
        }
    }
}

// ─── rendering helpers (operate on the services' JSON payload) ──────────

fn short_id(id: &str) -> &str {
    &id[..8.min(id.len())]
}

fn render_list_pools(v: &Value) -> String {
    use pisci_core::models::{KoiDefinition, PoolSession};
    let raw_sessions = v
        .get("raw_sessions")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();
    let sessions: Vec<PoolSession> = raw_sessions
        .into_iter()
        .filter_map(|x| serde_json::from_value::<PoolSession>(x).ok())
        .collect();
    let raw_kois = v
        .get("kois")
        .and_then(|k| k.as_array())
        .cloned()
        .unwrap_or_default();
    let kois: Vec<KoiDefinition> = raw_kois
        .into_iter()
        .filter_map(|x| serde_json::from_value::<KoiDefinition>(x).ok())
        .collect();

    if sessions.is_empty() {
        return "No project pools exist yet.\n\
                Use 'create' to set up a new project pool with an org_spec."
            .to_string();
    }

    let mut lines: Vec<String> = Vec::new();
    for s in &sessions {
        let has_spec = if s.org_spec.is_empty() {
            "no spec"
        } else {
            "has spec"
        };
        let last_active = s
            .last_active_at
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "never".to_string());
        let dir_info = s
            .project_dir
            .as_deref()
            .map(|d| format!(" | dir: {}", d))
            .unwrap_or_default();
        let timeout_info = if s.task_timeout_secs > 0 {
            format!(" | timeout: {}s", s.task_timeout_secs)
        } else {
            String::new()
        };
        lines.push(format!(
            "- {} (id: {}) [{}] status: {} | last active: {} | updated: {}{}{}",
            s.name,
            short_id(&s.id),
            has_spec,
            s.status,
            last_active,
            s.updated_at.format("%Y-%m-%d %H:%M"),
            dir_info,
            timeout_info
        ));
    }

    let koi_summary: Vec<String> = kois
        .iter()
        .map(|k| {
            format!(
                "  {} {} (id: {}) [{}] role: {}",
                k.icon,
                k.name,
                short_id(&k.id),
                k.status,
                if k.role.trim().is_empty() {
                    "unspecified"
                } else {
                    &k.role
                }
            )
        })
        .collect();

    format!(
        "Project Pools ({}):\n{}\n\nAvailable Koi ({}):\n{}",
        sessions.len(),
        lines.join("\n"),
        kois.len(),
        if koi_summary.is_empty() {
            "  (none)".to_string()
        } else {
            koi_summary.join("\n")
        }
    )
}

fn render_find_related(v: &Value, keywords: &str) -> String {
    use pisci_core::models::PoolSession;
    let arr = v
        .get("sessions")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();
    let results: Vec<PoolSession> = arr
        .into_iter()
        .filter_map(|x| serde_json::from_value::<PoolSession>(x).ok())
        .collect();

    if results.is_empty() {
        return format!(
            "No existing projects match keywords '{}'. Consider creating a new project.",
            keywords
        );
    }

    let mut lines: Vec<String> = Vec::new();
    for s in &results {
        let last_active = s
            .last_active_at
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "never".to_string());
        lines.push(format!(
            "- {} (id: {}) [{}] last active: {}{}",
            s.name,
            short_id(&s.id),
            s.status,
            last_active,
            if s.org_spec.is_empty() {
                ""
            } else {
                " | has org_spec"
            }
        ));
    }

    format!(
        "Found {} related project(s) for '{}':\n{}",
        results.len(),
        keywords,
        lines.join("\n")
    )
}

fn render_messages(v: &Value) -> String {
    use pisci_core::models::{PoolMessage, PoolSession};
    let session: Option<PoolSession> = v
        .get("pool")
        .cloned()
        .and_then(|x| serde_json::from_value(x).ok());
    let msgs: Vec<PoolMessage> = v
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|x| serde_json::from_value(x).ok())
        .collect();

    let session_label = session
        .as_ref()
        .map(|s| short_id(&s.id).to_string())
        .unwrap_or_else(|| "?".into());

    let mut lines: Vec<String> = Vec::new();
    for m in &msgs {
        let content_truncated = if m.content.chars().count() > 200 {
            format!("{}...", m.content.chars().take(200).collect::<String>())
        } else {
            m.content.clone()
        };
        let created = m.created_at.format("%Y-%m-%d %H:%M").to_string();
        lines.push(format!(
            "- {} | {} | {} | {}",
            m.sender_id, m.msg_type, content_truncated, created
        ));
    }

    format!(
        "Pool '{}' messages ({}):\n{}",
        session_label,
        msgs.len(),
        if lines.is_empty() {
            "(none)".to_string()
        } else {
            lines.join("\n")
        }
    )
}

fn render_todos(v: &Value) -> String {
    use pisci_core::models::KoiTodo;
    let pool_id = v
        .get("pool_id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let todos: Vec<KoiTodo> = v
        .get("todos")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|x| serde_json::from_value(x).ok())
        .collect();

    let mut lines: Vec<String> = Vec::new();
    for t in &todos {
        let claimed = t.claimed_by.as_deref().unwrap_or("-");
        lines.push(format!(
            "- {} | {} | {} | {} | {} | {}",
            short_id(&t.id),
            t.title,
            t.status,
            t.priority,
            t.owner_id,
            claimed
        ));
    }

    format!(
        "Pool '{}' todos ({}):\n{}",
        short_id(&pool_id),
        todos.len(),
        if lines.is_empty() {
            "(none)".to_string()
        } else {
            lines.join("\n")
        }
    )
}

fn render_wait_for_koi(v: &Value) -> String {
    let elapsed_ms = v.get("elapsed_ms").and_then(|x| x.as_u64()).unwrap_or(0);
    let timed_out = v
        .get("timed_out")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let terminal = v
        .get("terminal_reached")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let matched = v
        .get("matched_todo_count")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let counts = v
        .get("status_counts")
        .map(|x| x.to_string())
        .unwrap_or_else(|| "{}".to_string());
    let summary = v.get("summary").and_then(|x| x.as_str()).unwrap_or("");

    format!(
        "Waited {}ms for Koi work. terminal_reached={}, timed_out={}, matched_todos={}, status_counts={}. {}",
        elapsed_ms, terminal, timed_out, matched, counts, summary
    )
}
