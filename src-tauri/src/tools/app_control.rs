use crate::commands::config::tools::BuiltinToolInfo;
use crate::skills::loader::SkillLoader;
use crate::store::{settings::SshServerConfig, Database, Settings};
use crate::tools::user_tool::UserToolManifest;
use async_trait::async_trait;
use pisci_kernel::agent::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;
use tracing::{info, warn};

const CLAWHUB_API: &str = "https://clawhub.ai";

fn validate_koi_name(name: &str) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        anyhow::bail!("Koi name cannot be empty.");
    }
    if name.chars().any(char::is_whitespace) {
        anyhow::bail!("Koi name cannot contain spaces or other whitespace characters.");
    }
    if name.chars().any(is_disallowed_koi_name_char) {
        anyhow::bail!("Koi name cannot contain emoji or other pictographic characters.");
    }
    Ok(())
}

fn is_disallowed_koi_name_char(ch: char) -> bool {
    let cp = ch as u32;
    matches!(
        cp,
        0x200D
            | 0xFE0F
            | 0x1F1E6..=0x1F1FF
            | 0x1F300..=0x1FAFF
            | 0x2600..=0x27BF
            | 0x2300..=0x23FF
    )
}

/// GET with automatic retry on 429 / 5xx (exponential back-off, max 3 retries).
async fn clawhub_get_with_retry(
    client: &reqwest::Client,
    url: &str,
    max_retries: u32,
) -> anyhow::Result<reqwest::Response> {
    let base_delay_ms: u64 = 1000;
    let mut attempt = 0u32;
    loop {
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Network error: {}", e))?;
        let status = resp.status();
        if status.is_success() || (status.is_client_error() && status.as_u16() != 429) {
            return Ok(resp);
        }
        if attempt >= max_retries {
            return Ok(resp);
        }
        let retry_after_ms = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .map(|s| s * 1000)
            .unwrap_or(0);
        let backoff_ms = if retry_after_ms > 0 {
            retry_after_ms.min(30_000)
        } else {
            (base_delay_ms * (1u64 << attempt.min(4))).min(16_000)
        };
        warn!(
            "ClawHub {} for '{}', retrying in {}ms ({}/{})",
            status,
            url,
            backoff_ms,
            attempt + 1,
            max_retries
        );
        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
        attempt += 1;
    }
}

pub struct AppControlTool {
    pub db: Arc<Mutex<Database>>,
    pub settings: Arc<Mutex<Settings>>,
    pub app_data_dir: PathBuf,
    pub app_handle: Option<AppHandle>,
}

impl AppControlTool {
    /// Notify the frontend that settings have changed so it can re-fetch and
    /// refresh the Settings page without requiring a manual restart.
    fn emit_settings_changed(&self) {
        if let Some(app) = &self.app_handle {
            let _ = app.emit("settings_changed", ());
        }
    }
}

#[async_trait]
impl Tool for AppControlTool {
    fn name(&self) -> &str {
        "app_control"
    }

    fn description(&self) -> &str {
        "Control OpenPisci application: manage scheduled tasks, settings, UI/windows, built-in tools, user tools, runtimes, SSH servers, and skills.\
         \n\nACTIONS — Scheduled Tasks:\
         \n- 'task_list': List all scheduled tasks (name, cron, status, last run).\
         \n- 'task_create': Create a task. Required: name, cron_expression (5-field), task_prompt. Optional: description.\
         \n- 'task_update': Update a task by id. Optional: name, cron_expression, task_prompt, status (active/paused).\
         \n- 'task_delete': Delete a task by id.\
         \n- 'task_run_now': Immediately trigger a task by id.\
         \
         \n\nACTIONS — Settings:\
         \n- 'settings_get': Read current settings (provider, model, max_tokens, etc.).\
         \n- 'settings_set': Update settings. Supports LLM, agent, workspace, security, vision, IM gateway, and email fields.\
         \
         \n\nACTIONS — Runtimes:\
         \n- 'runtime_check': Detect Node.js, npm, Python, pip, Git, and browser availability.\
         \n- 'runtime_set_path': Override or clear a runtime executable path. Required: runtime_key. Optional: exe_path (empty string clears override).\
         \
         \n\nACTIONS — SSH Servers:\
         \n- 'ssh_list': List configured SSH servers.\
         \n- 'ssh_upsert': Create or update an SSH server entry. Required: ssh_id, ssh_host, ssh_username. Optional: ssh_label, ssh_port, ssh_password, ssh_private_key.\
         \n- 'ssh_delete': Delete an SSH server entry by id. Required: ssh_id.\
         \
         \n\nACTIONS — UI / Window:\
         \n- 'ui_set_theme': Switch the app theme and sync the native border color. Required: theme (violet|gold).\
         \n- 'ui_set_theme_border': Only set the native window border theme color. Required: theme (violet|gold).\
         \n- 'ui_enter_minimal_mode': Hide main window and show the floating overlay.\
         \n- 'ui_exit_minimal_mode': Exit minimal mode and restore the main window.\
         \n- 'window_move': Move the main or overlay window. Required: window_target (main|overlay). Use x+y or position_preset=bottom_right.\
         \n- 'notify_user': Show a toast notification in the main UI window to surface a situation to the human user.\
         \n  Required: message. Optional: title (default 'Pisci'), level (info|warning|error|critical, default 'info'),\
         \n  pool_id (to link the toast to a specific pool), duration_ms (0 = persistent until dismissed).\
         \n  Use sparingly. Typical cases: human escalation after unrecoverable failures, Pisci needs an explicit user decision,\
         \n  or a long-running project reached a milestone the user should know about. Do NOT use for chatty progress updates.\
         \
         \n\nACTIONS — Built-in Tools:\
         \n- 'builtin_tool_list': List built-in tools and whether each one is enabled.\
         \n- 'builtin_tool_toggle': Enable or disable a built-in tool. Required: tool_name, enabled.\
         \
         \n\nACTIONS — User Tools:\
         \n- 'user_tool_list': List installed user tools.\
         \n- 'user_tool_config_get': Read config for a user tool (passwords are masked). Required: tool_name.\
         \n- 'user_tool_config_set': Create/update config for a user tool. Required: tool_name, config (object). Merges with existing values.\
         \
         \n\nACTIONS — Skills:\
         \n- 'skill_list': List installed skills with enabled status.\
         \n- 'skill_search': Search ClawHub marketplace. Required: query (use empty string for top skills).\
         \n- 'skill_install': Install a skill from a ClawHub slug or direct URL. Required: source (slug or URL).\
         \n- 'skill_toggle': Enable or disable an installed skill. Required: skill_id, enabled (bool).\
         \n- 'skill_uninstall': Remove an installed skill by name. Required: skill_name.\
         \
         \n\nACTIONS — Koi Agents:\
         \n- 'koi_list': List all Koi agents with their id, name, role, icon, status, and description.\
         \n- 'koi_create': Create a new Koi agent. Required: name, role, system_prompt. Optional: icon (emoji), color (hex), description.\
         \n  Only create a Koi when the user explicitly requests it. Do NOT create Koi proactively.\
         \n  IMPORTANT: The 'name' field must be plain text only, with no spaces, no emoji, and no other pictographic characters. Emoji belongs in the 'icon' field only.\
         \n- 'koi_update': Update an existing Koi agent. Required: koi_id. Optional: name, role, icon, color, system_prompt, description. Only the provided fields are changed. If 'name' is provided, it must also follow the no-spaces / no-emoji rule.\
         \n- 'koi_delete': Delete a Koi agent by id. Required: koi_id. Use with caution — this permanently removes the Koi and all its memories.\
         \
         \n\nCron format (5 fields): <min> <hour> <day> <month> <weekday>\
         \nExamples: '0 * * * *'=every hour, '0 9 * * 1-5'=9am weekdays, '*/30 * * * *'=every 30min"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "task_list", "task_create", "task_update", "task_delete", "task_run_now",
                        "settings_get", "settings_set",
                        "runtime_check", "runtime_set_path",
                        "ssh_list", "ssh_upsert", "ssh_delete",
                        "ui_set_theme", "ui_set_theme_border", "ui_enter_minimal_mode", "ui_exit_minimal_mode", "window_move",
                        "notify_user",
                        "builtin_tool_list", "builtin_tool_toggle",
                        "user_tool_list", "user_tool_config_get", "user_tool_config_set",
                        "skill_list", "skill_search", "skill_install", "skill_toggle", "skill_uninstall",
                        "koi_list", "koi_create", "koi_update", "koi_delete"
                    ]
                },
                // Koi fields
                "koi_id": { "type": "string", "description": "Koi agent ID (for koi_update / koi_delete)" },
                "icon": { "type": "string", "description": "Emoji icon for the Koi (e.g. '🐬'). Defaults to '🐟' if omitted. Put emoji here, NOT in the name field." },
                "color": { "type": "string", "description": "Hex color for the Koi (e.g. '#22c55e'). Defaults to a random color if omitted." },
                "role": { "type": "string", "description": "Short role label for the Koi (e.g. 'Backend Engineer')" },
                "system_prompt": { "type": "string", "description": "System prompt defining the Koi's behavior and expertise" },
                // Task fields
                "id": { "type": "string", "description": "Task ID (for task_update/delete/run_now)" },
                "name": { "type": "string", "description": "Task name (required for task_create)" },
                "description": { "type": "string" },
                "cron_expression": { "type": "string", "description": "5-field cron, e.g. '0 * * * *'" },
                "task_prompt": { "type": "string", "description": "Prompt sent to agent when task fires" },
                "status": { "type": "string", "enum": ["active", "paused"] },
                // Settings fields
                "provider": { "type": "string", "description": "LLM provider: anthropic|openai|custom|deepseek|qwen|minimax|zhipu|kimi" },
                "model": { "type": "string" },
                "api_key": { "type": "string", "description": "API key for the provider" },
                "custom_base_url": { "type": "string" },
                "max_tokens": { "type": "integer" },
                "context_window": { "type": "integer", "description": "Context window tokens (0=auto)" },
                "max_iterations": { "type": "integer" },
                "policy_mode": { "type": "string", "enum": ["strict", "balanced", "dev"] },
                "workspace_root": { "type": "string" },
                "allow_outside_workspace": { "type": "boolean" },
                "confirm_shell_commands": { "type": "boolean" },
                "confirm_file_writes": { "type": "boolean" },
                "language": { "type": "string", "enum": ["zh", "en"] },
                "browser_headless": { "type": "boolean" },
                "tool_rate_limit_per_minute": { "type": "integer" },
                "vision_enabled": { "type": "boolean" },
                "heartbeat_enabled": { "type": "boolean" },
                "heartbeat_interval_mins": { "type": "integer" },
                "heartbeat_prompt": { "type": "string" },
                "feishu_app_id": { "type": "string" },
                "feishu_app_secret": { "type": "string" },
                "feishu_domain": { "type": "string" },
                "feishu_enabled": { "type": "boolean" },
                "wecom_corp_id": { "type": "string" },
                "wecom_agent_secret": { "type": "string" },
                "wecom_agent_id": { "type": "string" },
                "wecom_enabled": { "type": "boolean" },
                "dingtalk_app_key": { "type": "string" },
                "dingtalk_app_secret": { "type": "string" },
                "dingtalk_enabled": { "type": "boolean" },
                "telegram_bot_token": { "type": "string" },
                "telegram_enabled": { "type": "boolean" },
                "slack_webhook_url": { "type": "string" },
                "slack_enabled": { "type": "boolean" },
                "discord_webhook_url": { "type": "string" },
                "discord_enabled": { "type": "boolean" },
                "teams_webhook_url": { "type": "string" },
                "teams_enabled": { "type": "boolean" },
                "matrix_homeserver": { "type": "string" },
                "matrix_access_token": { "type": "string" },
                "matrix_room_id": { "type": "string" },
                "matrix_enabled": { "type": "boolean" },
                "webhook_outbound_url": { "type": "string" },
                "webhook_auth_token": { "type": "string" },
                "webhook_enabled": { "type": "boolean" },
                "wecom_inbox_file": { "type": "string" },
                "wechat_enabled": { "type": "boolean" },
                "wechat_gateway_token": { "type": "string" },
                "wechat_gateway_port": { "type": "integer" },
                "smtp_host": { "type": "string" },
                "smtp_port": { "type": "integer" },
                "smtp_username": { "type": "string" },
                "smtp_password": { "type": "string" },
                "imap_host": { "type": "string" },
                "imap_port": { "type": "integer" },
                "smtp_from_name": { "type": "string" },
                "email_enabled": { "type": "boolean" },
                // Skill fields
                "query": { "type": "string", "description": "Search query for skill_search" },
                "source": { "type": "string", "description": "ClawHub slug or direct URL for skill_install" },
                "skill_id": { "type": "string", "description": "Skill ID for skill_toggle (from skill_list)" },
                "skill_name": { "type": "string", "description": "Skill name for skill_uninstall" },
                "enabled": { "type": "boolean", "description": "Enable/disable for skill_toggle" },
                "tool_name": { "type": "string", "description": "Built-in or user tool name" },
                "config": { "type": "object", "description": "User tool configuration object for user_tool_config_set" },
                "runtime_key": { "type": "string", "description": "Runtime key for runtime_set_path, e.g. python|node|npm|pip|git" },
                "exe_path": { "type": "string", "description": "Absolute executable path for runtime_set_path; empty string clears override" },
                "ssh_id": { "type": "string", "description": "SSH server id for ssh_upsert/ssh_delete" },
                "ssh_label": { "type": "string", "description": "Optional SSH display label" },
                "ssh_host": { "type": "string", "description": "SSH host name or IP" },
                "ssh_port": { "type": "integer", "description": "SSH port (default 22)" },
                "ssh_username": { "type": "string", "description": "SSH username" },
                "ssh_password": { "type": "string", "description": "SSH password (optional; empty means unchanged when updating)" },
                "ssh_private_key": { "type": "string", "description": "SSH private key PEM (optional; empty means unchanged when updating)" },
                "theme": { "type": "string", "description": "Theme name for ui_set_theme/ui_set_theme_border: violet|gold" },
                "window_target": { "type": "string", "description": "Window to move: main|overlay" },
                "x": { "type": "integer", "description": "Absolute screen X for window_move" },
                "y": { "type": "integer", "description": "Absolute screen Y for window_move" },
                "position_preset": { "type": "string", "description": "Window position preset, currently supports: bottom_right" },
                "title": { "type": "string", "description": "Toast title for notify_user (default 'Pisci')" },
                "message": { "type": "string", "description": "Toast body text for notify_user" },
                "level": { "type": "string", "enum": ["info", "warning", "error", "critical"], "description": "Toast severity for notify_user (default 'info')" },
                "pool_id": { "type": "string", "description": "Optional pool id to associate a notify_user toast with a specific project" },
                "duration_ms": { "type": "integer", "description": "Auto-dismiss duration in ms for notify_user. 0 = persistent until the user closes it (use for level=critical)." }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let action = match input["action"].as_str() {
            Some(a) => a,
            None => return Ok(ToolResult::err("'action' field is required")),
        };

        match action {
            "task_list"       => self.task_list().await,
            "task_create"     => self.task_create(&input).await,
            "task_update"     => self.task_update(&input).await,
            "task_delete"     => self.task_delete(&input).await,
            "task_run_now"    => self.task_run_now(&input).await,
            "settings_get"    => self.settings_get().await,
            "settings_set"    => self.settings_set(&input).await,
            "runtime_check"   => self.runtime_check().await,
            "runtime_set_path"=> self.runtime_set_path(&input).await,
            "ssh_list"        => self.ssh_list().await,
            "ssh_upsert"      => self.ssh_upsert(&input).await,
            "ssh_delete"      => self.ssh_delete(&input).await,
            "ui_set_theme" => self.ui_set_theme(&input).await,
            "ui_set_theme_border" => self.ui_set_theme_border(&input).await,
            "ui_enter_minimal_mode" => self.ui_enter_minimal_mode().await,
            "ui_exit_minimal_mode" => self.ui_exit_minimal_mode().await,
            "window_move" => self.window_move(&input).await,
            "notify_user" => self.notify_user(&input).await,
            "builtin_tool_list" => self.builtin_tool_list().await,
            "builtin_tool_toggle" => self.builtin_tool_toggle(&input).await,
            "user_tool_list" => self.user_tool_list().await,
            "user_tool_config_get" => self.user_tool_config_get(&input).await,
            "user_tool_config_set" => self.user_tool_config_set(&input).await,
            "skill_list"      => self.skill_list().await,
            "skill_search"    => self.skill_search(&input).await,
            "skill_install"   => self.skill_install(&input).await,
            "skill_toggle"    => self.skill_toggle(&input).await,
            "skill_uninstall" => self.skill_uninstall(&input).await,
            "koi_list"        => self.koi_list().await,
            "koi_create"      => self.koi_create(&input).await,
            "koi_update"      => self.koi_update(&input).await,
            "koi_delete"      => self.koi_delete(&input).await,
            other => Ok(ToolResult::err(format!(
                "Unknown action '{}'. Valid actions: task_list, task_create, task_update, task_delete, task_run_now, settings_get, settings_set, runtime_check, runtime_set_path, ssh_list, ssh_upsert, ssh_delete, ui_set_theme, ui_set_theme_border, ui_enter_minimal_mode, ui_exit_minimal_mode, window_move, notify_user, builtin_tool_list, builtin_tool_toggle, user_tool_list, user_tool_config_get, user_tool_config_set, skill_list, skill_search, skill_install, skill_toggle, skill_uninstall, koi_list, koi_create, koi_update, koi_delete",
                other
            ))),
        }
    }
}

// ── Scheduled Tasks ───────────────────────────────────────────────────────────

impl AppControlTool {
    async fn task_list(&self) -> anyhow::Result<ToolResult> {
        let db = self.db.lock().await;
        match db.list_tasks() {
            Ok(tasks) if tasks.is_empty() => Ok(ToolResult::ok("No scheduled tasks configured.")),
            Ok(tasks) => {
                let lines: Vec<String> = tasks.iter().map(|t| {
                    format!(
                        "ID: {}\n  Name: {}\n  Cron: {}\n  Status: {}\n  Last run: {}\n  Run count: {}\n  Prompt: {}",
                        t.id, t.name, t.cron_expression, t.status,
                        t.last_run_at
                            .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
                            .unwrap_or_else(|| "never".to_string()),
                        t.run_count,
                        if t.task_prompt.chars().count() > 80 {
                            format!("{}…", t.task_prompt.chars().take(80).collect::<String>())
                        } else {
                            t.task_prompt.clone()
                        }
                    )
                }).collect();
                Ok(ToolResult::ok(format!(
                    "{} task(s):\n\n{}",
                    tasks.len(),
                    lines.join("\n\n")
                )))
            }
            Err(e) => Ok(ToolResult::err(format!("Failed to list tasks: {}", e))),
        }
    }

    async fn task_create(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let name = match input["name"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(n) => n,
            None => return Ok(ToolResult::err("'name' is required for task_create")),
        };
        let cron = match input["cron_expression"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
        {
            Some(c) => c,
            None => {
                return Ok(ToolResult::err(
                    "'cron_expression' is required (5 fields, e.g. '0 * * * *' = every hour)",
                ))
            }
        };
        let prompt = match input["task_prompt"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
        {
            Some(p) => p,
            None => return Ok(ToolResult::err("'task_prompt' is required for task_create")),
        };
        let description = input["description"].as_str();

        if cron.split_whitespace().count() != 5 {
            return Ok(ToolResult::err(format!(
                "Invalid cron_expression '{}': must have exactly 5 fields. \
                 Example: '0 * * * *' = every hour, '0 9 * * 1-5' = 9am weekdays.",
                cron
            )));
        }

        let db = self.db.lock().await;
        match db.create_task(name, description, cron, prompt) {
            Ok(task) => Ok(ToolResult::ok(format!(
                "Scheduled task created.\nID: {}\nName: {}\nCron: {}\nStatus: active",
                task.id, task.name, task.cron_expression
            ))),
            Err(e) => Ok(ToolResult::err(format!("Failed to create task: {}", e))),
        }
    }

    async fn task_update(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let id = match input["id"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(i) => i,
            None => return Ok(ToolResult::err("'id' is required for task_update")),
        };
        let name = input["name"].as_str();
        let cron = input["cron_expression"].as_str();
        let prompt = input["task_prompt"].as_str();
        let status = input["status"].as_str();

        if name.is_none() && cron.is_none() && prompt.is_none() && status.is_none() {
            return Ok(ToolResult::err(
                "task_update requires at least one field: name, cron_expression, task_prompt, or status"
            ));
        }
        if let Some(c) = cron {
            if c.split_whitespace().count() != 5 {
                return Ok(ToolResult::err(format!("Invalid cron_expression '{}'", c)));
            }
        }

        let db = self.db.lock().await;
        match db.get_task(id) {
            Ok(None) => return Ok(ToolResult::err(format!("Task '{}' not found", id))),
            Err(e) => return Ok(ToolResult::err(format!("Failed to look up task: {}", e))),
            Ok(Some(_)) => {}
        }
        match db.update_task(id, name, cron, prompt, status) {
            Ok(_) => Ok(ToolResult::ok(format!("Task '{}' updated.", id))),
            Err(e) => Ok(ToolResult::err(format!("Failed to update task: {}", e))),
        }
    }

    async fn task_delete(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let id = match input["id"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(i) => i,
            None => return Ok(ToolResult::err("'id' is required for task_delete")),
        };
        let db = self.db.lock().await;
        match db.get_task(id) {
            Ok(None) => Ok(ToolResult::err(format!("Task '{}' not found", id))),
            Err(e) => Ok(ToolResult::err(format!("Failed to look up task: {}", e))),
            Ok(Some(t)) => match db.delete_task(id) {
                Ok(_) => Ok(ToolResult::ok(format!(
                    "Task '{}' ('{}') deleted.",
                    id, t.name
                ))),
                Err(e) => Ok(ToolResult::err(format!("Failed to delete task: {}", e))),
            },
        }
    }

    async fn task_run_now(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let id = match input["id"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(i) => i,
            None => return Ok(ToolResult::err("'id' is required for task_run_now")),
        };
        let db = self.db.lock().await;
        match db.get_task(id) {
            Ok(None) => Ok(ToolResult::err(format!("Task '{}' not found", id))),
            Err(e)   => Ok(ToolResult::err(format!("Failed to look up task: {}", e))),
            Ok(Some(t)) => Ok(ToolResult::ok(format!(
                "Task '{}' ('{}') queued for immediate execution. Monitor progress in the Scheduler tab.",
                id, t.name
            ))),
        }
    }

    // ── Settings ──────────────────────────────────────────────────────────────

    async fn settings_get(&self) -> anyhow::Result<ToolResult> {
        let s = self.settings.lock().await;
        let mask = |key: &str| -> String {
            if key.is_empty() {
                "(not set)".to_string()
            } else if key.len() <= 4 {
                "****".to_string()
            } else {
                format!("****{}", &key[key.len() - 4..])
            }
        };
        let provider_key = match s.provider.as_str() {
            "openai" | "custom" => mask(&s.openai_api_key),
            "deepseek" => mask(&s.deepseek_api_key),
            "qwen" | "tongyi" => mask(&s.qwen_api_key),
            "minimax" => mask(&s.minimax_api_key),
            "zhipu" => mask(&s.zhipu_api_key),
            "kimi" | "moonshot" => mask(&s.kimi_api_key),
            _ => mask(&s.anthropic_api_key),
        };
        let configured = |s: &str| if s.is_empty() { "(not set)" } else { "(set)" };
        Ok(ToolResult::ok(format!(
            "Current settings:\n\
             LLM:\n\
             - provider: {provider}\n\
             - model: {model}\n\
             - custom_base_url: {base_url}\n\
             - api_key ({provider}): {key}\n\
             - max_tokens: {max_tokens}\n\
             - context_window: {ctx_win} (0=auto)\n\
             \nAgent:\n\
             - max_iterations: {max_iter}\n\
             - policy_mode: {policy}\n\
             - workspace_root: {workspace}\n\
             - allow_outside_workspace: {allow_outside_workspace}\n\
             - tool_rate_limit_per_minute: {tool_rate_limit_per_minute}\n\
             - confirm_shell_commands: {confirm_shell}\n\
             - confirm_file_writes: {confirm_file}\n\
             \nUI:\n\
             - language: {lang}\n\
             - browser_headless: {headless}\n\
             - vision_enabled: {vision_enabled}\n\
             \nHeartbeat:\n\
             - heartbeat_enabled: {hb_enabled}\n\
             - heartbeat_interval_mins: {hb_interval}\n\
             - heartbeat_prompt: {hb_prompt}\n\
             \nIM Gateways:\n\
             - feishu_enabled: {feishu_enabled}\n\
             - feishu_app_id: {feishu_app_id}\n\
             - feishu_app_secret: {feishu_app_secret}\n\
             - feishu_domain: {feishu_domain}\n\
             - dingtalk_enabled: {dingtalk_enabled}\n\
             - dingtalk_app_key: {dingtalk_app_key}\n\
             - dingtalk_app_secret: {dingtalk_app_secret}\n\
             - wechat_enabled: {wechat_enabled}\n\
             - wechat_gateway_port: {wechat_gateway_port}\n\
             - wecom_enabled: {wecom_enabled}\n\
             - wecom_corp_id: {wecom_corp_id}\n\
             - telegram_enabled: {telegram_enabled}\n\
             - telegram_bot_token: {telegram_bot_token}\
             {ssh_section}",
            provider = s.provider,
            model = s.model,
            base_url = if s.custom_base_url.is_empty() {
                "(none)".to_string()
            } else {
                s.custom_base_url.clone()
            },
            key = provider_key,
            max_tokens = s.max_tokens,
            ctx_win = s.context_window,
            max_iter = s.max_iterations,
            policy = s.policy_mode,
            workspace = s.workspace_root,
            allow_outside_workspace = s.allow_outside_workspace,
            tool_rate_limit_per_minute = s.tool_rate_limit_per_minute,
            confirm_shell = s.confirm_shell_commands,
            confirm_file = s.confirm_file_writes,
            lang = s.language,
            headless = s.browser_headless,
            vision_enabled = s.vision_enabled,
            hb_enabled = s.heartbeat_enabled,
            hb_interval = s.heartbeat_interval_mins,
            hb_prompt = s.heartbeat_prompt,
            feishu_enabled = s.feishu_enabled,
            feishu_app_id = configured(&s.feishu_app_id),
            feishu_app_secret = configured(&s.feishu_app_secret),
            feishu_domain = s.feishu_domain,
            dingtalk_enabled = s.dingtalk_enabled,
            dingtalk_app_key = configured(&s.dingtalk_app_key),
            dingtalk_app_secret = configured(&s.dingtalk_app_secret),
            wechat_enabled = s.wechat_enabled,
            wechat_gateway_port = s.wechat_gateway_port,
            wecom_enabled = s.wecom_enabled,
            wecom_corp_id = configured(&s.wecom_corp_id),
            telegram_enabled = s.telegram_enabled,
            telegram_bot_token = configured(&s.telegram_bot_token),
            ssh_section = if s.ssh_servers.is_empty() {
                "\n\nSSH Servers:\n- (none configured — add servers in Settings > SSH Servers)"
                    .to_string()
            } else {
                let lines: Vec<String> = s
                    .ssh_servers
                    .iter()
                    .map(|srv| {
                        let auth = if !srv.password.is_empty() {
                            "password"
                        } else if !srv.private_key.is_empty() {
                            "key"
                        } else {
                            "no-auth"
                        };
                        format!(
                            "  - '{}' ({}): {}@{}:{} [{}]",
                            srv.id,
                            if srv.label.is_empty() {
                                &srv.id
                            } else {
                                &srv.label
                            },
                            srv.username,
                            srv.host,
                            srv.port,
                            auth
                        )
                    })
                    .collect();
                format!(
                    "\n\nSSH Servers ({} configured):\n{}",
                    s.ssh_servers.len(),
                    lines.join("\n")
                )
            },
        )))
    }

    async fn settings_set(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let mut s = self.settings.lock().await;
        let mut changed: Vec<String> = Vec::new();

        macro_rules! apply_str {
            ($field:ident, $label:expr) => {
                if let Some(v) = input[stringify!($field)].as_str() {
                    s.$field = v.to_string();
                    changed.push(format!("{} = \"{}\"", $label, v));
                }
            };
        }
        macro_rules! apply_bool {
            ($field:ident, $label:expr) => {
                if let Some(v) = input[stringify!($field)].as_bool() {
                    s.$field = v;
                    changed.push(format!("{} = {}", $label, v));
                }
            };
        }
        macro_rules! apply_u32 {
            ($field:ident, $label:expr) => {
                if let Some(v) = input[stringify!($field)].as_u64() {
                    s.$field = v as u32;
                    changed.push(format!("{} = {}", $label, v));
                }
            };
        }

        apply_str!(provider, "provider");
        apply_str!(model, "model");
        apply_str!(custom_base_url, "custom_base_url");
        apply_u32!(max_tokens, "max_tokens");
        apply_u32!(context_window, "context_window");
        apply_u32!(max_iterations, "max_iterations");
        apply_str!(policy_mode, "policy_mode");
        apply_str!(workspace_root, "workspace_root");
        apply_bool!(allow_outside_workspace, "allow_outside_workspace");
        apply_bool!(confirm_shell_commands, "confirm_shell_commands");
        apply_bool!(confirm_file_writes, "confirm_file_writes");
        apply_str!(language, "language");
        apply_bool!(browser_headless, "browser_headless");
        apply_u32!(tool_rate_limit_per_minute, "tool_rate_limit_per_minute");
        apply_bool!(vision_enabled, "vision_enabled");
        apply_bool!(heartbeat_enabled, "heartbeat_enabled");
        apply_u32!(heartbeat_interval_mins, "heartbeat_interval_mins");
        apply_str!(heartbeat_prompt, "heartbeat_prompt");
        apply_str!(feishu_app_id, "feishu_app_id");
        apply_str!(feishu_app_secret, "feishu_app_secret");
        apply_str!(feishu_domain, "feishu_domain");
        apply_bool!(feishu_enabled, "feishu_enabled");
        apply_str!(wecom_corp_id, "wecom_corp_id");
        apply_str!(wecom_agent_secret, "wecom_agent_secret");
        apply_str!(wecom_agent_id, "wecom_agent_id");
        apply_bool!(wecom_enabled, "wecom_enabled");
        apply_str!(dingtalk_app_key, "dingtalk_app_key");
        apply_str!(dingtalk_app_secret, "dingtalk_app_secret");
        apply_bool!(dingtalk_enabled, "dingtalk_enabled");
        apply_str!(telegram_bot_token, "telegram_bot_token");
        apply_bool!(telegram_enabled, "telegram_enabled");
        apply_str!(slack_webhook_url, "slack_webhook_url");
        apply_bool!(slack_enabled, "slack_enabled");
        apply_str!(discord_webhook_url, "discord_webhook_url");
        apply_bool!(discord_enabled, "discord_enabled");
        apply_str!(teams_webhook_url, "teams_webhook_url");
        apply_bool!(teams_enabled, "teams_enabled");
        apply_str!(matrix_homeserver, "matrix_homeserver");
        apply_str!(matrix_access_token, "matrix_access_token");
        apply_str!(matrix_room_id, "matrix_room_id");
        apply_bool!(matrix_enabled, "matrix_enabled");
        apply_str!(webhook_outbound_url, "webhook_outbound_url");
        apply_str!(webhook_auth_token, "webhook_auth_token");
        apply_bool!(webhook_enabled, "webhook_enabled");
        apply_str!(wecom_inbox_file, "wecom_inbox_file");
        apply_bool!(wechat_enabled, "wechat_enabled");
        apply_str!(wechat_gateway_token, "wechat_gateway_token");
        if let Some(v) = input["wechat_gateway_port"].as_u64() {
            s.wechat_gateway_port = v as u16;
        }
        apply_str!(smtp_host, "smtp_host");
        if let Some(v) = input["smtp_port"].as_u64() {
            s.smtp_port = v as u16;
            changed.push(format!("smtp_port = {}", v));
        }
        apply_str!(smtp_username, "smtp_username");
        apply_str!(smtp_password, "smtp_password");
        apply_str!(imap_host, "imap_host");
        if let Some(v) = input["imap_port"].as_u64() {
            s.imap_port = v as u16;
            changed.push(format!("imap_port = {}", v));
        }
        apply_str!(smtp_from_name, "smtp_from_name");
        apply_bool!(email_enabled, "email_enabled");

        // API key — stored into the correct field based on provider
        if let Some(key) = input["api_key"].as_str().filter(|k| !k.trim().is_empty()) {
            let target = input["provider"]
                .as_str()
                .unwrap_or(&s.provider)
                .to_string();
            match target.as_str() {
                "openai" | "custom" => {
                    s.openai_api_key = key.to_string();
                }
                "deepseek" => {
                    s.deepseek_api_key = key.to_string();
                }
                "qwen" | "tongyi" => {
                    s.qwen_api_key = key.to_string();
                }
                "minimax" => {
                    s.minimax_api_key = key.to_string();
                }
                "zhipu" => {
                    s.zhipu_api_key = key.to_string();
                }
                "kimi" | "moonshot" => {
                    s.kimi_api_key = key.to_string();
                }
                _ => {
                    s.anthropic_api_key = key.to_string();
                }
            }
            changed.push(format!(
                "api_key ({}) = ****{}",
                target,
                if key.len() > 4 {
                    &key[key.len() - 4..]
                } else {
                    "****"
                }
            ));
        }

        if changed.is_empty() {
            return Ok(ToolResult::err(
                "No recognized fields provided. Use settings_get to see available fields.",
            ));
        }

        match s.save() {
            Ok(_) => {
                self.emit_settings_changed();
                Ok(ToolResult::ok(format!(
                    "Settings saved. Changed:\n{}",
                    changed
                        .iter()
                        .map(|c| format!("  - {}", c))
                        .collect::<Vec<_>>()
                        .join("\n")
                )))
            }
            Err(e) => Ok(ToolResult::err(format!("Failed to save settings: {}", e))),
        }
    }

    async fn runtime_check(&self) -> anyhow::Result<ToolResult> {
        let settings = self.settings.lock().await;
        let custom_paths = settings.runtime_paths.clone();
        drop(settings);

        let mut lines = Vec::new();
        for (key, label, hint) in [
            ("node", "Node.js", "Required for npm-based skills"),
            ("npm", "npm", "Package manager for Node.js skills"),
            ("python", "Python", "Required for Python-based skills"),
            ("pip", "pip", "Package manager for Python skills"),
            ("git", "Git", "Required for git-based skill sources"),
        ] {
            let override_path = custom_paths.get(key).cloned().unwrap_or_default();
            let detected = if !override_path.is_empty() {
                probe_command(&override_path, &["--version"])
            } else if key == "python" {
                probe_command("python", &["--version"])
                    .or_else(|| probe_command("python3", &["--version"]))
            } else if key == "pip" {
                probe_command("pip", &["--version"])
                    .or_else(|| probe_command("pip3", &["--version"]))
            } else {
                probe_command(key, &["--version"])
            };
            lines.push(format!(
                "- {} [{}]\n  Available: {}\n  Version: {}\n  Override: {}\n  Hint: {}",
                label,
                key,
                detected.is_some(),
                detected.unwrap_or_else(|| "(not found)".to_string()),
                if override_path.is_empty() {
                    "(none)".to_string()
                } else {
                    override_path
                },
                hint
            ));
        }

        Ok(ToolResult::ok(format!(
            "Runtime checks:\n\n{}",
            lines.join("\n\n")
        )))
    }

    async fn runtime_set_path(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let runtime_key = match input["runtime_key"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
        {
            Some(v) => v.trim().to_string(),
            None => {
                return Ok(ToolResult::err(
                    "'runtime_key' is required for runtime_set_path",
                ))
            }
        };
        let exe_path = input["exe_path"].as_str().unwrap_or("").trim().to_string();

        let mut settings = self.settings.lock().await;
        if exe_path.is_empty() {
            settings.runtime_paths.remove(&runtime_key);
        } else {
            settings
                .runtime_paths
                .insert(runtime_key.clone(), exe_path.clone());
        }
        settings
            .save()
            .map_err(|e| anyhow::anyhow!("Failed to save runtime path: {}", e))?;
        drop(settings);
        self.emit_settings_changed();

        let probe = if exe_path.is_empty() {
            "(using PATH lookup)".to_string()
        } else {
            probe_command(&exe_path, &["--version"])
                .unwrap_or_else(|| "(not executable or no version output)".to_string())
        };
        Ok(ToolResult::ok(format!(
            "Runtime override {} for '{}'. Probe result: {}",
            if exe_path.is_empty() {
                "cleared"
            } else {
                "saved"
            },
            runtime_key,
            probe
        )))
    }

    async fn ssh_list(&self) -> anyhow::Result<ToolResult> {
        let settings = self.settings.lock().await;
        if settings.ssh_servers.is_empty() {
            return Ok(ToolResult::ok("No SSH servers configured."));
        }
        let lines: Vec<String> = settings
            .ssh_servers
            .iter()
            .map(|srv| {
                let auth = if !srv.password.is_empty() {
                    "password"
                } else if !srv.private_key.is_empty() {
                    "key"
                } else {
                    "no-auth"
                };
                format!(
                    "- ID: {}\n  Label: {}\n  Host: {}:{}\n  Username: {}\n  Auth: {}",
                    srv.id,
                    if srv.label.is_empty() {
                        "(none)"
                    } else {
                        &srv.label
                    },
                    srv.host,
                    srv.port,
                    srv.username,
                    auth
                )
            })
            .collect();
        Ok(ToolResult::ok(format!(
            "SSH servers ({}):\n\n{}",
            settings.ssh_servers.len(),
            lines.join("\n\n")
        )))
    }

    async fn ssh_upsert(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let ssh_id = match input["ssh_id"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(v) => v.trim().to_string(),
            None => return Ok(ToolResult::err("'ssh_id' is required for ssh_upsert")),
        };
        let ssh_host = match input["ssh_host"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(v) => v.trim().to_string(),
            None => return Ok(ToolResult::err("'ssh_host' is required for ssh_upsert")),
        };
        let ssh_username = match input["ssh_username"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
        {
            Some(v) => v.trim().to_string(),
            None => return Ok(ToolResult::err("'ssh_username' is required for ssh_upsert")),
        };
        let ssh_label = input["ssh_label"].as_str().unwrap_or("").to_string();
        let ssh_port = input["ssh_port"].as_u64().unwrap_or(22) as u16;
        let ssh_password = input["ssh_password"].as_str().unwrap_or("");
        let ssh_private_key = input["ssh_private_key"].as_str().unwrap_or("");

        let mut settings = self.settings.lock().await;
        let existing = settings
            .ssh_servers
            .iter()
            .find(|s| s.id == ssh_id)
            .cloned();
        let server = SshServerConfig {
            id: ssh_id.clone(),
            label: ssh_label,
            host: ssh_host,
            port: ssh_port,
            username: ssh_username,
            password: if ssh_password.is_empty() {
                existing
                    .as_ref()
                    .map(|s| s.password.clone())
                    .unwrap_or_default()
            } else {
                ssh_password.to_string()
            },
            private_key: if ssh_private_key.is_empty() {
                existing
                    .as_ref()
                    .map(|s| s.private_key.clone())
                    .unwrap_or_default()
            } else {
                ssh_private_key.to_string()
            },
        };

        if let Some(idx) = settings.ssh_servers.iter().position(|s| s.id == ssh_id) {
            settings.ssh_servers[idx] = server;
        } else {
            settings.ssh_servers.push(server);
        }
        settings
            .save()
            .map_err(|e| anyhow::anyhow!("Failed to save SSH server: {}", e))?;
        drop(settings);
        self.emit_settings_changed();
        Ok(ToolResult::ok(format!("SSH server '{}' saved.", ssh_id)))
    }

    async fn ssh_delete(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let ssh_id = match input["ssh_id"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(v) => v.trim().to_string(),
            None => return Ok(ToolResult::err("'ssh_id' is required for ssh_delete")),
        };
        let mut settings = self.settings.lock().await;
        let before = settings.ssh_servers.len();
        settings.ssh_servers.retain(|s| s.id != ssh_id);
        if settings.ssh_servers.len() == before {
            return Ok(ToolResult::err(format!(
                "SSH server '{}' not found",
                ssh_id
            )));
        }
        settings
            .save()
            .map_err(|e| anyhow::anyhow!("Failed to save SSH settings: {}", e))?;
        drop(settings);
        self.emit_settings_changed();
        Ok(ToolResult::ok(format!("SSH server '{}' deleted.", ssh_id)))
    }

    // ── UI / Window ───────────────────────────────────────────────────────────

    async fn ui_set_theme(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let theme = match input["theme"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(v) => v.trim().to_string(),
            None => return Ok(ToolResult::err("'theme' is required for ui_set_theme")),
        };
        let app = match &self.app_handle {
            Some(app) => app.clone(),
            None => {
                return Ok(ToolResult::err(
                    "Window control is unavailable in this context",
                ))
            }
        };
        crate::commands::platform::window::apply_app_theme(&app, &theme)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(ToolResult::ok(format!(
            "App theme switched to '{}'.",
            theme
        )))
    }

    async fn ui_set_theme_border(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let theme = match input["theme"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(v) => v.trim().to_string(),
            None => {
                return Ok(ToolResult::err(
                    "'theme' is required for ui_set_theme_border",
                ))
            }
        };
        let app = match &self.app_handle {
            Some(app) => app.clone(),
            None => {
                return Ok(ToolResult::err(
                    "Window control is unavailable in this context",
                ))
            }
        };
        crate::commands::platform::window::set_window_theme_border(app, theme.clone())
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(ToolResult::ok(format!(
            "Main window border theme set to '{}'.",
            theme
        )))
    }

    async fn ui_enter_minimal_mode(&self) -> anyhow::Result<ToolResult> {
        let app = match &self.app_handle {
            Some(app) => app.clone(),
            None => {
                return Ok(ToolResult::err(
                    "Window control is unavailable in this context",
                ))
            }
        };
        let main = app
            .get_webview_window("main")
            .ok_or_else(|| anyhow::anyhow!("Main window not found"))?;
        let overlay = app
            .get_webview_window("overlay")
            .ok_or_else(|| anyhow::anyhow!("Overlay window not found"))?;

        let (ox, oy) = {
            let settings = self.settings.lock().await;
            if let (Some(x), Some(y)) = (settings.overlay_x, settings.overlay_y) {
                (x, y)
            } else if let Ok(pos) = main.outer_position() {
                if let Ok(size) = main.outer_size() {
                    let cx = pos.x + (size.width as i32) / 2 - 140;
                    let cy = pos.y + (size.height as i32) - 80;
                    (cx.max(0), cy.max(0))
                } else {
                    (100, 100)
                }
            } else {
                (100, 100)
            }
        };

        overlay
            .set_position(tauri::PhysicalPosition::new(ox, oy))
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        main.hide().map_err(|e| anyhow::anyhow!(e.to_string()))?;
        overlay.show().map_err(|e| anyhow::anyhow!(e.to_string()))?;
        overlay
            .set_always_on_top(true)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        Ok(ToolResult::ok(format!(
            "Entered minimal mode at ({}, {}).",
            ox, oy
        )))
    }

    async fn ui_exit_minimal_mode(&self) -> anyhow::Result<ToolResult> {
        let app = match &self.app_handle {
            Some(app) => app.clone(),
            None => {
                return Ok(ToolResult::err(
                    "Window control is unavailable in this context",
                ))
            }
        };
        crate::commands::platform::window::exit_minimal_mode(app)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(ToolResult::ok("Exited minimal mode."))
    }

    async fn window_move(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let target = match input["window_target"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
        {
            Some(v @ ("main" | "overlay")) => v,
            Some(_) => {
                return Ok(ToolResult::err(
                    "'window_target' must be 'main' or 'overlay'",
                ))
            }
            None => {
                return Ok(ToolResult::err(
                    "'window_target' is required for window_move",
                ))
            }
        };
        let app = match &self.app_handle {
            Some(app) => app.clone(),
            None => {
                return Ok(ToolResult::err(
                    "Window control is unavailable in this context",
                ))
            }
        };
        let window = app
            .get_webview_window(target)
            .ok_or_else(|| anyhow::anyhow!("Window '{}' not found", target))?;

        let (x, y) = match input["position_preset"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some("bottom_right") => bottom_right_position(&window)?,
            Some(other) => {
                return Ok(ToolResult::err(format!(
                    "Unsupported position_preset '{}'",
                    other
                )))
            }
            None => {
                let x = input["x"].as_i64().ok_or_else(|| {
                    anyhow::anyhow!("'x' is required when position_preset is not used")
                })? as i32;
                let y = input["y"].as_i64().ok_or_else(|| {
                    anyhow::anyhow!("'y' is required when position_preset is not used")
                })? as i32;
                (x, y)
            }
        };

        window
            .set_position(tauri::PhysicalPosition::new(x, y))
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        if target == "overlay" {
            let mut settings = self.settings.lock().await;
            settings.overlay_x = Some(x);
            settings.overlay_y = Some(y);
            settings
                .save()
                .map_err(|e| anyhow::anyhow!("Failed to persist overlay position: {}", e))?;
        }
        Ok(ToolResult::ok(format!(
            "Moved '{}' window to ({}, {}).",
            target, x, y
        )))
    }

    // ── User Notifications ────────────────────────────────────────────────────

    async fn notify_user(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let message = match input["message"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(m) => m.to_string(),
            None => return Ok(ToolResult::err("'message' is required for notify_user")),
        };
        let title = input["title"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Pisci")
            .to_string();
        let level = match input["level"].as_str().map(str::trim).unwrap_or("info") {
            "" | "info" => "info",
            "warning" | "warn" => "warning",
            "error" => "error",
            "critical" => "critical",
            other => {
                return Ok(ToolResult::err(format!(
                    "Invalid level '{}'. Use info|warning|error|critical",
                    other
                )))
            }
        }
        .to_string();
        let pool_id = input["pool_id"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let default_duration_ms: i64 = match level.as_str() {
            "critical" => 0,
            "error" => 12_000,
            "warning" => 8_000,
            _ => 5_000,
        };
        let duration_ms = input["duration_ms"].as_i64().unwrap_or(default_duration_ms);

        let app = match &self.app_handle {
            Some(app) => app.clone(),
            None => {
                return Ok(ToolResult::err(
                    "UI notifications are unavailable in this context",
                ))
            }
        };

        let id = format!(
            "toast_{}_{}",
            chrono::Utc::now().timestamp_millis(),
            uuid::Uuid::new_v4().simple()
        );
        let payload = json!({
            "id": id,
            "title": title,
            "message": message,
            "level": level,
            "pool_id": pool_id,
            "duration_ms": duration_ms,
            "source": "pisci",
            "ts": chrono::Utc::now().timestamp_millis(),
        });

        app.emit("pisci_toast", payload)
            .map_err(|e| anyhow::anyhow!("Failed to emit pisci_toast: {}", e))?;

        info!(
            "notify_user: level={} title={:?} message_len={}",
            level,
            title,
            message.chars().count()
        );

        Ok(ToolResult::ok(format!(
            "Toast delivered to main UI (level='{}', duration_ms={}).",
            level, duration_ms
        )))
    }

    // ── Built-in Tools ────────────────────────────────────────────────────────

    async fn builtin_tool_list(&self) -> anyhow::Result<ToolResult> {
        let settings = self.settings.lock().await;
        let mut lines = Vec::new();
        for tool in builtin_tool_catalog() {
            let enabled = settings
                .builtin_tool_enabled
                .get(&tool.name)
                .copied()
                .unwrap_or(true);
            lines.push(format!(
                "Name: {}\n  Enabled: {}\n  Windows-only: {}\n  Description: {}",
                tool.name, enabled, tool.windows_only, tool.description
            ));
        }
        Ok(ToolResult::ok(format!(
            "{} built-in tool(s):\n\n{}",
            lines.len(),
            lines.join("\n\n")
        )))
    }

    async fn builtin_tool_toggle(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let tool_name = match input["tool_name"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(v) => v.trim().to_string(),
            None => {
                return Ok(ToolResult::err(
                    "'tool_name' is required for builtin_tool_toggle",
                ))
            }
        };
        let enabled = match input["enabled"].as_bool() {
            Some(v) => v,
            None => {
                return Ok(ToolResult::err(
                    "'enabled' (boolean) is required for builtin_tool_toggle",
                ))
            }
        };
        if !builtin_tool_catalog()
            .iter()
            .any(|tool| tool.name == tool_name.as_str())
        {
            return Ok(ToolResult::err(format!(
                "Built-in tool '{}' not found",
                tool_name
            )));
        }

        let mut settings = self.settings.lock().await;
        settings
            .builtin_tool_enabled
            .insert(tool_name.clone(), enabled);
        settings
            .save()
            .map_err(|e| anyhow::anyhow!("Failed to save built-in tool settings: {}", e))?;
        drop(settings);
        self.emit_settings_changed();
        Ok(ToolResult::ok(format!(
            "Built-in tool '{}' is now {}.",
            tool_name,
            if enabled { "enabled" } else { "disabled" }
        )))
    }

    // ── User Tools ────────────────────────────────────────────────────────────

    async fn user_tool_list(&self) -> anyhow::Result<ToolResult> {
        let tools_dir = self.app_data_dir.join("user-tools");
        let tools = crate::tools::user_tool::load_user_tools(&tools_dir);
        let settings = self.settings.lock().await;

        if tools.is_empty() {
            return Ok(ToolResult::ok("No user tools installed."));
        }

        let mut lines = Vec::new();
        for tool in tools {
            let has_config = settings.user_tool_configs.contains_key(&tool.manifest.name);
            let config_fields = if tool.manifest.config_schema.is_empty() {
                "none".to_string()
            } else {
                tool.manifest
                    .config_schema
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            lines.push(format!(
                "Name: {}\n  Version: {}\n  Runtime: {}\n  Readonly: {}\n  Has config: {}\n  Config fields: {}\n  Description: {}",
                tool.manifest.name,
                tool.manifest.version,
                tool.manifest.runtime,
                tool.manifest.readonly,
                has_config,
                config_fields,
                tool.manifest.description
            ));
        }
        Ok(ToolResult::ok(format!(
            "{} user tool(s):\n\n{}",
            lines.len(),
            lines.join("\n\n")
        )))
    }

    async fn user_tool_config_get(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let tool_name = match input["tool_name"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(v) => v.trim().to_string(),
            None => {
                return Ok(ToolResult::err(
                    "'tool_name' is required for user_tool_config_get",
                ))
            }
        };
        let manifest = load_user_tool_manifest(&self.app_data_dir, &tool_name)?;
        let settings = self.settings.lock().await;
        let raw = settings
            .user_tool_configs
            .get(&tool_name)
            .cloned()
            .unwrap_or_else(|| json!({}));

        let mut result = serde_json::Map::new();
        if let Value::Object(map) = raw {
            for (key, value) in map {
                let is_password = manifest
                    .config_schema
                    .get(&key)
                    .map(|s| s.field_type == "password")
                    .unwrap_or(false);
                if is_password {
                    let masked = if value.as_str().map(|s| !s.is_empty()).unwrap_or(false) {
                        "••••••••".to_string()
                    } else {
                        String::new()
                    };
                    result.insert(key, Value::String(masked));
                } else {
                    result.insert(key, value);
                }
            }
        }
        for (key, schema) in &manifest.config_schema {
            if !result.contains_key(key) {
                if let Some(default) = &schema.default {
                    result.insert(key.clone(), default.clone());
                }
            }
        }

        Ok(ToolResult::ok(serde_json::to_string_pretty(
            &Value::Object(result),
        )?))
    }

    async fn user_tool_config_set(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let tool_name = match input["tool_name"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(v) => v.trim().to_string(),
            None => {
                return Ok(ToolResult::err(
                    "'tool_name' is required for user_tool_config_set",
                ))
            }
        };
        let new_config = match input.get("config") {
            Some(Value::Object(map)) => map.clone(),
            _ => {
                return Ok(ToolResult::err(
                    "'config' object is required for user_tool_config_set",
                ))
            }
        };
        let manifest = load_user_tool_manifest(&self.app_data_dir, &tool_name)?;

        let mut settings = self.settings.lock().await;
        let existing = settings
            .user_tool_configs
            .get(&tool_name)
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let mut merged = existing;
        for (key, value) in new_config {
            let preserve_existing_password = manifest
                .config_schema
                .get(&key)
                .map(|s| s.field_type == "password")
                .unwrap_or(false)
                && value.as_str() == Some("••••••••");
            if preserve_existing_password {
                continue;
            }
            merged.insert(key, value);
        }

        settings
            .user_tool_configs
            .insert(tool_name.clone(), Value::Object(merged));
        settings
            .save()
            .map_err(|e| anyhow::anyhow!("Failed to save user tool config: {}", e))?;
        drop(settings);
        self.emit_settings_changed();
        Ok(ToolResult::ok(format!(
            "Config for user tool '{}' saved.",
            tool_name
        )))
    }

    // ── Skills ────────────────────────────────────────────────────────────────

    async fn skill_list(&self) -> anyhow::Result<ToolResult> {
        let db_skills = {
            let db = self.db.lock().await;
            db.list_skills().unwrap_or_default()
        };

        let skills_dir = self.app_data_dir.join("skills");
        let mut loader = SkillLoader::new(&skills_dir);
        if let Err(e) = loader.load_all() {
            warn!("skill_list: failed to load skills from disk: {}", e);
        }
        let fs_skills = loader.list_skills();

        if db_skills.is_empty() {
            return Ok(ToolResult::ok("No skills installed."));
        }

        let mut lines: Vec<String> = Vec::new();
        for db_skill in &db_skills {
            let fs_entry = fs_skills
                .iter()
                .find(|s| s.name == db_skill.name || s.name == db_skill.id);
            lines.push(format!(
                "ID: {}\n  Name: {}\n  Description: {}\n  Enabled: {}\n  Source: {}\n  Version: {}\n  Tools: {}\n  Dependencies: {}{}",
                db_skill.id,
                db_skill.name,
                db_skill.description,
                db_skill.enabled,
                fs_entry.map(|s| s.source.as_str()).unwrap_or("db"),
                fs_entry.map(|s| s.version.as_str()).filter(|s| !s.is_empty()).unwrap_or("(unknown)"),
                fs_entry.map(|s| if s.tools.is_empty() { "(none)".to_string() } else { s.tools.join(", ") }).unwrap_or_else(|| "(unknown)".to_string()),
                fs_entry.map(|s| if s.dependencies.is_empty() { "(none)".to_string() } else { s.dependencies.join(", ") }).unwrap_or_else(|| "(unknown)".to_string()),
                if fs_entry.is_none() { "\n  (definition file not found on disk)" } else { "" }
            ));
        }

        Ok(ToolResult::ok(format!(
            "{} skill(s):\n\n{}",
            lines.len(),
            lines.join("\n\n")
        )))
    }

    async fn skill_search(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let query = input["query"].as_str().unwrap_or("").trim().to_string();
        let limit: u32 = input["limit"].as_u64().unwrap_or(10).min(20) as u32;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("Pisci-Desktop/1.0")
            .build()
            .map_err(|e| anyhow::anyhow!(e))?;

        let (url, use_search) = if query.is_empty() {
            (
                format!("{}/api/v1/skills?sort=stars&limit={}", CLAWHUB_API, limit),
                false,
            )
        } else {
            (
                format!(
                    "{}/api/v1/search?q={}&limit={}",
                    CLAWHUB_API,
                    urlencoding::encode(&query),
                    limit
                ),
                true,
            )
        };

        info!("skill_search: {}", url);

        let resp = clawhub_get_with_retry(&client, &url, 3)
            .await
            .map_err(|e| anyhow::anyhow!("Cannot reach ClawHub: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let hint = if status.as_u16() == 429 {
                " (rate limited, please retry later)"
            } else {
                ""
            };
            let body = resp.text().await.unwrap_or_default();
            return Ok(ToolResult::err(format!(
                "ClawHub HTTP {}{}: {}",
                status,
                hint,
                if body.chars().count() > 200 {
                    body.chars().take(200).collect::<String>()
                } else {
                    body
                }
            )));
        }

        let body: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Invalid ClawHub response: {}", e))?;

        let items: Vec<String> = if use_search {
            body["results"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|r| {
                    let slug = r["slug"].as_str()?;
                    let name = r["displayName"].as_str().unwrap_or(slug);
                    let desc = r["summary"].as_str().unwrap_or("");
                    let ver = r["version"].as_str().unwrap_or("");
                    Some(format!(
                        "Slug: {}\n  Name: {}\n  Version: {}\n  Description: {}",
                        slug,
                        name,
                        if ver.is_empty() { "(unspecified)" } else { ver },
                        desc
                    ))
                })
                .collect()
        } else {
            body["items"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|r| {
                    let slug = r["slug"].as_str()?;
                    let name = r["displayName"].as_str().unwrap_or(slug);
                    let desc = r["summary"].as_str().unwrap_or("");
                    let ver = r["latestVersion"]["version"].as_str().unwrap_or("latest");
                    let stars = r["stats"]["stars"].as_u64().unwrap_or(0);
                    Some(format!(
                        "Slug: {}\n  Name: {}\n  Version: {}\n  Stars: {}\n  Description: {}",
                        slug, name, ver, stars, desc
                    ))
                })
                .collect()
        };

        if items.is_empty() {
            return Ok(ToolResult::ok(format!(
                "No skills found for query '{}'.",
                query
            )));
        }

        Ok(ToolResult::ok(format!(
            "Found {} skill(s) on ClawHub{}:\n\nTo install, use: action=skill_install, source=<slug>\n\n{}",
            items.len(),
            if query.is_empty() { " (top by stars)".to_string() } else { format!(" matching '{}'", query) },
            items.join("\n\n")
        )))
    }

    async fn skill_install(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let source = match input["source"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(s) => s.to_string(),
            None => return Ok(ToolResult::err(
                "'source' is required: provide a ClawHub slug (e.g. 'pptx-maker') or a direct URL",
            )),
        };

        // Determine if source is a URL or a slug
        let content = if source.starts_with("http://") || source.starts_with("https://") {
            // Direct URL — download SKILL.md
            let blocked = [
                "localhost",
                "127.0.0.1",
                "0.0.0.0",
                "192.168.",
                "10.",
                "172.",
            ];
            for pat in blocked {
                if source.contains(pat) {
                    return Ok(ToolResult::err(format!(
                        "Blocked URL: '{}' points to a private address",
                        source
                    )));
                }
            }
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("Pisci-Desktop/1.0")
                .build()
                .map_err(|e| anyhow::anyhow!(e))?;
            let resp = client
                .get(&source)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Download failed: {}", e))?;
            if !resp.status().is_success() {
                return Ok(ToolResult::err(format!(
                    "HTTP {} fetching URL",
                    resp.status()
                )));
            }
            resp.text().await.map_err(|e| anyhow::anyhow!(e))?
        } else {
            // Treat as ClawHub slug
            if !source
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
            {
                return Ok(ToolResult::err(format!(
                    "Invalid slug '{}': use alphanumeric, hyphens, underscores only",
                    source
                )));
            }
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("Pisci-Desktop/1.0")
                .build()
                .map_err(|e| anyhow::anyhow!(e))?;
            let file_url = format!(
                "{}/api/v1/skills/{}/file?path=SKILL.md",
                CLAWHUB_API, source
            );
            info!("skill_install: fetching {} from {}", source, file_url);
            let resp = clawhub_get_with_retry(&client, &file_url, 3)
                .await
                .map_err(|e| anyhow::anyhow!("ClawHub request failed: {}", e))?;
            if resp.status().is_success() {
                resp.text().await.map_err(|e| anyhow::anyhow!(e))?
            } else {
                let file_status = resp.status();
                // Fallback: zip download
                let zip_url = format!("{}/api/v1/download?slug={}", CLAWHUB_API, source);
                info!(
                    "skill_install: file endpoint failed ({}), trying zip: {}",
                    file_status, zip_url
                );
                let zip_resp = clawhub_get_with_retry(&client, &zip_url, 3)
                    .await
                    .map_err(|e| anyhow::anyhow!("Zip download failed: {}", e))?;
                if !zip_resp.status().is_success() {
                    let hint = if zip_resp.status().as_u16() == 429 {
                        "请求过于频繁，请稍后再试".to_string()
                    } else {
                        format!("HTTP {}", zip_resp.status())
                    };
                    return Ok(ToolResult::err(format!(
                        "Skill '{}' install failed ({}). Check the slug with skill_search first.",
                        source, hint
                    )));
                }
                let zip_bytes = zip_resp.bytes().await.map_err(|e| anyhow::anyhow!(e))?;
                extract_skill_md_from_zip(&zip_bytes)
                    .map_err(|e| anyhow::anyhow!("Failed to extract SKILL.md from zip: {}", e))?
            }
        };
        self.install_skill_from_content(&content).await
    }

    async fn skill_toggle(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let skill_id = match input["skill_id"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(i) => i,
            None => {
                return Ok(ToolResult::err(
                    "'skill_id' is required for skill_toggle (get IDs from skill_list)",
                ))
            }
        };
        let enabled = match input["enabled"].as_bool() {
            Some(e) => e,
            None => {
                return Ok(ToolResult::err(
                    "'enabled' (boolean) is required for skill_toggle",
                ))
            }
        };

        let db = self.db.lock().await;
        match db.set_skill_enabled(skill_id, enabled) {
            Ok(_) => Ok(ToolResult::ok(format!(
                "Skill '{}' {}.",
                skill_id,
                if enabled { "enabled" } else { "disabled" }
            ))),
            Err(e) => Ok(ToolResult::err(format!("Failed to toggle skill: {}", e))),
        }
    }

    async fn skill_uninstall(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let skill_name = match input["skill_name"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
        {
            Some(n) => n,
            None => {
                return Ok(ToolResult::err(
                    "'skill_name' is required for skill_uninstall",
                ))
            }
        };

        let skills_dir = self.app_data_dir.join("skills");
        let safe_name: String = skill_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .to_lowercase();
        // Remove from DB first — abort before touching filesystem if this fails
        {
            let db = self.db.lock().await;
            db.delete_skill(&safe_name)
                .map_err(|e| anyhow::anyhow!("Failed to remove skill from database: {}", e))?;
        }

        // Remove matching skill directories from disk as well.
        if skills_dir.exists() {
            let canonical_skills = skills_dir
                .canonicalize()
                .map_err(|e| anyhow::anyhow!("Path error: {}", e))?;
            let mut loader = SkillLoader::new(&skills_dir);
            let _ = loader.load_all();
            let mut candidate_dirs: std::collections::BTreeSet<std::path::PathBuf> =
                std::collections::BTreeSet::new();
            candidate_dirs.insert(skills_dir.join(&safe_name));
            for skill in loader.list_skills() {
                let parsed_safe_name = skill
                    .name
                    .chars()
                    .map(|c| {
                        if c.is_alphanumeric() || c == '-' || c == '_' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect::<String>()
                    .to_lowercase();
                if skill.name.eq_ignore_ascii_case(skill_name) || parsed_safe_name == safe_name {
                    if let Some(dir) = skill.source_path.parent() {
                        candidate_dirs.insert(dir.to_path_buf());
                    }
                }
            }
            for skill_dir in candidate_dirs {
                if !skill_dir.exists() {
                    continue;
                }
                let canonical_dir = skill_dir
                    .canonicalize()
                    .map_err(|e| anyhow::anyhow!("Path error: {}", e))?;
                if !canonical_dir.starts_with(&canonical_skills) {
                    return Ok(ToolResult::err("Path traversal attempt blocked"));
                }
                tokio::fs::remove_dir_all(&skill_dir).await.map_err(|e| {
                    anyhow::anyhow!(
                        "Skill removed from database but failed to delete files: {}",
                        e
                    )
                })?;
            }
        }

        info!("Uninstalled skill '{}'", skill_name);
        Ok(ToolResult::ok(format!(
            "Skill '{}' uninstalled.",
            skill_name
        )))
    }

    async fn install_skill_from_content(&self, content: &str) -> anyhow::Result<ToolResult> {
        let skills_dir = self.app_data_dir.join("skills");
        let loader = SkillLoader::new(&skills_dir);
        let skill = loader
            .parse_skill_from_content(content)
            .map_err(|e| anyhow::anyhow!("Failed to parse SKILL.md: {}", e))?;

        if skill.name.is_empty() || skill.name == "unnamed" {
            return Ok(ToolResult::err(
                "SKILL.md must declare a 'name' field in frontmatter",
            ));
        }

        let compat = crate::skills::loader::check_skill_compatibility(&skill).await;
        if !compat.compatible {
            return Ok(ToolResult::err(format!(
                "Skill '{}' is incompatible with this system:\n{}",
                skill.name,
                compat.issues.join("\n")
            )));
        }
        for w in &compat.warnings {
            warn!("Skill '{}' warning: {}", skill.name, w);
        }

        let safe_name: String = skill
            .name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .to_lowercase();
        let skill_dir = skills_dir.join(&safe_name);
        tokio::fs::create_dir_all(&skill_dir)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create skill dir: {}", e))?;
        {
            let db = self.db.lock().await;
            db.upsert_skill(&safe_name, &skill.name, &skill.description, "📦")
                .map_err(|e| anyhow::anyhow!("Failed to register skill in database: {}", e))?;
        }
        if let Err(e) = tokio::fs::write(skill_dir.join("SKILL.md"), content).await {
            let db = self.db.lock().await;
            let _ = db.delete_skill(&safe_name);
            let _ = tokio::fs::remove_dir_all(&skill_dir).await;
            return Err(anyhow::anyhow!("Failed to write SKILL.md: {}", e));
        }

        info!("Installed skill '{}' to {:?}", skill.name, skill_dir);

        let warn_msg = if compat.warnings.is_empty() {
            String::new()
        } else {
            format!(
                "\nWarnings:\n{}",
                compat
                    .warnings
                    .iter()
                    .map(|w| format!("  - {}", w))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };

        Ok(ToolResult::ok(format!(
            "Skill '{}' installed successfully.\n\
             Version: {}\n\
             Description: {}\n\
             Tools used: {}\n\
             Dependencies: {}{}\n\
             \nTo enable it, use: action=skill_toggle, skill_id=<id from skill_list>, enabled=true",
            skill.name,
            skill.version,
            skill.description,
            if skill.tools.is_empty() {
                "(none)".to_string()
            } else {
                skill.tools.join(", ")
            },
            if skill.dependencies.is_empty() {
                "(none)".to_string()
            } else {
                skill.dependencies.join(", ")
            },
            warn_msg,
        )))
    }
}

fn builtin_tool_catalog() -> Vec<BuiltinToolInfo> {
    vec![
        BuiltinToolInfo {
            name: "file_read".into(),
            description: "Read local files.".into(),
            icon: "📄".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "file_write".into(),
            description: "Write or append local files.".into(),
            icon: "✏️".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "file_edit".into(),
            description: "Edit existing local files.".into(),
            icon: "📝".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "file_diff".into(),
            description: "Compare file contents.".into(),
            icon: "📚".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "code_run".into(),
            description: "Run code snippets.".into(),
            icon: "▶️".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "file_search".into(),
            description: "Search text in files.".into(),
            icon: "🔎".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "file_list".into(),
            description: "List files and folders.".into(),
            icon: "🗂️".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "process_control".into(),
            description: "Inspect and manage processes.".into(),
            icon: "⚙️".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "shell".into(),
            description: "Execute shell commands.".into(),
            icon: "⌨️".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "powershell_query".into(),
            description: "Run PowerShell commands.".into(),
            icon: "🪟".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "web_search".into(),
            description: "Search the web.".into(),
            icon: "🔍".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "office".into(),
            description: "Operate Office documents.".into(),
            icon: "📊".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "browser".into(),
            description: "Control the browser.".into(),
            icon: "🌐".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "email".into(),
            description: "Send and read email.".into(),
            icon: "📧".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "memory_store".into(),
            description: "Persist long-term memory.".into(),
            icon: "🧠".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "plan_todo".into(),
            description: "Maintain a visible task plan for complex work.".into(),
            icon: "📋".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "vision_context".into(),
            description: "Manage reusable vision artifacts for the next multimodal step.".into(),
            icon: "🖼️".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "call_fish".into(),
            description: "Delegate work to Fish sub-agents.".into(),
            icon: "🐠".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "call_koi".into(),
            description: "Delegate work to persistent Koi agents.".into(),
            icon: "🐟".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "pool_org".into(),
            description: "Create and manage project pools and organization specs.".into(),
            icon: "🏊".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "app_control".into(),
            description: "Manage Pisci app settings and system state.".into(),
            icon: "🎛️".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "skill_search".into(),
            description: "Search installed skills and instructions.".into(),
            icon: "📦".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "ssh".into(),
            description: "Run commands on SSH servers.".into(),
            icon: "🔐".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "pdf".into(),
            description: "Read or write PDF files.".into(),
            icon: "📕".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "wmi".into(),
            description: "Query Windows system information via WMI.".into(),
            icon: "💻".into(),
            windows_only: true,
        },
        BuiltinToolInfo {
            name: "uia".into(),
            description: "Control Windows UI Automation elements.".into(),
            icon: "🖱️".into(),
            windows_only: true,
        },
        BuiltinToolInfo {
            name: "screen_capture".into(),
            description: "Capture the screen.".into(),
            icon: "📸".into(),
            windows_only: true,
        },
        BuiltinToolInfo {
            name: "com".into(),
            description: "Use COM/OLE automation.".into(),
            icon: "🔌".into(),
            windows_only: true,
        },
        BuiltinToolInfo {
            name: "com_invoke".into(),
            description: "Invoke COM methods.".into(),
            icon: "🧩".into(),
            windows_only: true,
        },
    ]
}

// ── Koi Agent Management ──────────────────────────────────────────────────────

impl AppControlTool {
    async fn koi_list(&self) -> anyhow::Result<ToolResult> {
        let db = self.db.lock().await;
        let kois = db.list_kois().map_err(|e| anyhow::anyhow!(e))?;
        if kois.is_empty() {
            return Ok(ToolResult::ok("No Koi agents configured yet."));
        }
        let lines: Vec<String> = kois
            .iter()
            .map(|k| {
                format!(
                    "ID: {}\n  Name: {} {}\n  Role: {}\n  Status: {}\n  Description: {}",
                    k.id,
                    k.icon,
                    k.name,
                    k.role,
                    k.status,
                    if k.description.is_empty() {
                        "(none)"
                    } else {
                        &k.description
                    }
                )
            })
            .collect();
        Ok(ToolResult::ok(format!(
            "Koi agents ({}):\n\n{}",
            kois.len(),
            lines.join("\n\n")
        )))
    }

    async fn koi_create(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let name = match input["name"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(n) => n.trim().to_string(),
            None => return Ok(ToolResult::err("'name' is required for koi_create")),
        };
        if let Err(e) = validate_koi_name(&name) {
            return Ok(ToolResult::err(e.to_string()));
        }
        let role = match input["role"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(r) => r.trim().to_string(),
            None => return Ok(ToolResult::err("'role' is required for koi_create")),
        };
        let system_prompt = match input["system_prompt"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
        {
            Some(p) => p.trim().to_string(),
            None => {
                return Ok(ToolResult::err(
                    "'system_prompt' is required for koi_create",
                ))
            }
        };
        let icon = input["icon"].as_str().unwrap_or("🐟").to_string();
        let description = input["description"].as_str().unwrap_or("").to_string();

        // Pick a default color if not provided
        let color = input["color"]
            .as_str()
            .unwrap_or_else(|| {
                // Cycle through a palette based on name hash
                let palette = [
                    "#22c55e", "#3b82f6", "#f59e0b", "#ec4899", "#8b5cf6", "#06b6d4", "#ef4444",
                    "#84cc16",
                ];
                let idx =
                    name.bytes().fold(0usize, |a, b| a.wrapping_add(b as usize)) % palette.len();
                palette[idx]
            })
            .to_string();

        let db = self.db.lock().await;
        let existing = db.list_kois().map_err(|e| anyhow::anyhow!(e))?;
        const MAX_KOIS: usize = 10;
        if existing.len() >= MAX_KOIS {
            return Ok(ToolResult::err(format!(
                "Koi limit reached ({}/{}). Delete an existing Koi before creating a new one.",
                existing.len(),
                MAX_KOIS
            )));
        }

        let koi = db
            .create_koi(
                &name,
                &role,
                &icon,
                &color,
                &system_prompt,
                &description,
                None,
                0,
                0,
            )
            .map_err(|e| anyhow::anyhow!(e))?;

        // Notify frontend
        if let Some(app) = &self.app_handle {
            let _ = app.emit(
                "koi_created",
                serde_json::json!({ "id": koi.id, "name": koi.name }),
            );
        }

        Ok(ToolResult::ok(format!(
            "Koi '{}' {} created successfully.\nID: {}\nRole: {}\nYou can now assign tasks to this Koi using call_koi or pool_org.",
            koi.name, koi.icon, koi.id, koi.role
        )))
    }

    async fn koi_update(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let koi_id = match input["koi_id"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(id) => id.to_string(),
            None => return Ok(ToolResult::err("'koi_id' is required for koi_update")),
        };

        let name = input["name"].as_str().map(|s| s.trim().to_string());
        if let Some(ref name) = name {
            if let Err(e) = validate_koi_name(name) {
                return Ok(ToolResult::err(e.to_string()));
            }
        }
        let role = input["role"].as_str().map(|s| s.trim().to_string());
        let icon = input["icon"].as_str().map(|s| s.to_string());
        let color = input["color"].as_str().map(|s| s.to_string());
        let system_prompt = input["system_prompt"].as_str().map(|s| s.to_string());
        let description = input["description"].as_str().map(|s| s.to_string());

        // Ensure at least one field is being updated
        if name.is_none()
            && role.is_none()
            && icon.is_none()
            && color.is_none()
            && system_prompt.is_none()
            && description.is_none()
        {
            return Ok(ToolResult::err(
                "koi_update requires at least one field to update: name, role, icon, color, system_prompt, description",
            ));
        }

        let db = self.db.lock().await;
        // Verify the Koi exists
        let kois = db.list_kois().map_err(|e| anyhow::anyhow!(e))?;
        let koi = match kois.iter().find(|k| k.id == koi_id) {
            Some(k) => k.clone(),
            None => return Ok(ToolResult::err(format!("Koi '{}' not found.", koi_id))),
        };

        db.update_koi(
            &koi_id,
            name.as_deref(),
            role.as_deref(),
            icon.as_deref(),
            color.as_deref(),
            system_prompt.as_deref(),
            description.as_deref(),
            None, // don't touch llm_provider_id
            None, // don't touch max_iterations
            None, // don't touch task_timeout_secs
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        drop(db);

        // Notify frontend so the Koi panel refreshes
        if let Some(app) = &self.app_handle {
            let _ = app.emit("koi_updated", serde_json::json!({ "id": koi_id }));
        }

        let new_name = name.as_deref().unwrap_or(&koi.name);
        let new_icon = icon.as_deref().unwrap_or(&koi.icon);
        Ok(ToolResult::ok(format!(
            "Koi '{}' {} updated successfully.",
            new_name, new_icon
        )))
    }

    async fn koi_delete(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let koi_id = match input["koi_id"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(id) => id.to_string(),
            None => return Ok(ToolResult::err("'koi_id' is required for koi_delete")),
        };

        let db = self.db.lock().await;
        // Verify the Koi exists first
        let kois = db.list_kois().map_err(|e| anyhow::anyhow!(e))?;
        let koi = match kois.iter().find(|k| k.id == koi_id) {
            Some(k) => k.clone(),
            None => return Ok(ToolResult::err(format!("Koi '{}' not found.", koi_id))),
        };

        db.delete_koi(&koi_id).map_err(|e| anyhow::anyhow!(e))?;

        if let Some(app) = &self.app_handle {
            let _ = app.emit("koi_deleted", serde_json::json!({ "id": koi_id }));
        }

        Ok(ToolResult::ok(format!(
            "Koi '{}' {} has been deleted.",
            koi.name, koi.icon
        )))
    }
}

fn safe_user_tool_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .to_lowercase()
}

fn load_user_tool_manifest(
    app_data_dir: &std::path::Path,
    tool_name: &str,
) -> anyhow::Result<UserToolManifest> {
    let tool_dir = app_data_dir
        .join("user-tools")
        .join(safe_user_tool_name(tool_name));
    UserToolManifest::load(&tool_dir)
        .map_err(|e| anyhow::anyhow!("User tool '{}' not found: {}", tool_name, e))
}

fn bottom_right_position(window: &tauri::WebviewWindow) -> anyhow::Result<(i32, i32)> {
    let size = window
        .outer_size()
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETWORKAREA};

        let mut rect = RECT::default();
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some((&mut rect as *mut RECT).cast()),
                Default::default(),
            )
        }
        .is_ok();
        if ok {
            let x = (rect.right - size.width as i32 - 16).max(rect.left);
            let y = (rect.bottom - size.height as i32 - 16).max(rect.top);
            return Ok((x, y));
        }
    }
    Ok((
        ((1920 - size.width as i32) - 16).max(0),
        ((1080 - size.height as i32) - 16).max(0),
    ))
}

fn extract_skill_md_from_zip(zip_bytes: &[u8]) -> anyhow::Result<String> {
    use std::io::Read;
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_lowercase();
        if name == "skill.md" || name.ends_with("/skill.md") {
            let mut content = String::new();
            file.read_to_string(&mut content)?;
            return Ok(content);
        }
    }
    anyhow::bail!("SKILL.md not found in zip archive")
}

fn probe_command(cmd: &str, args: &[&str]) -> Option<String> {
    let mut command = std::process::Command::new(cmd);
    command.args(args);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = command.output().ok()?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let text = if stdout.is_empty() {
            String::from_utf8_lossy(&output.stderr).trim().to_string()
        } else {
            stdout
        };
        Some(text)
    } else {
        None
    }
}
