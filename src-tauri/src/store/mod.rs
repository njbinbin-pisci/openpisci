// The low-level storage primitives (`Database`, `Settings`) now live in
// `piscis-kernel::store`. We re-export them here so existing call sites that
// reference `crate::store::db::...` / `crate::store::settings::...` /
// `crate::store::{Database, Settings}` keep compiling unchanged.
pub use piscis_kernel::store::{db, settings, Database, Settings};

use anyhow::Result;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

use crate::lsp::manager::LspManager;

/// Global application state managed by Tauri
pub struct AppState {
    pub db: Arc<Mutex<Database>>,
    pub settings: Arc<Mutex<Settings>>,
    /// Current visible execution plan per session
    pub plan_state: Arc<
        Mutex<std::collections::HashMap<String, Vec<piscis_kernel::agent::plan::PlanTodoItem>>>,
    >,
    /// Active agent cancellation tokens: session_id -> cancel flag
    pub cancel_flags:
        Arc<Mutex<std::collections::HashMap<String, Arc<std::sync::atomic::AtomicBool>>>>,
    /// Shared browser manager (Chrome for Testing)
    pub browser: robotz_browser::SharedBrowserManager,
    /// Cron scheduler for recurring tasks
    pub scheduler: Arc<piscis_kernel::scheduler::cron::CronScheduler>,
    /// Active cron job ids keyed by scheduled task id so updates/restarts can replace jobs instead of duplicating them.
    pub scheduled_job_ids: Arc<Mutex<std::collections::HashMap<String, uuid::Uuid>>>,
    /// App handle for emitting events from scheduler tasks
    pub app_handle: AppHandle,
    /// Pending permission confirmation channels: request_id -> oneshot sender
    pub confirmation_responses:
        Arc<Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<bool>>>>,
    /// Pending interactive UI response channels: request_id -> oneshot sender
    pub interactive_responses: Arc<
        Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>,
    >,
    /// IM gateway manager
    pub gateway: Arc<crate::gateway::GatewayManager>,
    /// Per-pool heartbeat cursor: pool_id -> last processed pool_message.id
    pub piscis_heartbeat_cursor: Arc<Mutex<std::collections::HashMap<String, i64>>>,
    /// IDE terminal session registry
    pub terminals: Arc<Mutex<crate::commands::ide::TerminalRegistry>>,
    /// IDE file watcher registry: project_dir -> notify watcher handle
    pub file_watchers: Arc<Mutex<std::collections::HashMap<String, notify::RecommendedWatcher>>>,
    /// LSP (Language Server Protocol) session manager
    pub lsp_manager: Arc<LspManager>,
}

impl AppState {
    /// Synchronous construction — scheduler must be provided after async init.
    pub fn new_sync(
        app: &AppHandle,
        scheduler: piscis_kernel::scheduler::cron::CronScheduler,
    ) -> Result<Self> {
        let app_dir = app
            .path()
            .app_data_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from(".piscis"));
        Self::new_sync_with_app_dir(app, scheduler, app_dir)
    }

    /// Synchronous construction with an explicit app-data directory.
    /// Used by headless/CLI entry points so they can run against an isolated
    /// config + database root without mutating the desktop app's default state.
    pub fn new_sync_with_app_dir(
        app: &AppHandle,
        scheduler: piscis_kernel::scheduler::cron::CronScheduler,
        app_dir: std::path::PathBuf,
    ) -> Result<Self> {
        std::fs::create_dir_all(&app_dir)?;

        let db_path = app_dir.join("piscis.db");
        let db = Database::open(&db_path)?;

        let config_path = app_dir.join("config.json");
        let mut settings = Settings::load(&config_path)?;
        if crate::commands::config::bundled_mcp::strip_legacy_robotz_mcp_server(&mut settings) {
            settings.save().map_err(|e| anyhow::anyhow!("{e}"))?;
        }

        let browser_options = robotz_browser::BrowserOptions {
            chrome_dir: app_dir.join("chrome"),
            headless: settings.browser_headless,
            ..Default::default()
        };

        Ok(Self {
            db: Arc::new(Mutex::new(db)),
            settings: Arc::new(Mutex::new(settings)),
            plan_state: Arc::new(Mutex::new(std::collections::HashMap::new())),
            cancel_flags: Arc::new(Mutex::new(std::collections::HashMap::new())),
            browser: robotz_browser::create_browser_manager(browser_options),
            scheduler: Arc::new(scheduler),
            scheduled_job_ids: Arc::new(Mutex::new(std::collections::HashMap::new())),
            app_handle: app.clone(),
            confirmation_responses: Arc::new(Mutex::new(std::collections::HashMap::new())),
            interactive_responses: Arc::new(Mutex::new(std::collections::HashMap::new())),
            gateway: Arc::new(crate::gateway::GatewayManager::new()),
            piscis_heartbeat_cursor: Arc::new(Mutex::new(std::collections::HashMap::new())),
            terminals: Arc::new(Mutex::new(crate::commands::ide::TerminalRegistry::new())),
            file_watchers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            lsp_manager: Arc::new(LspManager::new()),
        })
    }
}
