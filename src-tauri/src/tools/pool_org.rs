/// pool_org tool — lets Pisci manage project pools and their organization specs.
///
/// Pisci can:
/// - Create a new project pool with an auto-generated org_spec
/// - Read the current org_spec for a pool
/// - Update the org_spec as the project evolves
/// - List all pools and their Koi assignments
/// - Kick off a project by creating initial tasks from the org_spec
///
/// This is the "project manager" interface: Pisci converses with the user,
/// understands the project goals, then uses this tool to formalize them into
/// a pool with an org_spec, assign Koi roles, and start execution.
use crate::agent::tool::{Tool, ToolContext, ToolResult};
use crate::koi::runtime::KOI_SESSIONS;
use crate::store::Database;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;

pub struct PoolOrgTool {
    pub app: AppHandle,
    pub db: Arc<Mutex<Database>>,
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
         - 'assign_koi': Assign a Koi to a pool and create an initial task. \
         - 'pause': Pause a project (freezes task scheduling). \
         - 'resume': Resume a paused or archived project. \
         - 'archive': Archive a project (read-only). \
         - 'find_related': Search for existing projects by keywords. \
         - 'get_messages': Read recent messages for a project pool (requires pool_id, optional limit). \
         - 'get_todos': Read koi_todos associated with a project pool (requires pool_id). \
         - 'create_todo': Create a new todo for yourself (requires pool_id, title; optional description, priority). Use this when you receive real work via @mention or self-identify a task. \
         - 'claim_todo': Claim an existing unclaimed todo (requires todo_id). Marks it in_progress and assigns it to you. \
         - 'complete_todo': Mark a todo as done (requires todo_id, summary). The summary is a concise description of what was accomplished — it becomes the visible result in the pool chat. Pisci can complete any todo; Koi can only complete their own. \
         - 'cancel_todo': Cancel a todo (requires todo_id, optional reason). Pisci can cancel any todo; Koi can only cancel their own — to cancel someone else's, @pisci in pool_chat. \
         - 'update_todo_status': Update a todo's status (requires todo_id, status). Pisci can change any; Koi can only change their own. Valid statuses: todo, in_progress, blocked. \
         - 'merge_branches': Merge all Koi worktree branches back into main (requires pool_id with project_dir). \
         \
         Workflow: ALWAYS call 'list' first to see all existing pools. \
         Then use 'find_related' to search for related projects by keywords. \
         Only call 'create' if no existing pool covers the requested work — \
         if an active or paused pool is related, add tasks to it instead of creating a new pool. \
         After creating a new pool, use 'assign_koi' or pool_chat @mention to kick off work. \
         During heartbeat/routine checks: NEVER create new pools — only manage existing ones."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "read", "update", "list", "assign_koi", "pause", "resume", "archive", "find_related", "get_messages", "get_todos", "create_todo", "claim_todo", "complete_todo", "cancel_todo", "update_todo_status", "merge_branches"],
                    "description": "Action to perform"
                },
                "project_dir": {
                    "type": "string",
                    "description": "For create: optional filesystem directory for the project. A Git repo will be auto-initialized there."
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
                "reason": {
                    "type": "string",
                    "description": "For cancel_todo: optional reason for cancellation"
                },
                "summary": {
                    "type": "string",
                    "description": "For complete_todo: REQUIRED. A concise description of what was accomplished. This becomes the visible result message in the pool chat."
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
        match action {
            "create" => self.create_pool(&input).await,
            "read" => self.read_org_spec(&input, ctx).await,
            "update" => self.update_org_spec(&input, ctx).await,
            "list" => self.list_pools().await,
            "assign_koi" => self.assign_koi(&input, ctx).await,
            "pause" => self.set_status(&input, ctx, "pause", "paused").await,
            "resume" => self.set_status(&input, ctx, "resume", "active").await,
            "archive" => self.set_status(&input, ctx, "archive", "archived").await,
            "find_related" => self.find_related(&input).await,
            "get_messages" => self.get_messages(&input, ctx).await,
            "get_todos" => self.get_todos(&input, ctx).await,
            "create_todo" => self.create_todo(&input, ctx).await,
            "claim_todo" => self.claim_todo(&input, ctx).await,
            "complete_todo" => self.complete_todo(&input, ctx).await,
            "cancel_todo" => self.cancel_todo(&input, ctx).await,
            "update_todo_status" => self.update_todo_status(&input, ctx).await,
            "merge_branches" => self.merge_branches(&input, ctx).await,
            _ => Ok(ToolResult::err(format!("Unknown action '{}'. Use: create, read, update, list, assign_koi, pause, resume, archive, find_related, get_messages, get_todos, create_todo, claim_todo, complete_todo, cancel_todo, update_todo_status, merge_branches", action))),
        }
    }
}

impl PoolOrgTool {
    fn ensure_pool_accepts_new_work(
        session: &crate::koi::PoolSession,
        action: &str,
    ) -> anyhow::Result<()> {
        if session.status == "active" {
            return Ok(());
        }
        Err(anyhow::anyhow!(
            "Pool '{}' is {}. Action '{}' is disabled until the pool is resumed.",
            session.name,
            session.status,
            action
        ))
    }

    async fn list_active_pool_todos(
        &self,
        pool_id: &str,
    ) -> anyhow::Result<Vec<crate::koi::KoiTodo>> {
        let db = self.db.lock().await;
        Ok(db
            .list_koi_todos(None)?
            .into_iter()
            .filter(|todo| {
                todo.pool_session_id.as_deref() == Some(pool_id)
                    && !matches!(todo.status.as_str(), "done" | "cancelled")
            })
            .collect())
    }

    async fn cancel_running_pool_kois(&self, pool_id: &str) -> anyhow::Result<usize> {
        let state = self.app.state::<crate::store::AppState>();
        let mut affected_koi_ids: HashSet<String> = HashSet::new();

        {
            let flags = state.cancel_flags.lock().await;
            for (key, flag) in flags.iter() {
                if let Some(koi_id) = Self::koi_id_from_cancel_key(key, pool_id) {
                    flag.store(true, Ordering::Relaxed);
                    affected_koi_ids.insert(koi_id.to_string());
                }
            }
        }

        let active_other_sessions: HashSet<String> = {
            let mut sessions = KOI_SESSIONS.lock().await;
            let keys_to_remove: Vec<String> = sessions
                .keys()
                .filter(|key| key.ends_with(&format!(":{}", pool_id)))
                .cloned()
                .collect();
            for key in keys_to_remove {
                if let Some(koi_id) = key.split(':').next() {
                    affected_koi_ids.insert(koi_id.to_string());
                }
                sessions.remove(&key);
            }
            sessions
                .keys()
                .filter_map(|key| key.split(':').next().map(str::to_string))
                .collect()
        };

        let koi_ids_to_idle: Vec<String> = affected_koi_ids
            .into_iter()
            .filter(|koi_id| !active_other_sessions.contains(koi_id))
            .collect();

        if koi_ids_to_idle.is_empty() {
            return Ok(0);
        }

        let db = self.db.lock().await;
        for koi_id in &koi_ids_to_idle {
            let _ = db.update_koi_status(koi_id, "idle");
        }
        drop(db);

        for koi_id in &koi_ids_to_idle {
            let _ = self.app.emit(
                "koi_status_changed",
                json!({ "id": koi_id, "status": "idle" }),
            );
        }

        Ok(koi_ids_to_idle.len())
    }

    fn koi_id_from_cancel_key<'a>(key: &'a str, pool_id: &str) -> Option<&'a str> {
        let suffix = format!("_{}", pool_id);
        key.strip_prefix("koi_")
            .and_then(|rest| rest.strip_suffix(&suffix))
    }

    async fn resolve_pool_session(
        &self,
        input: &Value,
        ctx: &ToolContext,
        action: &str,
    ) -> anyhow::Result<crate::koi::PoolSession> {
        let requested = input["pool_id"]
            .as_str()
            .map(str::trim)
            .filter(|id| !id.is_empty() && *id != "current")
            .map(str::to_string)
            .or_else(|| ctx.pool_session_id.clone());

        let pool_id = match requested {
            Some(id) => id,
            None => {
                return Err(anyhow::anyhow!(
                    "'pool_id' is required for action '{}'",
                    action
                ))
            }
        };

        let db = self.db.lock().await;
        match db.resolve_pool_session_identifier(&pool_id)? {
            Some(session) => Ok(session),
            None => Err(anyhow::anyhow!("Pool '{}' not found", pool_id)),
        }
    }

    async fn create_pool(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let name = match input["name"].as_str() {
            Some(n) if !n.trim().is_empty() => n.trim(),
            _ => return Ok(ToolResult::err("'name' is required for action 'create'")),
        };
        let org_spec = input["org_spec"].as_str().unwrap_or("").trim();
        let project_dir = input["project_dir"]
            .as_str()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());

        // If project_dir is specified, create directory and initialize Git repo
        let mut git_info = String::new();
        if let Some(dir) = project_dir {
            let dir_path = std::path::Path::new(dir);
            if let Err(e) = std::fs::create_dir_all(dir_path) {
                return Ok(ToolResult::err(format!(
                    "Failed to create project directory '{}': {}",
                    dir, e
                )));
            }
            let git_dir = dir_path.join(".git");
            if !git_dir.exists() {
                let output = std::process::Command::new("git")
                    .args(["init"])
                    .current_dir(dir_path)
                    .output();
                match output {
                    Ok(o) if o.status.success() => {
                        let gitignore = dir_path.join(".gitignore");
                        if !gitignore.exists() {
                            let _ = std::fs::write(&gitignore, ".koi-worktrees/\n");
                        }
                        let _ = std::process::Command::new("git")
                            .args(["add", ".gitignore"])
                            .current_dir(dir_path)
                            .output();
                        let _ = std::process::Command::new("git")
                            .args(["commit", "-m", "Initial commit", "--allow-empty"])
                            .current_dir(dir_path)
                            .output();
                        git_info = format!("\nGit: initialized at {}", dir);
                    }
                    Ok(o) => {
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        return Ok(ToolResult::err(format!("git init failed: {}", stderr)));
                    }
                    Err(e) => {
                        return Ok(ToolResult::err(format!(
                            "Failed to run git: {}. Is Git installed?",
                            e
                        )));
                    }
                }
            } else {
                git_info = format!("\nGit: existing repo at {}", dir);
            }
        }

        let db = self.db.lock().await;
        let session = db.create_pool_session_with_dir(name, project_dir)?;

        if !org_spec.is_empty() {
            db.update_pool_org_spec(&session.id, org_spec)?;
        }

        let _ = db.insert_pool_message_ext(
            &session.id,
            "pisci",
            &format!(
                "项目池「{}」已创建。{}{}",
                name,
                if org_spec.is_empty() {
                    "尚未设定组织规范。"
                } else {
                    "组织规范已就绪。"
                },
                if project_dir.is_some() {
                    " Git 仓库已初始化，Koi 将使用独立 worktree 工作。"
                } else {
                    ""
                }
            ),
            "status_update",
            &json!({ "event": "pool_created" }).to_string(),
            None,
            None,
            Some("pool_created"),
        );

        let _ = self.app.emit(
            "pool_session_created",
            json!({ "id": session.id, "name": name }),
        );

        Ok(ToolResult::ok(format!(
            "Project pool created.\n\
             ID: {}\n\
             Name: {}\n\
             Org Spec: {}{}\n\n\
             Next: use 'assign_koi' to assign Koi agents and kick off tasks, \
             or use 'call_koi' to directly delegate work.",
            session.id,
            name,
            if org_spec.is_empty() {
                "not set (use 'update' to add one)"
            } else {
                "set"
            },
            git_info
        )))
    }

    async fn read_org_spec(&self, input: &Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        match self.resolve_pool_session(input, ctx, "read").await {
            Ok(session) => {
                if session.org_spec.is_empty() {
                    Ok(ToolResult::ok(format!(
                        "Pool '{}' has no org_spec set yet.\n\
                         Use 'update' to create one with project goals, Koi roles, \
                         collaboration rules, and success metrics.",
                        session.name
                    )))
                } else {
                    Ok(ToolResult::ok(format!(
                        "Pool: {} ({})\n\n---\n{}",
                        session.name, session.id, session.org_spec
                    )))
                }
            }
            Err(err) => Ok(ToolResult::err(err.to_string())),
        }
    }

    async fn update_org_spec(
        &self,
        input: &Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<ToolResult> {
        let org_spec = match input["org_spec"].as_str() {
            Some(s) if !s.trim().is_empty() => s.trim(),
            _ => {
                return Ok(ToolResult::err(
                    "'org_spec' is required for action 'update'",
                ))
            }
        };

        let session = match self.resolve_pool_session(input, ctx, "update").await {
            Ok(session) => session,
            Err(err) => return Ok(ToolResult::err(err.to_string())),
        };

        let db = self.db.lock().await;
        db.update_pool_org_spec(&session.id, org_spec)?;

        let _ = db.insert_pool_message_ext(
            &session.id,
            "pisci",
            "组织规范已更新。",
            "status_update",
            &json!({ "event": "org_spec_updated" }).to_string(),
            None,
            None,
            Some("org_spec_updated"),
        );

        Ok(ToolResult::ok(format!(
            "Org spec updated for pool '{}' ({}).\n\
             The spec will be loaded as context when Koi agents are activated in this pool.",
            session.name, session.id
        )))
    }

    async fn list_pools(&self) -> anyhow::Result<ToolResult> {
        let db = self.db.lock().await;
        let sessions = db.list_pool_sessions()?;
        let kois = db.list_kois().unwrap_or_default();
        drop(db);

        if sessions.is_empty() {
            return Ok(ToolResult::ok(
                "No project pools exist yet.\n\
                 Use 'create' to set up a new project pool with an org_spec.",
            ));
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
            lines.push(format!(
                "- {} (id: {}) [{}] status: {} | last active: {} | updated: {}{}",
                s.name,
                &s.id[..8.min(s.id.len())],
                has_spec,
                s.status,
                last_active,
                s.updated_at.format("%Y-%m-%d %H:%M"),
                dir_info
            ));
        }

        let koi_summary: Vec<String> = kois
            .iter()
            .map(|k| {
                format!(
                    "  {} {} (id: {}) [{}] role: {}",
                    k.icon,
                    k.name,
                    &k.id[..8.min(k.id.len())],
                    k.status,
                    if k.role.trim().is_empty() {
                        "unspecified"
                    } else {
                        &k.role
                    }
                )
            })
            .collect();

        Ok(ToolResult::ok(format!(
            "Project Pools ({}):\n{}\n\nAvailable Koi ({}):\n{}",
            sessions.len(),
            lines.join("\n"),
            kois.len(),
            if koi_summary.is_empty() {
                "  (none)".to_string()
            } else {
                koi_summary.join("\n")
            }
        )))
    }

    async fn set_status(
        &self,
        input: &Value,
        ctx: &ToolContext,
        action: &str,
        new_status: &str,
    ) -> anyhow::Result<ToolResult> {
        let session = match self.resolve_pool_session(input, ctx, action).await {
            Ok(session) => session,
            Err(err) => return Ok(ToolResult::err(err.to_string())),
        };

        if session.status == new_status {
            return Ok(ToolResult::ok(format!(
                "Project '{}' is already {}.",
                session.name, new_status
            )));
        }

        if new_status == "archived" {
            let active_todos = self.list_active_pool_todos(&session.id).await?;
            if !active_todos.is_empty() {
                let todo_preview = active_todos
                    .iter()
                    .take(3)
                    .map(|todo| format!("{} [{}]", &todo.id[..8.min(todo.id.len())], todo.status))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Ok(ToolResult::err(format!(
                    "Pool '{}' still has {} active todo(s): {}. Finish, block, or cancel them before archiving.",
                    session.name,
                    active_todos.len(),
                    todo_preview
                )));
            }
        }

        let old_status = session.status.clone();
        {
            let db = self.db.lock().await;
            db.update_pool_session_status(&session.id, new_status)?;
        }

        let halted_koi_count = if new_status == "archived" {
            self.cancel_running_pool_kois(&session.id).await?
        } else {
            0
        };

        let status_label = match new_status {
            "paused" => "已暂停",
            "archived" => "已归档",
            "active" => "已恢复",
            _ => new_status,
        };

        let db = self.db.lock().await;
        let _ = db.insert_pool_message_ext(
            &session.id,
            "pisci",
            &format!("项目状态变更: {} → {}", old_status, new_status),
            "status_update",
            &json!({
                "event": "status_changed",
                "old": old_status,
                "new": new_status,
                "halted_koi_count": halted_koi_count
            })
            .to_string(),
            None,
            None,
            Some("status_changed"),
        );
        drop(db);

        let _ = self.app.emit(
            "pool_session_updated",
            json!({ "id": session.id, "status": new_status }),
        );

        Ok(ToolResult::ok(format!(
            "Project '{}' {status_label} (status: {} → {}).{}",
            session.name,
            old_status,
            new_status,
            if halted_koi_count > 0 {
                format!(" Halted {} running Koi session(s).", halted_koi_count)
            } else {
                String::new()
            }
        )))
    }

    async fn find_related(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let keywords = match input["keywords"].as_str() {
            Some(k) if !k.trim().is_empty() => k.trim(),
            _ => {
                return Ok(ToolResult::err(
                    "'keywords' is required for action 'find_related'",
                ))
            }
        };

        let db = self.db.lock().await;
        let results = db.find_related_pool_sessions(keywords)?;
        drop(db);

        if results.is_empty() {
            return Ok(ToolResult::ok(format!(
                "No existing projects match keywords '{}'. Consider creating a new project.",
                keywords
            )));
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
                &s.id[..8.min(s.id.len())],
                s.status,
                last_active,
                if s.org_spec.is_empty() {
                    ""
                } else {
                    " | has org_spec"
                }
            ));
        }

        Ok(ToolResult::ok(format!(
            "Found {} related project(s) for '{}':\n{}",
            results.len(),
            keywords,
            lines.join("\n")
        )))
    }

    async fn assign_koi(&self, input: &Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let pool = match self.resolve_pool_session(input, ctx, "assign_koi").await {
            Ok(session) => session,
            Err(err) => return Ok(ToolResult::err(err.to_string())),
        };
        if let Err(err) = Self::ensure_pool_accepts_new_work(&pool, "assign_koi") {
            return Ok(ToolResult::err(err.to_string()));
        }
        let pool_id = pool.id.clone();
        let koi_id = match input["koi_id"].as_str() {
            Some(id) if !id.trim().is_empty() => id.trim().to_string(),
            _ => {
                return Ok(ToolResult::err(
                    "'koi_id' is required for action 'assign_koi'",
                ))
            }
        };
        let task = match input["task"].as_str() {
            Some(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => {
                return Ok(ToolResult::err(
                    "'task' is required for action 'assign_koi'",
                ))
            }
        };
        let priority = input["priority"].as_str().unwrap_or("medium").to_string();

        let state = self.app.state::<crate::store::AppState>();
        let runtime =
            crate::koi::runtime::KoiRuntime::from_tauri(self.app.clone(), state.db.clone());

        // Create the todo (non-blocking)
        let (todo, assign_msg_id) = runtime
            .assign_task(&koi_id, &task, "pisci", Some(&pool_id), &priority)
            .await?;

        let todo_id_short = todo.id[..8.min(todo.id.len())].to_string();

        // Spawn async execution — Pisci doesn't wait for the Koi to finish
        let app_clone = self.app.clone();
        let db_clone = self.db.clone();
        let koi_id_clone = koi_id.clone();
        let pool_id_clone = pool_id.clone();
        tokio::spawn(async move {
            let runtime = crate::koi::runtime::KoiRuntime::from_tauri(app_clone, db_clone);
            match runtime
                .execute_todo(&koi_id_clone, &todo, assign_msg_id, Some(&pool_id_clone))
                .await
            {
                Ok(r) => {
                    tracing::info!(
                        "Koi '{}' task completed (success={})",
                        koi_id_clone,
                        r.success
                    );
                    if r.success && r.reply.contains('@') {
                        if let Err(e) = runtime
                            .handle_mention(&koi_id_clone, &pool_id_clone, &r.reply)
                            .await
                        {
                            tracing::warn!("@mention dispatch from result failed: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Koi '{}' task execution failed: {}", koi_id_clone, e);
                }
            }
        });

        Ok(ToolResult::ok(format!(
            "Task assigned to Koi '{}' (todo: {}).\n\
             The Koi is now working on it asynchronously. \
             Use pool_org(action=\"get_messages\", pool_id=\"{}\") to check progress, \
             or pool_org(action=\"get_todos\", pool_id=\"{}\") to see task status.",
            koi_id, todo_id_short, pool_id, pool_id
        )))
    }

    async fn get_messages(&self, input: &Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let limit = input["limit"].as_i64().unwrap_or(50);

        let session = match self.resolve_pool_session(input, ctx, "get_messages").await {
            Ok(session) => session,
            Err(err) => return Ok(ToolResult::err(err.to_string())),
        };

        let db = self.db.lock().await;
        let messages = db.get_pool_messages(&session.id, limit, 0)?;
        drop(db);

        let mut lines: Vec<String> = Vec::new();
        for m in &messages {
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

        Ok(ToolResult::ok(format!(
            "Pool '{}' messages ({}):\n{}",
            &session.id[..8.min(session.id.len())],
            messages.len(),
            if lines.is_empty() {
                "(none)".to_string()
            } else {
                lines.join("\n")
            }
        )))
    }

    async fn get_todos(&self, input: &Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let session = match self.resolve_pool_session(input, ctx, "get_todos").await {
            Ok(session) => session,
            Err(err) => return Ok(ToolResult::err(err.to_string())),
        };

        let db = self.db.lock().await;
        let all_todos = db.list_koi_todos(None)?;
        drop(db);

        let todos: Vec<_> = all_todos
            .into_iter()
            .filter(|t| t.pool_session_id.as_deref() == Some(session.id.as_str()))
            .collect();

        let mut lines: Vec<String> = Vec::new();
        for t in &todos {
            let id_short = &t.id[..8.min(t.id.len())];
            let claimed = t.claimed_by.as_deref().unwrap_or("-");
            lines.push(format!(
                "- {} | {} | {} | {} | {} | {}",
                id_short, t.title, t.status, t.priority, t.owner_id, claimed
            ));
        }

        Ok(ToolResult::ok(format!(
            "Pool '{}' todos ({}):\n{}",
            &session.id[..8.min(session.id.len())],
            todos.len(),
            if lines.is_empty() {
                "(none)".to_string()
            } else {
                lines.join("\n")
            }
        )))
    }

    fn is_pisci(ctx: &ToolContext) -> bool {
        ctx.memory_owner_id == "pisci"
    }

    fn resolve_todo_id(input: &Value) -> Option<&str> {
        input["todo_id"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim())
    }

    async fn find_todo_by_prefix(
        &self,
        prefix: &str,
    ) -> anyhow::Result<Option<crate::koi::KoiTodo>> {
        let db = self.db.lock().await;
        let todos = db.list_koi_todos(None)?;
        drop(db);
        let matches: Vec<_> = todos
            .into_iter()
            .filter(|t| t.id.starts_with(prefix))
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(Some(matches.into_iter().next().unwrap())),
            _ => Ok(Some(matches.into_iter().next().unwrap())),
        }
    }

    fn check_todo_ownership(
        todo: &crate::koi::KoiTodo,
        ctx: &ToolContext,
    ) -> Result<(), ToolResult> {
        if Self::is_pisci(ctx) {
            return Ok(());
        }
        if todo.owner_id != ctx.memory_owner_id {
            return Err(ToolResult::err(format!(
                "Permission denied. You can only manage your own todos. This todo belongs to '{}'. \
                 To cancel or modify another agent's task, @pisci in pool_chat to request it.",
                todo.owner_id
            )));
        }
        Ok(())
    }

    async fn create_todo(&self, input: &Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let pool_id = match input["pool_id"].as_str() {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => {
                return Ok(ToolResult::err(
                    "'pool_id' is required for action 'create_todo'",
                ))
            }
        };
        let title = match input["title"].as_str() {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => {
                return Ok(ToolResult::err(
                    "'title' is required for action 'create_todo'",
                ))
            }
        };
        let description = input["description"].as_str().unwrap_or("").to_string();
        let priority = input["priority"].as_str().unwrap_or("medium").to_string();
        let owner_id = ctx.memory_owner_id.clone();

        let session = match self.resolve_pool_session(input, ctx, "create_todo").await {
            Ok(s) => s,
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };
        if let Err(e) = Self::ensure_pool_accepts_new_work(&session, "create_todo") {
            return Ok(ToolResult::err(e.to_string()));
        }

        let todo = {
            let db = self.db.lock().await;
            db.create_koi_todo(
                &owner_id,
                &title,
                &description,
                &priority,
                &owner_id,
                Some(&pool_id),
                "koi",
                None,
            )
            .map_err(|e| anyhow::anyhow!(e))?
        };

        let _ = self.app.emit(
            "koi_todo_updated",
            serde_json::json!({ "id": todo.id, "action": "created", "by": owner_id }),
        );

        Ok(ToolResult::ok(format!(
            "Todo '{}' created with ID `{}`. Use claim_todo to start working on it.",
            todo.title,
            &todo.id[..8.min(todo.id.len())]
        )))
    }

    async fn claim_todo(&self, input: &Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let todo_id = match Self::resolve_todo_id(input) {
            Some(id) => id,
            None => {
                return Ok(ToolResult::err(
                    "'todo_id' is required for action 'claim_todo'",
                ))
            }
        };

        let todo = match self.find_todo_by_prefix(todo_id).await? {
            Some(t) => t,
            None => return Ok(ToolResult::err(format!("Todo '{}' not found", todo_id))),
        };

        if todo.status == "done" || todo.status == "cancelled" {
            return Ok(ToolResult::err(format!(
                "Cannot claim a todo with status '{}'.",
                todo.status
            )));
        }
        if todo.claimed_by.is_some() && todo.claimed_by.as_deref() != Some(&ctx.memory_owner_id) {
            return Ok(ToolResult::err(format!(
                "Todo '{}' is already claimed by '{}'.",
                &todo.id[..8.min(todo.id.len())],
                todo.claimed_by.as_deref().unwrap_or("unknown")
            )));
        }

        // Koi can only claim their own todos (or Pisci can claim any)
        if !Self::is_pisci(ctx) && todo.owner_id != ctx.memory_owner_id {
            return Ok(ToolResult::err(format!(
                "Permission denied. You can only claim your own todos. This todo belongs to '{}'.",
                todo.owner_id
            )));
        }

        {
            let db = self.db.lock().await;
            db.claim_koi_todo(&todo.id, &ctx.memory_owner_id)
                .map_err(|e| anyhow::anyhow!(e))?;
        }

        // Post a "task_claimed" message to pool chat so the coordinator tab shows it
        if let Some(ref psid) = todo.pool_session_id {
            let db = self.db.lock().await;
            if let Ok(msg) = db.insert_pool_message_ext(
                psid,
                &ctx.memory_owner_id,
                &format!("接受了任务: {}", todo.title),
                "task_claimed",
                "{}",
                Some(&todo.id),
                None,
                Some("task_claimed"),
            ) {
                let _ = self.app.emit(
                    &format!("pool_message_{}", psid),
                    serde_json::to_value(&msg).unwrap_or_default(),
                );
            }
        }

        let _ = self.app.emit(
            "koi_todo_updated",
            serde_json::json!({ "id": todo.id, "action": "claimed", "claimed_by": ctx.memory_owner_id }),
        );

        Ok(ToolResult::ok(format!(
            "Todo '{}' ({}) claimed. Status is now in_progress.",
            &todo.id[..8.min(todo.id.len())],
            todo.title
        )))
    }

    async fn complete_todo(&self, input: &Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let todo_id = match Self::resolve_todo_id(input) {
            Some(id) => id,
            None => {
                return Ok(ToolResult::err(
                    "'todo_id' is required for action 'complete_todo'",
                ))
            }
        };

        let summary = match input["summary"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(s) => s.to_string(),
            None => {
                return Ok(ToolResult::err(
                    "'summary' is required for action 'complete_todo'. Provide a concise description of what was accomplished.",
                ))
            }
        };

        let todo = match self.find_todo_by_prefix(todo_id).await? {
            Some(t) => t,
            None => return Ok(ToolResult::err(format!("Todo '{}' not found", todo_id))),
        };

        if todo.status == "done" {
            return Ok(ToolResult::ok(format!(
                "Todo '{}' is already completed.",
                &todo.id[..8.min(todo.id.len())]
            )));
        }
        if todo.status == "cancelled" {
            return Ok(ToolResult::err("Cannot complete a cancelled todo."));
        }

        if let Err(r) = Self::check_todo_ownership(&todo, ctx) {
            return Ok(r);
        }

        // Write the summary as a result message in the pool, then link it to the todo
        let result_msg_id = if let Some(ref psid) = todo.pool_session_id {
            let db = self.db.lock().await;
            match db.insert_pool_message(psid, &ctx.memory_owner_id, &summary, "result", "{}") {
                Ok(msg) => {
                    let _ = self.app.emit(
                        &format!("pool_message_{}", psid),
                        serde_json::to_value(&msg).unwrap_or_default(),
                    );
                    Some(msg.id)
                }
                Err(_) => None,
            }
        } else {
            None
        };

        let db = self.db.lock().await;
        db.complete_koi_todo(&todo.id, result_msg_id)?;
        drop(db);

        let _ = self.app.emit(
            "koi_todo_updated",
            json!({
                "id": todo.id, "action": "completed", "by": ctx.memory_owner_id
            }),
        );

        Ok(ToolResult::ok(format!(
            "Todo '{}' ({}) marked as completed. Summary recorded.",
            &todo.id[..8.min(todo.id.len())],
            todo.title
        )))
    }

    async fn cancel_todo(&self, input: &Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let todo_id = match Self::resolve_todo_id(input) {
            Some(id) => id,
            None => {
                return Ok(ToolResult::err(
                    "'todo_id' is required for action 'cancel_todo'",
                ))
            }
        };
        let reason = input["reason"].as_str().unwrap_or("Cancelled");

        let todo = match self.find_todo_by_prefix(todo_id).await? {
            Some(t) => t,
            None => return Ok(ToolResult::err(format!("Todo '{}' not found", todo_id))),
        };

        if todo.status == "cancelled" {
            return Ok(ToolResult::ok(format!(
                "Todo '{}' is already cancelled.",
                &todo.id[..8.min(todo.id.len())]
            )));
        }
        if todo.status == "done" {
            return Ok(ToolResult::err("Cannot cancel a completed todo."));
        }

        if let Err(r) = Self::check_todo_ownership(&todo, ctx) {
            return Ok(r);
        }

        let db = self.db.lock().await;
        db.update_koi_todo(&todo.id, None, None, Some("cancelled"), None)?;
        drop(db);

        let _ = self.app.emit(
            "koi_todo_updated",
            json!({
                "id": todo.id, "action": "cancelled", "by": ctx.memory_owner_id, "reason": reason
            }),
        );

        if let Some(ref psid) = todo.pool_session_id {
            let db = self.db.lock().await;
            if let Ok(msg) = db.insert_pool_message(
                psid,
                &ctx.memory_owner_id,
                &format!("[Task Cancelled] \"{}\" — {}", todo.title, reason),
                "system",
                "{}",
            ) {
                let _ = self.app.emit(
                    &format!("pool_message_{}", psid),
                    serde_json::to_value(&msg).unwrap_or_default(),
                );
            }
        }

        Ok(ToolResult::ok(format!(
            "Todo '{}' ({}) cancelled. Reason: {}",
            &todo.id[..8.min(todo.id.len())],
            todo.title,
            reason
        )))
    }

    async fn update_todo_status(
        &self,
        input: &Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<ToolResult> {
        let todo_id = match Self::resolve_todo_id(input) {
            Some(id) => id,
            None => {
                return Ok(ToolResult::err(
                    "'todo_id' is required for action 'update_todo_status'",
                ))
            }
        };
        let new_status = match input["status"].as_str() {
            Some(s) if matches!(s, "todo" | "in_progress" | "blocked") => s,
            _ => return Ok(ToolResult::err("'status' must be one of: todo, in_progress, blocked. Use 'complete_todo' or 'cancel_todo' for terminal states.")),
        };

        let todo = match self.find_todo_by_prefix(todo_id).await? {
            Some(t) => t,
            None => return Ok(ToolResult::err(format!("Todo '{}' not found", todo_id))),
        };

        if todo.status == "done" || todo.status == "cancelled" {
            return Ok(ToolResult::err(format!(
                "Cannot update status of a {} todo.",
                todo.status
            )));
        }

        if let Err(r) = Self::check_todo_ownership(&todo, ctx) {
            return Ok(r);
        }

        let db = self.db.lock().await;
        db.update_koi_todo(&todo.id, None, None, Some(new_status), None)?;
        drop(db);

        let _ = self.app.emit("koi_todo_updated", json!({
            "id": todo.id, "action": "status_changed", "status": new_status, "by": ctx.memory_owner_id
        }));

        Ok(ToolResult::ok(format!(
            "Todo '{}' ({}) status changed to '{}'.",
            &todo.id[..8.min(todo.id.len())],
            todo.title,
            new_status
        )))
    }

    async fn merge_branches(&self, input: &Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let session = match self
            .resolve_pool_session(input, ctx, "merge_branches")
            .await
        {
            Ok(session) => session,
            Err(err) => return Ok(ToolResult::err(err.to_string())),
        };

        let project_dir =
            match session.project_dir.as_deref() {
                Some(d) => d,
                None => return Ok(ToolResult::err(
                    "This pool has no project_dir. merge_branches requires a Git-backed project.",
                )),
            };

        let dir = std::path::Path::new(project_dir);
        if !dir.join(".git").exists() {
            return Ok(ToolResult::err(format!(
                "No Git repo found at '{}'",
                project_dir
            )));
        }

        let branch_output = std::process::Command::new("git")
            .args(["branch", "--list", "koi/*"])
            .current_dir(dir)
            .output();

        let branches: Vec<String> = match branch_output {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().trim_start_matches("* ").to_string())
                .filter(|l| !l.is_empty())
                .collect(),
            _ => return Ok(ToolResult::err("Failed to list git branches")),
        };

        if branches.is_empty() {
            return Ok(ToolResult::ok("No koi/* branches to merge."));
        }

        let mut results: Vec<String> = Vec::new();
        let _ = std::process::Command::new("git")
            .args(["checkout", "main"])
            .current_dir(dir)
            .output()
            .or_else(|_| {
                std::process::Command::new("git")
                    .args(["checkout", "master"])
                    .current_dir(dir)
                    .output()
            });

        for branch in &branches {
            let merge = std::process::Command::new("git")
                .args([
                    "merge",
                    "--no-ff",
                    branch,
                    "-m",
                    &format!("Merge {}", branch),
                ])
                .current_dir(dir)
                .output();
            match merge {
                Ok(o) if o.status.success() => {
                    results.push(format!("  {} — merged OK", branch));
                }
                Ok(o) => {
                    let _ = std::process::Command::new("git")
                        .args(["merge", "--abort"])
                        .current_dir(dir)
                        .output();
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    results.push(format!(
                        "  {} — CONFLICT (aborted): {}",
                        branch,
                        stderr.trim()
                    ));
                }
                Err(e) => {
                    results.push(format!("  {} — error: {}", branch, e));
                }
            }
        }

        Ok(ToolResult::ok(format!(
            "Merge results for '{}' ({} branches):\n{}",
            session.name,
            branches.len(),
            results.join("\n")
        )))
    }
}
