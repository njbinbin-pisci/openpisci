use crate::agent::tool::{Tool, ToolContext, ToolResult};
use crate::skills::loader::SkillLoader;
use crate::store::{Database, Settings};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

const CLAWHUB_API: &str = "https://clawhub.ai";

/// GET with automatic retry on 429 / 5xx (exponential back-off, max 3 retries).
async fn clawhub_get_with_retry(
    client: &reqwest::Client,
    url: &str,
    max_retries: u32,
) -> anyhow::Result<reqwest::Response> {
    let base_delay_ms: u64 = 1000;
    let mut attempt = 0u32;
    loop {
        let resp = client.get(url).send().await
            .map_err(|e| anyhow::anyhow!("Network error: {}", e))?;
        let status = resp.status();
        if status.is_success() || (status.is_client_error() && status.as_u16() != 429) {
            return Ok(resp);
        }
        if attempt >= max_retries {
            return Ok(resp);
        }
        let retry_after_ms = resp.headers()
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
        warn!("ClawHub {} for '{}', retrying in {}ms ({}/{})", status, url, backoff_ms, attempt + 1, max_retries);
        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
        attempt += 1;
    }
}

pub struct AppControlTool {
    pub db: Arc<Mutex<Database>>,
    pub settings: Arc<Mutex<Settings>>,
    pub app_data_dir: PathBuf,
}

#[async_trait]
impl Tool for AppControlTool {
    fn name(&self) -> &str { "app_control" }

    fn description(&self) -> &str {
        "Control OpenPisci application: manage scheduled tasks, update settings, and manage skills.\
         \n\nACTIONS — Scheduled Tasks:\
         \n- 'task_list': List all scheduled tasks (name, cron, status, last run).\
         \n- 'task_create': Create a task. Required: name, cron_expression (5-field), task_prompt. Optional: description.\
         \n- 'task_update': Update a task by id. Optional: name, cron_expression, task_prompt, status (active/paused).\
         \n- 'task_delete': Delete a task by id.\
         \n- 'task_run_now': Immediately trigger a task by id.\
         \
         \n\nACTIONS — Settings:\
         \n- 'settings_get': Read current settings (provider, model, max_tokens, etc.).\
         \n- 'settings_set': Update settings. Fields: provider, model, api_key, custom_base_url, max_tokens, context_window, max_iterations, policy_mode, workspace_root, confirm_shell_commands, confirm_file_writes, language, browser_headless, heartbeat_enabled, heartbeat_interval_mins, heartbeat_prompt.\
         \
         \n\nACTIONS — Skills:\
         \n- 'skill_list': List installed skills with enabled status.\
         \n- 'skill_search': Search ClawHub marketplace. Required: query (use empty string for top skills).\
         \n- 'skill_install': Install a skill from a ClawHub slug or direct URL. Required: source (slug or URL).\
         \n- 'skill_toggle': Enable or disable an installed skill. Required: skill_id, enabled (bool).\
         \n- 'skill_uninstall': Remove an installed skill by name. Required: skill_name.\
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
                        "skill_list", "skill_search", "skill_install", "skill_toggle", "skill_uninstall"
                    ]
                },
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
                "confirm_shell_commands": { "type": "boolean" },
                "confirm_file_writes": { "type": "boolean" },
                "language": { "type": "string", "enum": ["zh", "en"] },
                "browser_headless": { "type": "boolean" },
                "heartbeat_enabled": { "type": "boolean" },
                "heartbeat_interval_mins": { "type": "integer" },
                "heartbeat_prompt": { "type": "string" },
                // Skill fields
                "query": { "type": "string", "description": "Search query for skill_search" },
                "source": { "type": "string", "description": "ClawHub slug or direct URL for skill_install" },
                "skill_id": { "type": "string", "description": "Skill ID for skill_toggle (from skill_list)" },
                "skill_name": { "type": "string", "description": "Skill name for skill_uninstall" },
                "enabled": { "type": "boolean", "description": "Enable/disable for skill_toggle" }
            }
        })
    }

    fn is_read_only(&self) -> bool { false }

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
            "skill_list"      => self.skill_list().await,
            "skill_search"    => self.skill_search(&input).await,
            "skill_install"   => self.skill_install(&input).await,
            "skill_toggle"    => self.skill_toggle(&input).await,
            "skill_uninstall" => self.skill_uninstall(&input).await,
            other => Ok(ToolResult::err(format!(
                "Unknown action '{}'. Valid actions: task_list, task_create, task_update, task_delete, task_run_now, settings_get, settings_set, skill_list, skill_search, skill_install, skill_toggle, skill_uninstall",
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
                        if t.task_prompt.len() > 80 {
                            format!("{}…", &t.task_prompt[..80])
                        } else {
                            t.task_prompt.clone()
                        }
                    )
                }).collect();
                Ok(ToolResult::ok(format!("{} task(s):\n\n{}", tasks.len(), lines.join("\n\n"))))
            }
            Err(e) => Ok(ToolResult::err(format!("Failed to list tasks: {}", e))),
        }
    }

    async fn task_create(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let name = match input["name"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(n) => n,
            None => return Ok(ToolResult::err("'name' is required for task_create")),
        };
        let cron = match input["cron_expression"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(c) => c,
            None => return Ok(ToolResult::err(
                "'cron_expression' is required (5 fields, e.g. '0 * * * *' = every hour)"
            )),
        };
        let prompt = match input["task_prompt"].as_str().filter(|s| !s.trim().is_empty()) {
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
        let name   = input["name"].as_str();
        let cron   = input["cron_expression"].as_str();
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
            Err(e)   => return Ok(ToolResult::err(format!("Failed to look up task: {}", e))),
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
            Err(e)   => Ok(ToolResult::err(format!("Failed to look up task: {}", e))),
            Ok(Some(t)) => match db.delete_task(id) {
                Ok(_) => Ok(ToolResult::ok(format!("Task '{}' ('{}') deleted.", id, t.name))),
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
            if key.is_empty() { "(not set)".to_string() }
            else if key.len() <= 4 { "****".to_string() }
            else { format!("****{}", &key[key.len()-4..]) }
        };
        let provider_key = match s.provider.as_str() {
            "openai" | "custom" => mask(&s.openai_api_key),
            "deepseek"          => mask(&s.deepseek_api_key),
            "qwen" | "tongyi"   => mask(&s.qwen_api_key),
            "minimax"           => mask(&s.minimax_api_key),
            "zhipu"             => mask(&s.zhipu_api_key),
            "kimi" | "moonshot" => mask(&s.kimi_api_key),
            _                   => mask(&s.anthropic_api_key),
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
             - confirm_shell_commands: {confirm_shell}\n\
             - confirm_file_writes: {confirm_file}\n\
             \nUI:\n\
             - language: {lang}\n\
             - browser_headless: {headless}\n\
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
             - wecom_enabled: {wecom_enabled}\n\
             - wecom_corp_id: {wecom_corp_id}\n\
             - telegram_enabled: {telegram_enabled}\n\
             - telegram_bot_token: {telegram_bot_token}\
             {ssh_section}",
            provider = s.provider,
            model = s.model,
            base_url = if s.custom_base_url.is_empty() { "(none)".to_string() } else { s.custom_base_url.clone() },
            key = provider_key,
            max_tokens = s.max_tokens,
            ctx_win = s.context_window,
            max_iter = s.max_iterations,
            policy = s.policy_mode,
            workspace = s.workspace_root,
            confirm_shell = s.confirm_shell_commands,
            confirm_file = s.confirm_file_writes,
            lang = s.language,
            headless = s.browser_headless,
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
            wecom_enabled = s.wecom_enabled,
            wecom_corp_id = configured(&s.wecom_corp_id),
            telegram_enabled = s.telegram_enabled,
            telegram_bot_token = configured(&s.telegram_bot_token),
            ssh_section = if s.ssh_servers.is_empty() {
                "\n\nSSH Servers:\n- (none configured — add servers in Settings > SSH Servers)".to_string()
            } else {
                let lines: Vec<String> = s.ssh_servers.iter().map(|srv| {
                    let auth = if !srv.password.is_empty() { "password" }
                               else if !srv.private_key.is_empty() { "key" }
                               else { "no-auth" };
                    format!("  - '{}' ({}): {}@{}:{} [{}]",
                        srv.id,
                        if srv.label.is_empty() { &srv.id } else { &srv.label },
                        srv.username, srv.host, srv.port, auth)
                }).collect();
                format!("\n\nSSH Servers ({} configured):\n{}", s.ssh_servers.len(), lines.join("\n"))
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
        apply_bool!(confirm_shell_commands, "confirm_shell_commands");
        apply_bool!(confirm_file_writes, "confirm_file_writes");
        apply_str!(language, "language");
        apply_bool!(browser_headless, "browser_headless");
        apply_bool!(heartbeat_enabled, "heartbeat_enabled");
        apply_u32!(heartbeat_interval_mins, "heartbeat_interval_mins");
        apply_str!(heartbeat_prompt, "heartbeat_prompt");

        // API key — stored into the correct field based on provider
        if let Some(key) = input["api_key"].as_str().filter(|k| !k.trim().is_empty()) {
            let target = input["provider"].as_str().unwrap_or(&s.provider).to_string();
            match target.as_str() {
                "openai" | "custom" => { s.openai_api_key = key.to_string(); }
                "deepseek"          => { s.deepseek_api_key = key.to_string(); }
                "qwen" | "tongyi"   => { s.qwen_api_key = key.to_string(); }
                "minimax"           => { s.minimax_api_key = key.to_string(); }
                "zhipu"             => { s.zhipu_api_key = key.to_string(); }
                "kimi" | "moonshot" => { s.kimi_api_key = key.to_string(); }
                _                   => { s.anthropic_api_key = key.to_string(); }
            }
            changed.push(format!("api_key ({}) = ****{}",
                target,
                if key.len() > 4 { &key[key.len()-4..] } else { "****" }
            ));
        }

        if changed.is_empty() {
            return Ok(ToolResult::err(
                "No recognized fields provided. Use settings_get to see available fields."
            ));
        }

        match s.save() {
            Ok(_) => Ok(ToolResult::ok(format!(
                "Settings saved. Changed:\n{}",
                changed.iter().map(|c| format!("  - {}", c)).collect::<Vec<_>>().join("\n")
            ))),
            Err(e) => Ok(ToolResult::err(format!("Failed to save settings: {}", e))),
        }
    }

    // ── Skills ────────────────────────────────────────────────────────────────

    async fn skill_list(&self) -> anyhow::Result<ToolResult> {
        // DB skills (enabled/disabled state)
        let db_skills = {
            let db = self.db.lock().await;
            db.list_skills().unwrap_or_default()
        };

        // File-system skills (actual SKILL.md definitions)
        let skills_dir = self.app_data_dir.join("skills");
        let mut loader = SkillLoader::new(&skills_dir);
        if let Err(e) = loader.load_all() {
            warn!("skill_list: failed to load skills from disk: {}", e);
        }
        let fs_skills = loader.list_skills();

        if db_skills.is_empty() && fs_skills.is_empty() {
            return Ok(ToolResult::ok("No skills installed."));
        }

        // Merge: prefer FS info, annotate with DB enabled state
        let mut lines: Vec<String> = Vec::new();

        // Show FS skills with DB enabled state
        for skill in &fs_skills {
            let db_entry = db_skills.iter().find(|s| s.name == skill.name || s.id == skill.name);
            let enabled = db_entry.map(|s| s.enabled).unwrap_or(true);
            let skill_id = db_entry.map(|s| s.id.as_str()).unwrap_or(&skill.name);
            lines.push(format!(
                "ID: {}\n  Name: {}\n  Description: {}\n  Version: {}\n  Source: {}\n  Enabled: {}\n  Tools: {}\n  Dependencies: {}",
                skill_id,
                skill.name,
                skill.description,
                skill.version,
                skill.source,
                enabled,
                if skill.tools.is_empty() { "(none)".to_string() } else { skill.tools.join(", ") },
                if skill.dependencies.is_empty() { "(none)".to_string() } else { skill.dependencies.join(", ") },
            ));
        }

        // Also show DB-only skills not found on FS
        for db_skill in &db_skills {
            let in_fs = fs_skills.iter().any(|s| s.name == db_skill.name || s.name == db_skill.id);
            if !in_fs {
                lines.push(format!(
                    "ID: {}\n  Name: {}\n  Description: {}\n  Enabled: {}\n  (definition file not found on disk)",
                    db_skill.id, db_skill.name, db_skill.description, db_skill.enabled
                ));
            }
        }

        Ok(ToolResult::ok(format!("{} skill(s):\n\n{}", lines.len(), lines.join("\n\n"))))
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
            (format!("{}/api/v1/skills?sort=stars&limit={}", CLAWHUB_API, limit), false)
        } else {
            (format!("{}/api/v1/search?q={}&limit={}", CLAWHUB_API, urlencoding::encode(&query), limit), true)
        };

        info!("skill_search: {}", url);

        let resp = clawhub_get_with_retry(&client, &url, 3).await
            .map_err(|e| anyhow::anyhow!("Cannot reach ClawHub: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let hint = if status.as_u16() == 429 { " (rate limited, please retry later)" } else { "" };
            let body = resp.text().await.unwrap_or_default();
            return Ok(ToolResult::err(format!("ClawHub HTTP {}{}: {}", status, hint,
                if body.len() > 200 { &body[..200] } else { &body })));
        }

        let body: Value = resp.json().await
            .map_err(|e| anyhow::anyhow!("Invalid ClawHub response: {}", e))?;

        let items: Vec<String> = if use_search {
            body["results"].as_array().cloned().unwrap_or_default()
                .iter()
                .filter_map(|r| {
                    let slug = r["slug"].as_str()?;
                    let name = r["displayName"].as_str().unwrap_or(slug);
                    let desc = r["summary"].as_str().unwrap_or("");
                    let ver  = r["version"].as_str().unwrap_or("latest");
                    Some(format!("Slug: {}\n  Name: {}\n  Version: {}\n  Description: {}", slug, name, ver, desc))
                }).collect()
        } else {
            body["items"].as_array().cloned().unwrap_or_default()
                .iter()
                .filter_map(|r| {
                    let slug = r["slug"].as_str()?;
                    let name = r["displayName"].as_str().unwrap_or(slug);
                    let desc = r["summary"].as_str().unwrap_or("");
                    let ver  = r["latestVersion"]["version"].as_str().unwrap_or("latest");
                    let stars = r["stats"]["stars"].as_u64().unwrap_or(0);
                    Some(format!("Slug: {}\n  Name: {}\n  Version: {}\n  Stars: {}\n  Description: {}", slug, name, ver, stars, desc))
                }).collect()
        };

        if items.is_empty() {
            return Ok(ToolResult::ok(format!("No skills found for query '{}'.", query)));
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
                "'source' is required: provide a ClawHub slug (e.g. 'pptx-maker') or a direct URL"
            )),
        };

        let skills_dir = self.app_data_dir.join("skills");

        // Determine if source is a URL or a slug
        let content = if source.starts_with("http://") || source.starts_with("https://") {
            // Direct URL — download SKILL.md
            let blocked = ["localhost", "127.0.0.1", "0.0.0.0", "192.168.", "10.", "172."];
            for pat in blocked {
                if source.contains(pat) {
                    return Ok(ToolResult::err(format!("Blocked URL: '{}' points to a private address", source)));
                }
            }
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("Pisci-Desktop/1.0")
                .build().map_err(|e| anyhow::anyhow!(e))?;
            let resp = client.get(&source).send().await
                .map_err(|e| anyhow::anyhow!("Download failed: {}", e))?;
            if !resp.status().is_success() {
                return Ok(ToolResult::err(format!("HTTP {} fetching URL", resp.status())));
            }
            resp.text().await.map_err(|e| anyhow::anyhow!(e))?
        } else {
            // Treat as ClawHub slug
            if !source.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.') {
                return Ok(ToolResult::err(format!("Invalid slug '{}': use alphanumeric, hyphens, underscores only", source)));
            }
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("Pisci-Desktop/1.0")
                .build().map_err(|e| anyhow::anyhow!(e))?;
            let file_url = format!("{}/api/v1/skills/{}/file?path=SKILL.md", CLAWHUB_API, source);
            info!("skill_install: fetching {} from {}", source, file_url);
            let resp = clawhub_get_with_retry(&client, &file_url, 3).await
                .map_err(|e| anyhow::anyhow!("ClawHub request failed: {}", e))?;
            if resp.status().is_success() {
                resp.text().await.map_err(|e| anyhow::anyhow!(e))?
            } else {
                let file_status = resp.status();
                // Fallback: zip download
                let zip_url = format!("{}/api/v1/download?slug={}", CLAWHUB_API, source);
                info!("skill_install: file endpoint failed ({}), trying zip: {}", file_status, zip_url);
                let zip_resp = clawhub_get_with_retry(&client, &zip_url, 3).await
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

        // Parse and validate
        let loader = SkillLoader::new(&skills_dir);
        let skill = loader.parse_skill_from_content(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse SKILL.md: {}", e))?;

        if skill.name.is_empty() || skill.name == "unnamed" {
            return Ok(ToolResult::err("SKILL.md must declare a 'name' field in frontmatter"));
        }

        // Compatibility check
        let compat = crate::skills::loader::check_skill_compatibility(&skill).await;
        if !compat.compatible {
            return Ok(ToolResult::err(format!(
                "Skill '{}' is incompatible with this system:\n{}",
                skill.name, compat.issues.join("\n")
            )));
        }
        for w in &compat.warnings {
            warn!("Skill '{}' warning: {}", skill.name, w);
        }

        // Write to disk
        let safe_name: String = skill.name.chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect::<String>().to_lowercase();
        let skill_dir = skills_dir.join(&safe_name);
        tokio::fs::create_dir_all(&skill_dir).await
            .map_err(|e| anyhow::anyhow!("Failed to create skill dir: {}", e))?;
        // Register in DB first — abort before touching filesystem if this fails
        {
            let db = self.db.lock().await;
            db.upsert_skill(&safe_name, &skill.name, &skill.description, "📦")
                .map_err(|e| anyhow::anyhow!("Failed to register skill in database: {}", e))?;
        }

        if let Err(e) = tokio::fs::write(skill_dir.join("SKILL.md"), &content).await {
            // Roll back DB entry on filesystem failure
            let db = self.db.lock().await;
            let _ = db.delete_skill(&safe_name);
            let _ = tokio::fs::remove_dir_all(&skill_dir).await;
            return Err(anyhow::anyhow!("Failed to write SKILL.md: {}", e));
        }

        info!("Installed skill '{}' to {:?}", skill.name, skill_dir);

        let warn_msg = if compat.warnings.is_empty() {
            String::new()
        } else {
            format!("\nWarnings:\n{}", compat.warnings.iter().map(|w| format!("  - {}", w)).collect::<Vec<_>>().join("\n"))
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
            if skill.tools.is_empty() { "(none)".to_string() } else { skill.tools.join(", ") },
            if skill.dependencies.is_empty() { "(none)".to_string() } else { skill.dependencies.join(", ") },
            warn_msg,
        )))
    }

    async fn skill_toggle(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let skill_id = match input["skill_id"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(i) => i,
            None => return Ok(ToolResult::err("'skill_id' is required for skill_toggle (get IDs from skill_list)")),
        };
        let enabled = match input["enabled"].as_bool() {
            Some(e) => e,
            None => return Ok(ToolResult::err("'enabled' (boolean) is required for skill_toggle")),
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
        let skill_name = match input["skill_name"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(n) => n,
            None => return Ok(ToolResult::err("'skill_name' is required for skill_uninstall")),
        };

        let skills_dir = self.app_data_dir.join("skills");
        let safe_name: String = skill_name.chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect::<String>().to_lowercase();
        let skill_dir = skills_dir.join(&safe_name);

        if !skill_dir.exists() {
            return Ok(ToolResult::err(format!("Skill '{}' not found on disk", skill_name)));
        }

        // Safety: must be inside skills_dir
        let canonical_dir = skill_dir.canonicalize()
            .map_err(|e| anyhow::anyhow!("Path error: {}", e))?;
        let canonical_skills = skills_dir.canonicalize()
            .map_err(|e| anyhow::anyhow!("Path error: {}", e))?;
        if !canonical_dir.starts_with(&canonical_skills) {
            return Ok(ToolResult::err("Path traversal attempt blocked"));
        }

        // Remove from DB first — abort before touching filesystem if this fails
        {
            let db = self.db.lock().await;
            db.delete_skill(&safe_name)
                .map_err(|e| anyhow::anyhow!("Failed to remove skill from database: {}", e))?;
        }

        // Remove files; if this fails the DB entry is already gone (acceptable)
        if skill_dir.exists() {
            tokio::fs::remove_dir_all(&skill_dir).await
                .map_err(|e| anyhow::anyhow!("Skill removed from database but failed to delete files: {}", e))?;
        }

        info!("Uninstalled skill '{}'", skill_name);
        Ok(ToolResult::ok(format!("Skill '{}' uninstalled.", skill_name)))
    }
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
