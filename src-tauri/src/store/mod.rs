pub mod db;
pub mod settings;

use anyhow::Result;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

pub use db::Database;
pub use settings::Settings;

/// Global application state managed by Tauri
pub struct AppState {
    pub db: Arc<Mutex<Database>>,
    pub settings: Arc<Mutex<Settings>>,
    /// Current visible execution plan per session
    pub plan_state:
        Arc<Mutex<std::collections::HashMap<String, Vec<crate::agent::plan::PlanTodoItem>>>>,
    /// Active agent cancellation tokens: session_id -> cancel flag
    pub cancel_flags:
        Arc<Mutex<std::collections::HashMap<String, Arc<std::sync::atomic::AtomicBool>>>>,
    /// Shared browser manager (Chrome for Testing)
    pub browser: crate::browser::SharedBrowserManager,
    /// Cron scheduler for recurring tasks
    pub scheduler: Arc<crate::scheduler::cron::CronScheduler>,
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
    pub pisci_heartbeat_cursor: Arc<Mutex<std::collections::HashMap<String, i64>>>,
}

impl AppState {
    /// Synchronous construction — scheduler must be provided after async init.
    pub fn new_sync(
        app: &AppHandle,
        scheduler: crate::scheduler::cron::CronScheduler,
    ) -> Result<Self> {
        let app_dir = app
            .path()
            .app_data_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
        Self::new_sync_with_app_dir(app, scheduler, app_dir)
    }

    /// Synchronous construction with an explicit app-data directory.
    /// Used by headless/CLI entry points so they can run against an isolated
    /// config + database root without mutating the desktop app's default state.
    pub fn new_sync_with_app_dir(
        app: &AppHandle,
        scheduler: crate::scheduler::cron::CronScheduler,
        app_dir: std::path::PathBuf,
    ) -> Result<Self> {
        std::fs::create_dir_all(&app_dir)?;

        let db_path = app_dir.join("pisci.db");
        let db = Database::open(&db_path)?;

        let config_path = app_dir.join("config.json");
        let settings = Settings::load(&config_path)?;

        let browser_options = crate::browser::BrowserOptions {
            chrome_dir: app_dir.join("chrome"),
            headless: settings.browser_headless,
            ..Default::default()
        };

        Ok(Self {
            db: Arc::new(Mutex::new(db)),
            settings: Arc::new(Mutex::new(settings)),
            plan_state: Arc::new(Mutex::new(std::collections::HashMap::new())),
            cancel_flags: Arc::new(Mutex::new(std::collections::HashMap::new())),
            browser: crate::browser::create_browser_manager(browser_options),
            scheduler: Arc::new(scheduler),
            app_handle: app.clone(),
            confirmation_responses: Arc::new(Mutex::new(std::collections::HashMap::new())),
            interactive_responses: Arc::new(Mutex::new(std::collections::HashMap::new())),
            gateway: Arc::new(crate::gateway::GatewayManager::new()),
            pisci_heartbeat_cursor: Arc::new(Mutex::new(std::collections::HashMap::new())),
        })
    }
}
