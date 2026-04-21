//! Desktop (Tauri) implementation of the `pisci-core` host traits.
//!
//! The kernel consumes these traits to surface events, prompt the user, attach
//! platform-specific tools, and persist secrets. On desktop we back them onto:
//!   * Tauri events (for EventSink + toast notifier)
//!   * Shared oneshot-channel maps held in `AppState` (for confirmation and
//!     interactive prompts)
//!   * [`DesktopHostTools`] (for platform tools — browser, UIA, screen,
//!     app_control, plan_todo, chat_ui, call_fish/koi, pool_org/chat,
//!     Windows-only COM/WMI/Office, and the kernel's neutral set)
//!   * The on-disk `Settings` object (for encrypted secrets)
//!
//! Creating a host is cheap (just clones `Arc`s). The resulting `DesktopHost`
//! can be handed to kernel entry points as `Arc<dyn HostRuntime>`.

use pisci_core::host::{
    ConfirmRequest, EventSink, HostRuntime, HostTools, InteractiveRequest, Notifier, SecretsStore,
    ToolRegistryHandle,
};
use pisci_kernel::agent::tool::{new_tool_registry_handle, ToolRegistry, ToolRegistryHandleExt};
use pisci_kernel::tools::NeutralToolsConfig;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;
use tokio::sync::Mutex;

use crate::browser::SharedBrowserManager;
use crate::skills::loader::SkillLoader;
use crate::store::{AppState, Database, Settings};
use crate::tools::{
    app_control, browser, call_fish, call_koi, chat_ui, plan_todo, pool_chat, pool_org, skill_list,
};

#[cfg(target_os = "windows")]
use crate::tools::{com_invoke, com_tool, office, powershell, screen, uia, wmi_tool};

// ─── Shared maps -------------------------------------------------------------

pub type ConfirmationResponseMap =
    Arc<Mutex<std::collections::HashMap<String, oneshot::Sender<bool>>>>;
pub type InteractiveResponseMap =
    Arc<Mutex<std::collections::HashMap<String, oneshot::Sender<serde_json::Value>>>>;

// ─── EventSink --------------------------------------------------------------

#[derive(Clone)]
pub struct DesktopEventSink {
    app: AppHandle,
}

impl DesktopEventSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl EventSink for DesktopEventSink {
    fn emit_session(&self, session_id: &str, event: &str, payload: Value) {
        let mut payload = payload;
        // Ensure session_id always travels with the event so frontend reducers
        // can route the payload without guessing.
        if let Value::Object(ref mut map) = payload {
            map.entry("session_id".to_string())
                .or_insert_with(|| Value::String(session_id.to_string()));
        }
        let _ = self.app.emit(event, payload);
    }

    fn emit_broadcast(&self, event: &str, payload: Value) {
        let _ = self.app.emit(event, payload);
    }
}

// ─── Notifier ----------------------------------------------------------------

#[derive(Clone)]
pub struct DesktopNotifier {
    app: AppHandle,
    confirmations: ConfirmationResponseMap,
    interactives: InteractiveResponseMap,
}

impl DesktopNotifier {
    pub fn new(
        app: AppHandle,
        confirmations: ConfirmationResponseMap,
        interactives: InteractiveResponseMap,
    ) -> Self {
        Self {
            app,
            confirmations,
            interactives,
        }
    }
}

#[async_trait::async_trait]
impl Notifier for DesktopNotifier {
    fn toast(&self, level: &str, message: &str, pool_id: Option<&str>, duration_ms: Option<u64>) {
        let payload = json!({
            "level": level,
            "message": message,
            "pool_id": pool_id,
            "duration_ms": duration_ms,
        });
        let _ = self.app.emit("host://toast", payload);
    }

    async fn request_confirmation(&self, req: ConfirmRequest) -> bool {
        let (tx, rx) = oneshot::channel::<bool>();
        {
            let mut map = self.confirmations.lock().await;
            map.insert(req.request_id.clone(), tx);
        }
        let _ = self.app.emit(
            "host://confirm",
            serde_json::to_value(&req).unwrap_or(Value::Null),
        );
        match rx.await {
            Ok(answer) => answer,
            Err(_) => req.default.unwrap_or(false),
        }
    }

    async fn request_interactive(&self, req: InteractiveRequest) -> Value {
        let (tx, rx) = oneshot::channel::<Value>();
        {
            let mut map = self.interactives.lock().await;
            map.insert(req.request_id.clone(), tx);
        }
        let _ = self.app.emit(
            "host://interactive",
            serde_json::to_value(&req).unwrap_or(Value::Null),
        );
        match rx.await {
            Ok(v) => v,
            Err(_) => req.default.unwrap_or(Value::Null),
        }
    }
}

// ─── HostTools ---------------------------------------------------------------

/// Desktop host-tools injector. Carries every dependency the platform tools
/// need so the kernel can drive registration entirely through the
/// [`HostTools`] trait:
///
/// ```ignore
/// let host = DesktopHost::from_state(app.clone(), &state);
/// let mut handle = pisci_kernel::agent::tool::new_tool_registry_handle();
/// host.host_tools().register(&mut handle);
/// let registry = handle.into_registry().unwrap();
/// ```
///
/// Scene-aware callers that want per-call overrides (a custom
/// `builtin_tool_enabled` map, an alternate `skill_loader`, …) build a
/// fresh `DesktopHostTools` with the desired fields and call
/// [`DesktopHostTools::build_registry`] — the one-shot helper that runs
/// `.register()` into a fresh handle and extracts the populated
/// [`ToolRegistry`].
#[derive(Clone, Default)]
pub struct DesktopHostTools {
    pub browser: Option<SharedBrowserManager>,
    pub db: Option<Arc<Mutex<Database>>>,
    pub settings: Option<Arc<Mutex<Settings>>>,
    pub app_handle: Option<AppHandle>,
    pub app_data_dir: Option<PathBuf>,
    pub skill_loader: Option<Arc<Mutex<SkillLoader>>>,
    pub builtin_tool_enabled: Option<HashMap<String, bool>>,
    pub user_tools_dir: Option<PathBuf>,
}

impl DesktopHostTools {
    fn is_enabled(&self, name: &str) -> bool {
        self.builtin_tool_enabled
            .as_ref()
            .and_then(|m| m.get(name).copied())
            .unwrap_or(true)
    }

    fn neutral_config(&self) -> NeutralToolsConfig {
        NeutralToolsConfig {
            db: self.db.clone(),
            settings: self.settings.clone(),
            builtin_tool_enabled: self.builtin_tool_enabled.clone(),
            user_tools_dir: self.user_tools_dir.clone(),
        }
    }

    /// One-shot helper: build a fresh `ToolRegistryHandle`, run `register`
    /// on it, and extract the populated [`ToolRegistry`]. This is the
    /// canonical way to materialise a registry from scene / koi / fish /
    /// scheduler call sites that previously relied on the old
    /// `tools::build_registry` free function.
    pub fn build_registry(self) -> ToolRegistry {
        let mut handle: ToolRegistryHandle = new_tool_registry_handle();
        self.register(&mut handle);
        match handle.into_inner::<ToolRegistry>() {
            Ok(reg) => reg,
            Err(_) => unreachable!("new_tool_registry_handle must yield a ToolRegistry"),
        }
    }
}

impl HostTools for DesktopHostTools {
    fn register(&self, handle: &mut ToolRegistryHandle) {
        // 1) Neutral tools shared with the CLI host.
        pisci_kernel::tools::register_neutral_tools(handle, &self.neutral_config());

        // 2) Platform-specific desktop tools.
        let Some(registry) = handle.as_registry_mut() else {
            tracing::error!(
                "DesktopHostTools::register: handle is not a ToolRegistry ({})",
                handle.type_name()
            );
            return;
        };

        if self.is_enabled("browser") {
            if let Some(ref browser) = self.browser {
                registry.register(Box::new(browser::BrowserTool::new(browser.clone())));
            }
        }
        if self.is_enabled("plan_todo") {
            if let Some(ref app) = self.app_handle {
                registry.register(Box::new(plan_todo::PlanTodoTool { app: app.clone() }));
            }
        }
        if self.is_enabled("call_fish") {
            if let Some(ref app) = self.app_handle {
                registry.register(Box::new(call_fish::CallFishTool { app: app.clone() }));
            }
        }
        if self.is_enabled("call_koi") {
            if let Some(ref app) = self.app_handle {
                registry.register(Box::new(call_koi::CallKoiTool {
                    app: app.clone(),
                    caller_koi_id: None,
                    depth: 0,
                    managed_externally: false,
                    notification_rx: std::sync::Mutex::new(None),
                    await_completion: false,
                }));
            }
        }
        if self.is_enabled("chat_ui") {
            if let Some(ref app) = self.app_handle {
                registry.register(Box::new(chat_ui::ChatUiTool { app: app.clone() }));
            }
        }
        if self.is_enabled("pool_org") {
            if let (Some(ref app), Some(ref db)) = (&self.app_handle, &self.db) {
                registry.register(Box::new(pool_org::PoolOrgTool {
                    app: app.clone(),
                    db: db.clone(),
                }));
            }
        }
        if self.is_enabled("pool_chat") {
            if let (Some(ref app), Some(ref db)) = (&self.app_handle, &self.db) {
                registry.register(Box::new(pool_chat::PoolChatTool {
                    app: app.clone(),
                    db: db.clone(),
                    sender_id: "pisci".to_string(),
                }));
            }
        }
        if self.is_enabled("app_control") {
            if let (Some(ref db), Some(ref settings), Some(ref dir)) =
                (&self.db, &self.settings, &self.app_data_dir)
            {
                registry.register(Box::new(app_control::AppControlTool {
                    db: db.clone(),
                    settings: settings.clone(),
                    app_data_dir: dir.clone(),
                    app_handle: self.app_handle.clone(),
                }));
            }
        }
        if self.is_enabled("skill_list") {
            if let Some(ref loader) = self.skill_loader {
                registry.register(Box::new(skill_list::SkillListTool {
                    loader: loader.clone(),
                }));
            }
        }

        // 3) Windows-only tools.
        #[cfg(target_os = "windows")]
        {
            if self.is_enabled("powershell_query") {
                registry.register(Box::new(powershell::PowerShellTool));
            }
            if self.is_enabled("wmi") {
                registry.register(Box::new(wmi_tool::WmiTool));
            }
            if self.is_enabled("office") {
                registry.register(Box::new(office::OfficeTool));
            }
            if self.is_enabled("uia") {
                registry.register(Box::new(uia::UiaTool));
            }
            if self.is_enabled("screen_capture") {
                registry.register(Box::new(screen::ScreenTool));
            }
            if self.is_enabled("com") {
                registry.register(Box::new(com_tool::ComTool));
            }
            if self.is_enabled("com_invoke") {
                registry.register(Box::new(com_invoke::ComInvokeTool));
            }
        }
    }
}

// ─── SecretsStore ------------------------------------------------------------

#[derive(Clone)]
pub struct DesktopSecretsStore {
    settings: Arc<Mutex<Settings>>,
}

impl DesktopSecretsStore {
    pub fn new(settings: Arc<Mutex<Settings>>) -> Self {
        Self { settings }
    }
}

impl DesktopSecretsStore {
    fn read_field(s: &Settings, key: &str) -> Option<String> {
        match key {
            "anthropic_api_key" => Some(s.anthropic_api_key.clone()),
            "openai_api_key" => Some(s.openai_api_key.clone()),
            "deepseek_api_key" => Some(s.deepseek_api_key.clone()),
            "qwen_api_key" => Some(s.qwen_api_key.clone()),
            "minimax_api_key" => Some(s.minimax_api_key.clone()),
            "zhipu_api_key" => Some(s.zhipu_api_key.clone()),
            "kimi_api_key" => Some(s.kimi_api_key.clone()),
            _ => None,
        }
    }

    fn write_field(s: &mut Settings, key: &str, value: &str) -> anyhow::Result<()> {
        match key {
            "anthropic_api_key" => s.anthropic_api_key = value.to_string(),
            "openai_api_key" => s.openai_api_key = value.to_string(),
            "deepseek_api_key" => s.deepseek_api_key = value.to_string(),
            "qwen_api_key" => s.qwen_api_key = value.to_string(),
            "minimax_api_key" => s.minimax_api_key = value.to_string(),
            "zhipu_api_key" => s.zhipu_api_key = value.to_string(),
            "kimi_api_key" => s.kimi_api_key = value.to_string(),
            other => anyhow::bail!("unknown secret key: {other}"),
        }
        Ok(())
    }
}

impl SecretsStore for DesktopSecretsStore {
    fn get(&self, key: &str) -> Option<String> {
        let settings = self.settings.clone();
        let key = key.to_string();
        let handle = tokio::runtime::Handle::try_current().ok()?;
        tokio::task::block_in_place(|| {
            handle.block_on(async move {
                let s = settings.lock().await;
                Self::read_field(&s, &key).filter(|v| !v.is_empty())
            })
        })
    }

    fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let settings = self.settings.clone();
        let key = key.to_string();
        let value = value.to_string();
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|e| anyhow::anyhow!("no tokio runtime: {e}"))?;
        tokio::task::block_in_place(|| {
            handle.block_on(async move {
                let mut s = settings.lock().await;
                Self::write_field(&mut s, &key, &value)
            })
        })
    }
}

// ─── HostRuntime ------------------------------------------------------------

#[derive(Clone)]
pub struct DesktopHost {
    app: AppHandle,
    event_sink: Arc<DesktopEventSink>,
    notifier: Arc<DesktopNotifier>,
    tools: Arc<DesktopHostTools>,
    secrets: Arc<DesktopSecretsStore>,
}

impl DesktopHost {
    pub fn from_state(app: AppHandle, state: &AppState) -> Self {
        let event_sink = Arc::new(DesktopEventSink::new(app.clone()));
        let notifier = Arc::new(DesktopNotifier::new(
            app.clone(),
            state.confirmation_responses.clone(),
            state.interactive_responses.clone(),
        ));
        let app_data_dir = app
            .path()
            .app_data_dir()
            .ok()
            .or_else(|| Some(PathBuf::from(".pisci")));
        let tools = Arc::new(DesktopHostTools {
            browser: Some(state.browser.clone()),
            db: Some(state.db.clone()),
            settings: Some(state.settings.clone()),
            app_handle: Some(app.clone()),
            app_data_dir: app_data_dir.clone(),
            // Scene-aware callers (chat / scheduler / call_fish / call_koi)
            // build their own `DesktopHostTools` per request with the
            // right `skill_loader`, `builtin_tool_enabled`, and
            // `user_tools_dir`. The default host instance carries `None`
            // for all three — "no user tools, all builtins enabled, no
            // skill loader".
            skill_loader: None,
            builtin_tool_enabled: None,
            user_tools_dir: None,
        });
        let secrets = Arc::new(DesktopSecretsStore::new(state.settings.clone()));
        Self {
            app,
            event_sink,
            notifier,
            tools,
            secrets,
        }
    }
}

impl HostRuntime for DesktopHost {
    fn event_sink(&self) -> Arc<dyn EventSink> {
        self.event_sink.clone()
    }

    fn notifier(&self) -> Arc<dyn Notifier> {
        self.notifier.clone()
    }

    fn host_tools(&self) -> Arc<dyn HostTools> {
        self.tools.clone()
    }

    fn secrets(&self) -> Arc<dyn SecretsStore> {
        self.secrets.clone()
    }

    fn app_data_dir(&self) -> PathBuf {
        self.app
            .path()
            .app_data_dir()
            .unwrap_or_else(|_| PathBuf::from(".pisci"))
    }
}
