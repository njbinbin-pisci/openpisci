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
    /// Active agent cancellation tokens: session_id -> cancel flag
    pub cancel_flags: Arc<Mutex<std::collections::HashMap<String, Arc<std::sync::atomic::AtomicBool>>>>,
    /// Shared browser manager (Chrome for Testing)
    pub browser: crate::browser::SharedBrowserManager,
}

impl AppState {
    pub fn new(app: &AppHandle) -> Result<Self> {
        let app_dir = app
            .path()
            .app_data_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
        std::fs::create_dir_all(&app_dir)?;

        let db_path = app_dir.join("pisci.db");
        let db = Database::open(&db_path)?;

        let config_path = app_dir.join("config.json");
        let settings = Settings::load(&config_path)?;

        let browser_options = crate::browser::BrowserOptions {
            chrome_dir: app_dir.join("chrome"),
            ..Default::default()
        };

        Ok(Self {
            db: Arc::new(Mutex::new(db)),
            settings: Arc::new(Mutex::new(settings)),
            cancel_flags: Arc::new(Mutex::new(std::collections::HashMap::new())),
            browser: crate::browser::create_browser_manager(browser_options),
        })
    }
}
