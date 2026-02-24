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

    /// Ensure Chrome is available; download if necessary.
    pub async fn ensure_chrome(&self) -> Result<PathBuf> {
        // 1. Use explicitly configured path
        if let Some(ref path) = self.options.chrome_path {
            if path.exists() {
                return Ok(path.clone());
            }
            warn!("Configured chrome_path does not exist: {}", path.display());
        }

        // 2. Check if already downloaded
        if let Some(exe) = download::chrome_exists(&self.options.chrome_dir) {
            info!("Using cached Chrome for Testing: {}", exe.display());
            return Ok(exe);
        }

        // 3. Try system Chrome
        if let Some(sys) = download::find_system_chrome() {
            info!("Using system Chrome: {}", sys.display());
            return Ok(sys);
        }

        // 4. Download Chrome for Testing
        info!("No Chrome found, downloading Chrome for Testing...");
        download::download_chrome_for_testing(&self.options.chrome_dir).await
    }

    /// Launch the browser if not already running.
    pub async fn launch(&mut self) -> Result<()> {
        if self.browser.is_some() {
            return Ok(());
        }

        let chrome_path = self.ensure_chrome().await?;
        info!("Launching Chrome: {}", chrome_path.display());

        let mut builder = BrowserConfig::builder()
            .chrome_executable(chrome_path)
            .window_size(self.options.window_width, self.options.window_height)
            .arg("--no-sandbox")
            .arg("--disable-dev-shm-usage")
            .arg("--disable-gpu")
            .arg("--no-first-run");

        // with_head() enables headed (visible) mode; omitting it = headless
        if !self.options.headless {
            builder = builder.with_head();
        }

        if let Some(ref proxy) = self.options.proxy {
            builder = builder.arg(format!("--proxy-server={}", proxy));
        }

        if let Some(ref udd) = self.options.user_data_dir {
            builder = builder.user_data_dir(udd);
        }

        let config = builder
            .build()
            .map_err(|e| anyhow::anyhow!("BrowserConfig error: {}", e))?;

        let (browser, mut handler) = Browser::launch(config)
            .await
            .context("Failed to launch Chrome")?;

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
