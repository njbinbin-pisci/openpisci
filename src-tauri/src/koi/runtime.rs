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
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// Global registry of running Koi sessions.
/// Maps koi_id -> notification sender channel.
/// Used to inject @mention notifications into a busy Koi's AgentLoop.
pub static KOI_SESSIONS: Lazy<Mutex<HashMap<String, mpsc::Sender<String>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub struct KoiRuntime {
    bus: Arc<dyn EventBus>,
}

/// Result of a Koi task execution
pub struct KoiExecResult {
    pub success: bool,
    pub reply: String,
    pub result_message_id: Option<i64>,
}

impl KoiRuntime {
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
            )?
        };

        let assign_msg_id = if let Some(psid) = pool_session_id.as_deref() {
            let db = self.db().lock().await;
            let msg = db.insert_pool_message_ext(
                psid,
                assigned_by,
                &format!("@{} {}", koi_def.name, task),
                "task_assign",
                &json!({ "koi_id": &koi_def.id, "priority": priority }).to_string(),
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

        // Claim the todo
        {
            let db = self.db().lock().await;
            db.claim_koi_todo(&todo.id, koi_id)?;
        }

        // Set Koi status to busy
        self.set_koi_status(koi_id, "busy").await;

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

        // Append todo_id and identity reminder to task
        let task_with_meta = format!(
            "{}\n\n[System: You are {name}. Your todo ID for this task is `{id}`. \
             Before starting work, call pool_chat(action=\"read\") to check if a teammate has already done related work or left relevant context. \
             When reading pool chat, focus on what is addressed to you ({name}) specifically — \
             do not confuse other team members' statuses or roles with your own. \
             IMPORTANT: Your workspace is a Git worktree — always use RELATIVE paths (e.g. src/auth/auth.service.ts) for all file_write and file_edit operations. \
             NEVER use absolute paths pointing to the main project directory — doing so bypasses your worktree and corrupts the shared codebase. \
             Mark this todo done ONLY after the actual deliverable is complete and verifiable \
             (code written, file created, review posted, etc.). \
             Writing a plan or having a discussion does NOT count as done. \
             Call pool_org(action=\"complete_todo\", todo_id=\"{id}\", summary=\"<what you accomplished>\") when the real output exists. \
             The summary is REQUIRED and must describe what was actually done (e.g. \"Implemented auth module in src/auth/, 3 files created, all tests pass\"). \
             After completing, if your branch of work is fully done, post [ProjectStatus] ready_for_pisci_review @pisci in pool_chat. \
             Always end with a concise summary of what was accomplished — never end mid-sentence or with a phrase like \"now I will...\". \
             If your result is longer than ~500 words, write it to a file (e.g. kb/reports/<date>-<topic>.md) and post only the file path + a 3-5 sentence summary to pool_chat.]",
            task,
            name = koi_def.name,
            id = &todo.id[..8.min(todo.id.len())]
        );

        // Execute via CallKoiTool with timeout protection (10 min default)
        let exec_result = match tokio::time::timeout(
            std::time::Duration::from_secs(600),
            self.execute_koi_agent(
                &koi_def,
                &task_with_meta,
                canonical_pool_session_id.as_deref(),
                worktree_path.as_deref(),
            ),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!(
                "Koi '{}' timed out after 10 minutes on task: {}",
                koi_def.name,
                task
            )),
        };

        // Post result and update todo
        let (success, raw_reply) = match &exec_result {
            Ok(reply) => (true, reply.clone()),
            Err(e) => (false, format!("Execution error: {}", e)),
        };

        let result_msg_id = if let Some(psid) = canonical_pool_session_id.as_deref() {
            let db = self.db().lock().await;

            // If the raw_reply is empty, the Koi ended with a tool call (e.g. complete_todo).
            // complete_todo already wrote a "result" message to the pool — find it and link it
            // to this todo instead of writing a duplicate empty message.
            if success && raw_reply.trim().is_empty() {
                // Look for the latest result message from this Koi that is not yet linked to a todo
                let existing_id = db
                    .get_latest_unlinked_result_message_id(psid, koi_id)
                    .unwrap_or_default();
                if let Some(msg_id) = existing_id {
                    // Link the existing message to this todo
                    let _ = db.link_pool_message_to_todo(msg_id, &todo.id);
                    drop(db);
                    Some(msg_id)
                } else {
                    drop(db);
                    None
                }
            } else {
                let summary = if raw_reply.chars().count() > 5000 {
                    raw_reply.chars().take(5000).collect::<String>()
                } else {
                    raw_reply.clone()
                };
                let event_type = if success {
                    "task_completed"
                } else {
                    "task_failed"
                };
                let msg = db.insert_pool_message_ext(
                    psid,
                    koi_id,
                    &summary,
                    if success { "result" } else { "status_update" },
                    &json!({ "todo_id": todo.id, "success": success }).to_string(),
                    Some(&todo.id),
                    assign_msg_id,
                    Some(event_type),
                )?;
                self.bus.emit_event(
                    &format!("pool_message_{}", psid),
                    serde_json::to_value(&msg).unwrap_or_default(),
                );
                drop(db);
                Some(msg.id)
            }
        } else {
            None
        };

        // Complete or block the todo
        {
            let db = self.db().lock().await;
            if success {
                db.complete_koi_todo(&todo.id, result_msg_id)?;
            } else {
                db.block_koi_todo(&todo.id, &raw_reply)?;
            }
        }
        self.bus.emit_event(
            "koi_todo_updated",
            json!({
                "id": todo.id, "action": if success { "completed" } else { "blocked" }
            }),
        );

        // Clean up Git worktree if one was created
        if let Some(ref wt) = worktree_path {
            self.cleanup_worktree(wt, &koi_def.name, &task);
        }

        // Set Koi back to idle
        self.set_koi_status(koi_id, "idle").await;

        Ok(KoiExecResult {
            success,
            reply: raw_reply,
            result_message_id: result_msg_id,
        })
    }

    /// Combined assign + execute (backward compat for heartbeat patrol and tests).
    pub async fn assign_and_execute(
        &self,
        koi_id: &str,
        task: &str,
        assigned_by: &str,
        pool_session_id: Option<&str>,
        priority: &str,
    ) -> anyhow::Result<KoiExecResult> {
        let (todo, assign_msg_id) = self
            .assign_task(koi_id, task, assigned_by, pool_session_id, priority)
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

        if koi_def.status != "idle" {
            return Ok(KoiExecResult {
                success: true,
                reply: format!(
                    "{} is busy, notification was injected instead",
                    koi_def.name
                ),
                result_message_id: None,
            });
        }

        self.set_koi_status(koi_id, "busy").await;

        let task = format!(
            "You have been mentioned in the pool chat (pool_id: \"{pool_id}\"). \
            IMPORTANT: You are {name} — keep your own identity in mind while reading messages from others. \
            Use pool_chat(action=\"read\") to see the latest messages, then decide how to respond. \
            When reading the chat, focus on what is addressed TO YOU specifically (your name or @{name}). \
            Do not confuse descriptions of other team members' roles or statuses with your own. \
            Use your judgment: \
            - If someone handed off concrete work to you ({name}): \
              (1) First call pool_org(action=\"get_todos\", pool_id=\"{pool_id}\") to check if a similar unclaimed todo already exists for this work. \
              (2) If no matching todo exists, create one: pool_org(action=\"create_todo\", pool_id=\"{pool_id}\", title=\"...\"). \
              (3) Claim it: pool_org(action=\"claim_todo\", todo_id=\"...\"). \
              (4) Do the work, then mark it complete: pool_org(action=\"complete_todo\", todo_id=\"...\"). \
              (5) After completing, post [ProjectStatus] ready_for_pisci_review @pisci if your branch of work is done. \
            - If you need to ask a clarifying question, do so via pool_chat. \
            - If the messages are status updates, acknowledgements, or peers saying the project is done, \
              you do not need to reply and you do NOT need to create a todo — simply finish. \
            Only send a message if you have something genuinely new or actionable to contribute. \
            Always end with a concise summary of what was accomplished — never end mid-sentence or with a phrase like \"now I will...\". \
            If your response is longer than ~500 words, write the full content to a file (e.g. kb/reports/<date>-<topic>.md) and post only the file path + a 3-5 sentence summary to pool_chat.",
            name = koi_def.name,
            pool_id = pool_session_id
        );

        let exec_result = match tokio::time::timeout(
            std::time::Duration::from_secs(600),
            self.execute_koi_agent(&koi_def, &task, Some(&pool_session_id), None),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!(
                "Koi '{}' timed out checking messages",
                koi_def.name
            )),
        };

        let (success, reply) = match &exec_result {
            Ok(reply) => (true, reply.clone()),
            Err(e) => (false, format!("Error: {}", e)),
        };

        // Write a task_completed/task_failed event to pool so trial polling and
        // other observers can detect that this Koi finished its peer-mention work.
        let result_msg_id = {
            let db = self.db().lock().await;
            let summary = if reply.chars().count() > 5000 {
                reply.chars().take(5000).collect::<String>()
            } else {
                reply.clone()
            };
            let event_type = if success {
                "task_completed"
            } else {
                "task_failed"
            };
            match db.insert_pool_message_ext(
                &pool_session_id,
                koi_id,
                &summary,
                if success { "result" } else { "status_update" },
                "{}",
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
            }
        };

        self.set_koi_status(koi_id, "idle").await;

        Ok(KoiExecResult {
            success,
            reply,
            result_message_id: result_msg_id,
        })
    }

    /// Execute the Koi agent. This is the only part that needs Tauri for now
    /// (because CallKoiTool and AgentLoop need AppHandle/settings).
    /// In tests, override this via a simpler mock.
    async fn execute_koi_agent(
        &self,
        koi_def: &KoiDefinition,
        task: &str,
        pool_session_id: Option<&str>,
        workspace_override: Option<&str>,
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

            // Create notification channel and register in global session registry.
            // Key includes pool_session_id so the same Koi can run in multiple projects concurrently.
            let session_key = format!("{}:{}", koi_def.id, pool_session_id.unwrap_or("default"));
            let (notif_tx, notif_rx) = mpsc::channel::<String>(32);
            {
                let mut sessions = KOI_SESSIONS.lock().await;
                sessions.insert(session_key.clone(), notif_tx);
            }

            let koi_tool = crate::tools::call_koi::CallKoiTool {
                app: app.clone(),
                caller_koi_id: None,
                depth: 0,
                managed_externally: true,
                notification_rx: std::sync::Mutex::new(Some(notif_rx)),
            };

            let ctx = ToolContext {
                // Include pool_session_id in session_id so each project gets an isolated AgentLoop context
                session_id: format!(
                    "koi_runtime_{}_{}",
                    koi_def.id,
                    pool_session_id.unwrap_or("default")
                ),
                workspace_root: std::path::PathBuf::from(&workspace_root),
                bypass_permissions: false,
                settings: tool_settings_data,
                // 0 means "use system default (30)"; non-zero values are user-configured
                max_iterations: Some(if koi_def.max_iterations > 0 {
                    koi_def.max_iterations
                } else {
                    30
                }),
                memory_owner_id: koi_def.id.clone(),
                pool_session_id: pool_session_id.map(String::from),
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

    /// If a Koi's result contains @mentions, spawn a cascading dispatch.
    /// Uses from_tauri pattern (AppHandle + db clone) so the spawned future is Send.
    fn spawn_cascade(&self, koi_id: &str, pool_session_id: &str, reply: &str) {
        if !reply.contains('@') {
            return;
        }
        if let Some(app) = self.bus.app_handle() {
            let app_clone = app.clone();
            let db_clone = self.db().clone();
            let koi_id = koi_id.to_string();
            let psid = pool_session_id.to_string();
            let reply = reply.to_string();
            tokio::spawn(async move {
                let rt = KoiRuntime::from_tauri(app_clone, db_clone);
                if let Err(e) = rt.handle_mention(&koi_id, &psid, &reply).await {
                    tracing::warn!("Cascade @mention dispatch failed: {}", e);
                }
            });
        }
    }

    /// Handle an @mention or @all in the chat pool.
    /// Behavior depends on who is mentioning:
    /// - Pisci/user: direct task assignment (creates todo + executes)
    /// - Another Koi: peer request — no todo, just notification/activation
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

        for koi in &kois {
            if koi.status == "offline" || koi.id == sender_id {
                continue;
            }
            let mention = format!("@{}", koi.name);
            if !mention_all && !content.contains(&mention) {
                continue;
            }

            if sender_is_koi {
                // Peer Koi @mention: just a message, not a command.
                // Check if target is busy in THIS project → inject notification; idle → activate to check messages.
                let koi_session_key = format!("{}:{}", koi.id, pool_session_id);
                let sessions = KOI_SESSIONS.lock().await;
                if let Some(tx) = sessions.get(&koi_session_key) {
                    let sender_name = kois
                        .iter()
                        .find(|k| k.id == sender_id)
                        .map(|k| k.name.as_str())
                        .unwrap_or(sender_id);
                    let notification = format!(
                        "[Pool Notification] @{} from {} (in pool chat):\n{}\n\n\
                         You can use pool_chat to respond. \
                         If you want to take on this request, create a todo for yourself. \
                         You may also continue your current work and respond later.",
                        koi.name, sender_name, content
                    );
                    let _ = tx.send(notification).await;
                    tracing::info!("Injected @mention notification to busy Koi '{}'", koi.name);
                    results.push(KoiExecResult {
                        success: true,
                        reply: format!("Notification sent to busy Koi '{}'", koi.name),
                        result_message_id: None,
                    });
                } else {
                    drop(sessions);
                    let result = self.activate_for_messages(&koi.id, pool_session_id).await?;
                    self.spawn_cascade(&koi.id, pool_session_id, &result.reply);
                    results.push(result);
                }
            } else {
                // Pisci/user @mention: direct task assignment
                let task = content
                    .split(&mention)
                    .nth(1)
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(content);

                let result = self
                    .assign_and_execute(&koi.id, task, sender_id, Some(pool_session_id), "medium")
                    .await?;
                self.spawn_cascade(&koi.id, pool_session_id, &result.reply);
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
        let todos = {
            let db = self.db().lock().await;
            db.list_koi_todos(None)?
        };

        let pending: Vec<&KoiTodo> = todos
            .iter()
            .filter(|t| {
                t.status == "todo"
                    && t.claimed_by.is_none()
                    && pool_session_id
                        .map_or(true, |psid| t.pool_session_id.as_deref() == Some(psid))
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
