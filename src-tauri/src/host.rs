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

use pisci_core::host::SubagentRuntime;
use pisci_core::host::{
    ConfirmRequest, EventSink, HostRuntime, HostTools, InteractiveRequest, Notifier, PoolEvent,
    PoolEventSink, SecretsStore, ToolRegistryHandle,
};
use pisci_kernel::agent::plan::PlanStore;
use pisci_kernel::agent::tool::{new_tool_registry_handle, ToolRegistry, ToolRegistryHandleExt};
use pisci_kernel::pool::coordinator::CoordinatorConfig;
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
use crate::tools::{app_control, browser, call_fish, call_koi, chat_ui, im_send, skill_list};

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

// ─── PoolEventSink -----------------------------------------------------------
//
// Translates kernel-level [`PoolEvent`]s into the Tauri event names the
// existing React UI already subscribes to (see
// `src/components/Pond/**` and `src/services/tauri.ts`). Every variant is
// additionally forwarded on the canonical `host://pool_event` channel so
// new consumers (e.g. a future unified reducer) can subscribe to the full
// typed stream without per-kind listeners.
//
// Event-name mapping (kept in sync with the frontend):
//   PoolCreated                         → `pool_session_created`
//   PoolUpdated / PoolPaused / Resumed  → `pool_session_updated`
//   PoolArchived                        → `pool_session_updated` (status=archived)
//   MessageAppended                     → `pool_message_{pool_id}`
//   TodoChanged                         → `koi_todo_updated`
//   KoiAssigned                         → `koi_status_changed` (status=assigned)
//   KoiStatusChanged                    → `koi_status_changed`
//   KoiStaleRecovered                   → `koi_status_changed` (status=idle) + recovered count
//   CoordinatorIdle / Completed / TimedOut → `pool_coordinator_*`
//   FishProgress                        → `fish_progress_{parent_session_id}`
/// Canonical channel every [`PoolEvent`] is also fanned out on, so
/// forward-looking consumers can subscribe once and dispatch on the
/// serialised `kind` tag instead of maintaining per-variant listeners.
pub const POOL_EVENT_CANONICAL_CHANNEL: &str = "host://pool_event";

/// Pure mapper: produce the list of `(tauri_event_name, payload)` pairs
/// a given [`PoolEvent`] must be emitted on. Extracted into a
/// standalone function so tests can assert the wire-format contract
/// without constructing a Tauri [`AppHandle`].
///
/// The canonical channel ([`POOL_EVENT_CANONICAL_CHANNEL`]) is NOT
/// included here; [`DesktopEventSink::emit_pool`] emits it separately
/// to keep this function focused on the variant-specific fan-out.
pub fn pool_event_envelopes(event: &PoolEvent) -> Vec<(String, Value)> {
    match event {
        PoolEvent::PoolCreated { pool } => vec![(
            "pool_session_created".to_string(),
            serde_json::to_value(pool).unwrap_or(Value::Null),
        )],
        PoolEvent::PoolUpdated { pool }
        | PoolEvent::PoolPaused { pool }
        | PoolEvent::PoolResumed { pool } => vec![(
            "pool_session_updated".to_string(),
            serde_json::to_value(pool).unwrap_or(Value::Null),
        )],
        PoolEvent::PoolArchived { pool_id } => vec![(
            "pool_session_updated".to_string(),
            json!({ "id": pool_id, "status": "archived" }),
        )],
        PoolEvent::MessageAppended { pool_id, message } => vec![(
            format!("pool_message_{}", pool_id),
            serde_json::to_value(message).unwrap_or(Value::Null),
        )],
        PoolEvent::TodoChanged {
            pool_id,
            action,
            todo,
        } => vec![(
            "koi_todo_updated".to_string(),
            json!({
                "id": todo.id,
                "pool_id": pool_id,
                "action": action,
                "todo": todo,
            }),
        )],
        PoolEvent::KoiAssigned {
            pool_id,
            koi_id,
            todo_id,
        } => vec![(
            "koi_status_changed".to_string(),
            json!({
                "id": koi_id,
                "pool_id": pool_id,
                "status": "assigned",
                "todo_id": todo_id,
            }),
        )],
        PoolEvent::KoiStatusChanged {
            pool_id,
            koi_id,
            status,
        } => vec![(
            "koi_status_changed".to_string(),
            json!({
                "id": koi_id,
                "pool_id": pool_id,
                "status": status,
            }),
        )],
        PoolEvent::KoiStaleRecovered {
            pool_id,
            koi_id,
            recovered_todo_count,
        } => vec![(
            "koi_status_changed".to_string(),
            json!({
                "id": koi_id,
                "pool_id": pool_id,
                "status": "idle",
                "recovered_todo_count": recovered_todo_count,
                "stale_recovered": true,
            }),
        )],
        PoolEvent::CoordinatorIdle { pool_id } => vec![(
            "pool_coordinator_idle".to_string(),
            json!({ "pool_id": pool_id }),
        )],
        PoolEvent::CoordinatorCompleted { pool_id, summary } => vec![(
            "pool_coordinator_completed".to_string(),
            json!({ "pool_id": pool_id, "summary": summary }),
        )],
        PoolEvent::CoordinatorTimedOut { pool_id, summary } => vec![(
            "pool_coordinator_timed_out".to_string(),
            json!({ "pool_id": pool_id, "summary": summary }),
        )],
        PoolEvent::FishProgress {
            parent_session_id,
            fish_id,
            stage,
            payload,
        } => vec![(
            format!("fish_progress_{}", parent_session_id),
            json!({
                "fish_id": fish_id,
                "stage": stage,
                "payload": payload,
            }),
        )],
    }
}

impl PoolEventSink for DesktopEventSink {
    fn emit_pool(&self, event: &PoolEvent) {
        let _ = self.app.emit(POOL_EVENT_CANONICAL_CHANNEL, event);
        for (name, payload) in pool_event_envelopes(event) {
            let _ = self.app.emit(&name, payload);
        }
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
    /// Shared agent event sink — the desktop reuses [`DesktopEventSink`]
    /// (from `AppState.host_event_sink`) so legacy session events and
    /// kernel-emitted events travel over the same `AppHandle`.
    pub event_sink: Option<Arc<dyn EventSink>>,
    /// Shared per-session plan state backing the kernel `plan_todo`
    /// tool. Populated from `AppState.plan_state`.
    pub plan_store: Option<PlanStore>,
    /// Outlet for kernel pool events. Reuses [`DesktopEventSink`].
    pub pool_event_sink: Option<Arc<dyn PoolEventSink>>,
    /// Host-supplied [`SubagentRuntime`]. The desktop wires a
    /// subprocess-backed runtime (spawns `openpisci-headless run
    /// --mode pisci`) so Koi turns execute out-of-process. `None`
    /// leaves `assign_koi` / `resume_todo` / `replace_todo` returning
    /// a clean "not available" error rather than silently dropping
    /// work.
    pub subagent_runtime: Option<Arc<dyn SubagentRuntime>>,
    /// Coordinator configuration (task timeout, worktree usage).
    pub coordinator_config: CoordinatorConfig,
    /// IM gateway shared by every tool that needs the `wecom` /
    /// `feishu` / `dingtalk` / … long-running connection.
    ///
    /// Layered architecture (see `Settings::feishu_app_id` doc, the
    /// runtime arrows in `分层企业能力架构.plan.md`):
    ///   * `app_control(notify_user, targets=[…])` fans toasts out to
    ///     IM targets through the same connection the channel maintains.
    ///   * `im_send_message` (registered below in [`HostTools::register`])
    ///     uses it to push raw outbound messages from the agent.
    ///   * MCP enterprise-capability subprocesses receive credentials
    ///     via env-var placeholders; they do *not* share this Arc.
    ///
    /// CLI / headless callers leave it `None`; the affected tools then
    /// degrade gracefully (UI-only notifications) or refuse to register
    /// at all (`im_send_message`).
    pub gateway: Option<Arc<crate::gateway::GatewayManager>>,
}

fn resolve_desktop_coordinator_config(app: Option<&AppHandle>) -> CoordinatorConfig {
    let mut cfg = CoordinatorConfig::default();
    if let Some(app) = app {
        if let Some(state) = app.try_state::<AppState>() {
            if let Ok(settings) = state.settings.try_lock() {
                if settings.koi_timeout_secs > 0 {
                    cfg.default_task_timeout_secs = settings.koi_timeout_secs;
                }
            }
        }
    }
    cfg
}

impl DesktopHostTools {
    fn is_enabled(&self, name: &str) -> bool {
        self.builtin_tool_enabled
            .as_ref()
            .and_then(|m| m.get(name).copied())
            .unwrap_or(true)
    }

    /// Auto-populate the kernel pool seams (`event_sink`,
    /// `plan_store`, `pool_event_sink`, `subagent_runtime`) from the
    /// host's [`AppHandle`] if they are not already set.
    ///
    /// Why the `subagent_runtime` fallback matters: scene-aware callers
    /// (see [`crate::commands::config::scene::build_registry_for_scene`])
    /// build a fresh `DesktopHostTools` per request using
    /// `..DesktopHostTools::default()`, which leaves
    /// `subagent_runtime` as `None`. Without this lazy-load, the neutral
    /// kernel tools (`pool_chat`, `pool_org`) would receive `None` and
    /// silently drop @mention fan-out — Pisci could @-mention Kois in
    /// `pool_chat` and they would stay idle forever because
    /// [`pisci_kernel::pool::services::send_pool_message`] short-circuits
    /// the coordinator call when `subagent` is `None`.
    ///
    /// [`DesktopHost::from_state`] still wires the runtime explicitly
    /// at process startup; that instance is preserved here (we only
    /// fill in when the caller left the slot empty).
    pub fn fill_pool_defaults(mut self) -> Self {
        let Some(app) = self.app_handle.as_ref() else {
            return self;
        };
        self.coordinator_config = resolve_desktop_coordinator_config(Some(app));
        // db is still required for plan-store lookup even though we no
        // longer need it for the legacy dispatcher.
        let _ = self.db.as_ref();
        let sink = Arc::new(DesktopEventSink::new(app.clone()));
        if self.event_sink.is_none() {
            let sink_dyn: Arc<dyn EventSink> = sink.clone();
            self.event_sink = Some(sink_dyn);
        }
        if self.pool_event_sink.is_none() {
            let sink_dyn: Arc<dyn PoolEventSink> = sink.clone();
            self.pool_event_sink = Some(sink_dyn);
        }
        if self.plan_store.is_none() {
            self.plan_store = app.try_state::<AppState>().map(|s| s.plan_state.clone());
        }
        if self.subagent_runtime.is_none() {
            let app_data_dir = self
                .app_data_dir
                .clone()
                .or_else(|| app.path().app_data_dir().ok())
                .or_else(|| Some(PathBuf::from(".pisci")));
            let headless_bin = resolve_headless_binary();
            let mut runtime = pisci_kernel::pool::SubprocessSubagentRuntime::new(headless_bin);
            if let Some(dir) = app_data_dir {
                runtime = runtime.with_app_data_dir(dir);
            }
            let runtime_dyn: Arc<dyn SubagentRuntime> = Arc::new(runtime);
            self.subagent_runtime = Some(runtime_dyn);
        }
        if self.gateway.is_none() {
            self.gateway = app.try_state::<AppState>().map(|s| s.gateway.clone());
        }
        self
    }

    fn neutral_config(&self) -> NeutralToolsConfig {
        NeutralToolsConfig {
            db: self.db.clone(),
            settings: self.settings.clone(),
            builtin_tool_enabled: self.builtin_tool_enabled.clone(),
            user_tools_dir: self.user_tools_dir.clone(),
            // Full kernel wiring: the desktop registers pool_org /
            // pool_chat / plan_todo through the neutral tool registry
            // using the shared event sink, plan store, pool event
            // sink, and subagent runtime.
            event_sink: self.event_sink.clone(),
            plan_store: self.plan_store.clone(),
            pool_event_sink: self.pool_event_sink.clone(),
            subagent_runtime: self.subagent_runtime.clone(),
            coordinator_config: self.coordinator_config.clone(),
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
        // `plan_todo`, `pool_org`, and `pool_chat` are registered by the
        // neutral kernel layer via `register_neutral_tools` above (driven
        // by `NeutralToolsConfig.plan_store / pool_event_sink / ...`).
        // We only keep the Tauri-coupled tools here.
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
        if self.is_enabled("app_control") {
            if let (Some(ref db), Some(ref settings), Some(ref dir)) =
                (&self.db, &self.settings, &self.app_data_dir)
            {
                registry.register(Box::new(app_control::AppControlTool {
                    db: db.clone(),
                    settings: settings.clone(),
                    app_data_dir: dir.clone(),
                    app_handle: self.app_handle.clone(),
                    gateway: self.gateway.clone(),
                }));
            }
        }
        // `im_send_message` lives in the *tool* layer of the layered IM
        // architecture: it lets the agent push outbound IM messages
        // through the same `GatewayManager` the inbound channel already
        // owns. Registered whenever a gateway is available; the
        // `builtin_tool_enabled` map can disable it per-deployment.
        if self.is_enabled("im_send_message") {
            if let Some(ref gateway) = self.gateway {
                registry.register(Box::new(im_send::ImSendMessageTool {
                    gateway: Some(gateway.clone()),
                    db: self.db.clone(),
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
        // Reuse `DesktopEventSink` for both `EventSink` and `PoolEventSink`
        // so legacy session events and kernel pool events share a single
        // `AppHandle` + atomic emit path.
        let event_sink_dyn: Arc<dyn EventSink> = event_sink.clone();
        let pool_event_sink_dyn: Arc<dyn PoolEventSink> = event_sink.clone();

        // Locate `openpisci-headless` for the kernel's subagent
        // runtime. Production builds ship the headless binary
        // alongside the desktop executable; dev runs can drop it
        // anywhere on PATH or set `PISCI_HEADLESS_BIN`. The runtime
        // itself tolerates a missing binary — `spawn_koi_turn` just
        // surfaces the filesystem error up to the caller.
        let headless_bin = resolve_headless_binary();
        let subprocess = pisci_kernel::pool::SubprocessSubagentRuntime::new(headless_bin);
        let subprocess = if let Some(ref dir) = app_data_dir {
            subprocess.with_app_data_dir(dir.clone())
        } else {
            subprocess
        };
        let subagent_runtime: Arc<dyn SubagentRuntime> = Arc::new(subprocess);

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
            event_sink: Some(event_sink_dyn),
            plan_store: Some(state.plan_state.clone()),
            pool_event_sink: Some(pool_event_sink_dyn),
            subagent_runtime: Some(subagent_runtime),
            coordinator_config: resolve_desktop_coordinator_config(Some(&app)),
            gateway: Some(state.gateway.clone()),
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

    fn pool_event_sink(&self) -> Arc<dyn PoolEventSink> {
        // `DesktopEventSink` implements both [`EventSink`] and
        // [`PoolEventSink`] — reuse the same instance so both legacy
        // session events and new pool events travel over the same
        // `AppHandle`.
        self.event_sink.clone()
    }

    fn subagent_runtime(&self) -> Option<Arc<dyn SubagentRuntime>> {
        self.tools.subagent_runtime.clone()
    }
}

/// Locate the `openpisci-headless` binary the kernel's
/// [`SubprocessSubagentRuntime`] should spawn.
///
/// Resolution order:
/// 1. `PISCI_HEADLESS_BIN` environment variable (absolute path).
/// 2. Sibling of the desktop executable
///    (`current_exe().parent().join("openpisci-headless[.exe]")`).
/// 3. Bare command name `openpisci-headless` — defers resolution to
///    `PATH` at spawn time.
fn resolve_headless_binary() -> PathBuf {
    if let Ok(raw) = std::env::var("PISCI_HEADLESS_BIN") {
        let raw = raw.trim();
        if !raw.is_empty() {
            return PathBuf::from(raw);
        }
    }
    let exe_name = if cfg!(windows) {
        "openpisci-headless.exe"
    } else {
        "openpisci-headless"
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(exe_name);
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from(exe_name)
}
