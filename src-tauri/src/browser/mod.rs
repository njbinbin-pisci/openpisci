/// Browser module — Chrome for Testing lifecycle management via CDP.
pub mod download;

use anyhow::{Context, Result};
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures::StreamExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Configuration for the browser manager
#[derive(Debug, Clone)]
pub struct BrowserOptions {
    /// Run in headless mode (default: true)
    pub headless: bool,
    /// Custom Chrome executable path (auto-detected if None)
    pub chrome_path: Option<PathBuf>,
    /// Directory to store Chrome for Testing downloads
    pub chrome_dir: PathBuf,
    /// Custom user-data-dir (isolated profile)
    pub user_data_dir: Option<PathBuf>,
    /// HTTP proxy (e.g. "http://127.0.0.1:8080")
    pub proxy: Option<String>,
    /// Window width (for headed mode)
    pub window_width: u32,
    /// Window height (for headed mode)
    pub window_height: u32,
}

impl Default for BrowserOptions {
    fn default() -> Self {
        let chrome_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("com.pisci.desktop")
            .join("chrome");
        Self {
            headless: true,
            chrome_path: None,
            chrome_dir,
            user_data_dir: None,
            proxy: None,
            window_width: 1280,
            window_height: 800,
        }
    }
}

/// Manages a single Chrome browser instance and its pages.
pub struct BrowserManager {
    browser: Option<Browser>,
    pages: HashMap<String, Arc<Page>>,
    pub active_tab: Option<String>,
    options: BrowserOptions,
    /// Background handler task handle
    _handler: Option<tokio::task::JoinHandle<()>>,
}

impl BrowserManager {
    pub fn new(options: BrowserOptions) -> Self {
        Self {
            browser: None,
            pages: HashMap::new(),
            active_tab: None,
            options,
            _handler: None,
        }
    }

    pub fn headless(&self) -> bool {
        self.options.headless
    }

    pub fn set_headless(&mut self, headless: bool) {
        self.options.headless = headless;
    }

    /// Ensure Chrome is available; download only as last resort.
    pub async fn ensure_chrome(&self) -> Result<PathBuf> {
        // 1. Use explicitly configured path (highest priority)
        if let Some(ref path) = self.options.chrome_path {
            if path.exists() {
                return Ok(path.clone());
            }
            warn!("Configured chrome_path does not exist: {}", path.display());
        }

        // 2. System browser: Edge (Win built-in) → Chrome → Brave
        //    This avoids any download for the vast majority of users.
        if let Some(sys) = download::find_system_chrome() {
            info!("Using system browser: {}", sys.display());
            return Ok(sys);
        }

        // 3. Previously downloaded Chrome for Testing
        if let Some(exe) = download::chrome_exists(&self.options.chrome_dir) {
            info!("Using cached Chrome for Testing: {}", exe.display());
            return Ok(exe);
        }

        // 4. Download Chrome for Testing (last resort — requires internet + ~111 MB)
        info!("No Chromium-based browser found on system, downloading Chrome for Testing...");
        download::download_chrome_for_testing(&self.options.chrome_dir).await
    }

    /// Launch the browser if not already running.
    pub async fn launch(&mut self) -> Result<()> {
        if self.browser.is_some() {
            return Ok(());
        }

        let chrome_path = self.ensure_chrome().await?;
        let chrome_path_str = chrome_path.display().to_string();
        let is_edge = chrome_path_str.to_lowercase().contains("msedge")
            || chrome_path_str.to_lowercase().contains("edge");
        info!("Launching browser: headless={} edge={} path={}", self.options.headless, is_edge, chrome_path_str);

        let mut builder = BrowserConfig::builder()
            .chrome_executable(chrome_path)
            .window_size(self.options.window_width, self.options.window_height)
            .arg("--no-sandbox")
            .arg("--disable-dev-shm-usage")
            .arg("--no-first-run")
            .arg("--disable-background-networking")
            .arg("--disable-client-side-phishing-detection")
            .arg("--disable-default-apps")
            .arg("--disable-extensions")
            .arg("--disable-sync")
            // Suppress automation banner and improve CDP stability
            .arg("--disable-blink-features=AutomationControlled")
            .arg("--disable-infobars");

        // --disable-gpu can cause CDP response decode errors with Edge headless on some versions;
        // only apply it on non-Edge browsers where it is safe.
        if !is_edge {
            builder = builder.arg("--disable-gpu");
        }

        // Extra flags for Edge to suppress SmartScreen and first-run UX
        if is_edge {
            builder = builder
                .arg("--disable-features=msSmartScreenNewsAndInterests,SmartScreenEnabled")
                .arg("--no-default-browser-check")
                .arg("--disable-notifications");
        }

        // with_head() enables headed (visible) mode; omitting it = headless.
        // --new-window forces a visible window on Windows; --start-maximized brings it to front.
        if !self.options.headless {
            builder = builder
                .with_head()
                .arg("--new-window")
                .arg("--start-maximized");
        }

        if let Some(ref proxy) = self.options.proxy {
            builder = builder.arg(format!("--proxy-server={}", proxy));
        }

        // Always use an isolated user-data-dir so Chrome doesn't reuse an existing
        // running instance (Chrome's single-instance lock would cause the new process
        // to hand off to the existing one and exit immediately, breaking CDP).
        // Use a random temp directory to avoid stale lock files from previous crashes.
        let udd = if let Some(ref udd) = self.options.user_data_dir {
            udd.clone()
        } else {
            // Unique temp profile per launch — avoids SingletonLock conflicts entirely
            let unique = format!("pisci-chrome-{}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis());
            std::env::temp_dir().join(unique)
        };
        std::fs::create_dir_all(&udd).ok();
        builder = builder.user_data_dir(udd.clone());

        let config = builder
            .build()
            .map_err(|e| anyhow::anyhow!("BrowserConfig error: {}", e))?;

        let udd_str = udd.display().to_string();
        let (browser, handler) = Browser::launch(config)
            .await
            .map_err(|e| anyhow::anyhow!(
                "Failed to launch Chrome (path={}, udd={}): {}",
                chrome_path_str,
                udd_str,
                e
            ))?;

        // Spawn handler loop (required by chromiumoxide to process CDP events)
        let handle = tokio::spawn(async move {
            let mut h = handler;
            while h.next().await.is_some() {}
        });

        self.browser = Some(browser);
        self._handler = Some(handle);
        info!("Chrome launched successfully");
        Ok(())
    }

    /// Get or create a page (tab) by ID.
    pub async fn get_or_create_page(&mut self, tab_id: &str) -> Result<Arc<Page>> {
        self.launch().await?;

        if let Some(page) = self.pages.get(tab_id) {
            return Ok(page.clone());
        }

        let browser = self.browser.as_ref().ok_or_else(|| anyhow::anyhow!("Browser not running"))?;
        let page = browser.new_page("about:blank").await.context("Failed to create new page")?;
        let page = Arc::new(page);
        self.pages.insert(tab_id.to_string(), page.clone());
        self.active_tab = Some(tab_id.to_string());
        Ok(page)
    }

    /// Always create a fresh page and bind it to tab_id.
    pub async fn create_page(&mut self, tab_id: &str) -> Result<Arc<Page>> {
        self.launch().await?;
        let browser = self
            .browser
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Browser not running"))?;
        let page = browser
            .new_page("about:blank")
            .await
            .context("Failed to create new page")?;
        let page = Arc::new(page);
        self.pages.insert(tab_id.to_string(), page.clone());
        self.active_tab = Some(tab_id.to_string());
        Ok(page)
    }

    /// Get the active page, creating one if needed.
    pub async fn active_page(&mut self) -> Result<Arc<Page>> {
        let tab_id = self.active_tab.clone().unwrap_or_else(|| "default".to_string());
        self.get_or_create_page(&tab_id).await
    }

    /// List all open tabs
    pub fn list_tabs(&self) -> Vec<String> {
        self.pages.keys().cloned().collect()
    }

    /// Switch active tab
    pub fn switch_tab(&mut self, tab_id: &str) -> Result<()> {
        if self.pages.contains_key(tab_id) {
            self.active_tab = Some(tab_id.to_string());
            Ok(())
        } else {
            Err(anyhow::anyhow!("Tab '{}' not found", tab_id))
        }
    }

    /// Close a tab
    pub async fn close_tab(&mut self, tab_id: &str) -> Result<()> {
        if let Some(page) = self.pages.remove(tab_id) {
            // Page::close consumes Page, so clone the inner page value first.
            page.as_ref().clone()
                .close()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to close tab '{}': {}", tab_id, e))?;
        }
        if self.active_tab.as_deref() == Some(tab_id) {
            self.active_tab = self.pages.keys().next().cloned();
        }
        Ok(())
    }

    /// Close the browser entirely
    pub async fn close(&mut self) {
        self.pages.clear();
        self.active_tab = None;
        // Drop the browser (chromiumoxide will close Chrome on drop)
        self.browser.take();
        if let Some(handle) = self._handler.take() {
            handle.abort();
        }
    }

    pub fn is_running(&self) -> bool {
        self.browser.is_some()
    }
}

/// Thread-safe wrapper around BrowserManager stored in AppState
pub type SharedBrowserManager = Arc<Mutex<BrowserManager>>;

pub fn create_browser_manager(options: BrowserOptions) -> SharedBrowserManager {
    Arc::new(Mutex::new(BrowserManager::new(options)))
}
