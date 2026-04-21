//! Host traits — the contract between `pisci-kernel` (OS/UI-neutral runtime)
//! and the concrete hosts that embed it (Tauri desktop, `openpisci` CLI,
//! future server process, …).
//!
//! The kernel always consumes these traits behind `Arc<dyn Trait>` pointers
//! obtained from a [`HostRuntime`]. The desktop host implements them by
//! forwarding to Tauri events / windows; the CLI host implements them by
//! writing NDJSON to stdout and returning deterministic defaults for
//! interactive prompts.
//!
//! `pisci-core` remains dependency-light on purpose: only `chrono`, `serde`,
//! `serde_json` and `async-trait`. No tokio, no reqwest, no rusqlite. If a
//! future trait needs async, express it via `#[async_trait]`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

// -- EventSink -------------------------------------------------------------

/// Publishes agent events out of the kernel so that a host can surface them
/// to whatever UI (Tauri window, terminal, web socket) it maintains.
///
/// Every `emit_session` call corresponds to a one-off payload tied to a
/// single agent session. `emit_broadcast` is for cross-session events
/// (completion notifications, state changes in the global view).
pub trait EventSink: Send + Sync {
    fn emit_session(&self, session_id: &str, event: &str, payload: Value);
    fn emit_broadcast(&self, event: &str, payload: Value);
}

// -- Notifier --------------------------------------------------------------

/// Request shape for a yes/no confirmation prompt that the agent wants to
/// surface to the user before performing a risky action (delete file, run
/// destructive shell command, …).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfirmRequest {
    pub request_id: String,
    pub session_id: Option<String>,
    pub title: String,
    pub body: String,
    /// Optional tool name the confirmation is gating.
    pub tool: Option<String>,
    /// Preset response if no human is around (CLI host uses this).
    pub default: Option<bool>,
}

/// Rich interactive prompt (e.g. a form rendered in the desktop chat panel
/// that expects a JSON response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractiveRequest {
    pub request_id: String,
    pub session_id: Option<String>,
    pub kind: String,
    pub payload: Value,
    pub default: Option<Value>,
}

/// Surface user-visible toasts and wait for confirmation/interactive
/// responses. Async methods are only meaningful on desktop; the CLI host
/// returns instantly.
#[async_trait]
pub trait Notifier: Send + Sync {
    fn toast(&self, level: &str, message: &str, pool_id: Option<&str>, duration_ms: Option<u64>);

    async fn request_confirmation(&self, req: ConfirmRequest) -> bool;
    async fn request_interactive(&self, req: InteractiveRequest) -> Value;
}

// -- HostTools -------------------------------------------------------------

/// Opaque handle to the kernel's `ToolRegistry`, supplied to `HostTools::
/// register` so hosts can drop in platform-specific tools without the core
/// crate taking a dependency on the concrete registry type.
///
/// Because `pisci-core` cannot mention the kernel's concrete `ToolRegistry`
/// type, we keep the payload type-erased behind `Box<dyn Any>`. Hosts that
/// live inside the same process (always the case for us: desktop and CLI
/// link against the exact same kernel build) can recover the concrete type
/// through [`downcast_mut`](Self::downcast_mut) / [`downcast_ref`](Self::downcast_ref).
///
/// The ergonomic kernel-side helpers (`as_registry_mut`, `register_tool`,
/// …) live in `pisci-kernel::agent::tool::ToolRegistryHandleExt` so host
/// crates can drop the downcast entirely:
///
/// ```ignore
/// use pisci_kernel::agent::tool::ToolRegistryHandleExt;
///
/// impl HostTools for DesktopHostTools {
///     fn register(&self, handle: &mut ToolRegistryHandle) {
///         let reg = handle.as_registry_mut().expect("kernel registry");
///         reg.register(Box::new(MyDesktopTool::new()));
///     }
/// }
/// ```
pub struct ToolRegistryHandle {
    /// Type-erased pointer managed by the kernel; boxing keeps the ABI
    /// stable while we move real tool registration code in.
    pub inner: Box<dyn std::any::Any + Send + Sync>,
    /// Snapshot of the concrete `T` used to build this handle. We capture
    /// it at construction time because `dyn Any` erases the type name —
    /// handy for diagnostics and for cross-kernel version-mismatch
    /// error messages.
    type_name: &'static str,
}

impl ToolRegistryHandle {
    /// Construct a handle from any type-erased value. Intended for internal
    /// use by the kernel when it hands the registry to a host.
    pub fn new<T: std::any::Any + Send + Sync>(value: T) -> Self {
        Self {
            inner: Box::new(value),
            type_name: std::any::type_name::<T>(),
        }
    }

    /// Mutable downcast to the kernel's concrete registry type. Returns
    /// `None` if `T` does not match the stored payload — that should never
    /// happen in-process but failing soft makes wiring bugs easier to debug.
    pub fn downcast_mut<T: std::any::Any>(&mut self) -> Option<&mut T> {
        self.inner.downcast_mut::<T>()
    }

    /// Shared downcast for read-only access (tool-list inspection,
    /// diagnostics, capability reporting).
    pub fn downcast_ref<T: std::any::Any>(&self) -> Option<&T> {
        self.inner.downcast_ref::<T>()
    }

    /// Consume the handle and recover the concrete payload. On type
    /// mismatch the handle is returned untouched in `Err` so the caller
    /// can try a different type or wrap it again.
    pub fn into_inner<T: std::any::Any + Send + Sync>(self) -> Result<T, Self> {
        let type_name = self.type_name;
        match self.inner.downcast::<T>() {
            Ok(boxed) => Ok(*boxed),
            Err(inner) => Err(Self { inner, type_name }),
        }
    }

    /// Typed scoped mutation. Folds the `downcast_mut + option-map` dance
    /// into one call so host adapters read linearly. Returns `None` only
    /// when the payload type does not match.
    pub fn with_mut<T: std::any::Any, R>(&mut self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        self.inner.downcast_mut::<T>().map(f)
    }

    /// Name of the concrete type this handle was built with. Captured at
    /// construction time so we still have it even after the payload has
    /// been downcast behind `dyn Any`.
    pub fn type_name(&self) -> &'static str {
        self.type_name
    }
}

/// Injection point for platform-specific tools (browser / UIA / screen /
/// COM / PowerShell / WMI / IM gateways). The desktop host attaches its
/// tool implementations inside `register`; the CLI host does nothing.
pub trait HostTools: Send + Sync {
    fn register(&self, registry: &mut ToolRegistryHandle);
}

// -- SecretsStore ----------------------------------------------------------

/// Read/write access to host-managed secrets (API keys, OAuth tokens). The
/// desktop host encrypts them at rest via `chacha20poly1305`; the CLI host
/// backs onto environment variables.
pub trait SecretsStore: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, value: &str) -> anyhow::Result<()>;
}

// -- HostRuntime -----------------------------------------------------------

/// Aggregate host interface. The kernel only ever borrows from this;
/// never stores host-specific types directly.
pub trait HostRuntime: Send + Sync {
    fn event_sink(&self) -> Arc<dyn EventSink>;
    fn notifier(&self) -> Arc<dyn Notifier>;
    fn host_tools(&self) -> Arc<dyn HostTools>;
    fn secrets(&self) -> Arc<dyn SecretsStore>;
    fn app_data_dir(&self) -> PathBuf;
}

// -- Shared headless schema ------------------------------------------------
//
// The CLI request / response and context-toggles schema lives in `pisci-core`
// so that hosts (pisci-desktop, pisci-cli) and external consumers (python
// benchmark scripts) share a single canonical shape.

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HeadlessCliMode {
    #[default]
    Pisci,
    Pool,
}

impl HeadlessCliMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pisci => "pisci",
            Self::Pool => "pool",
        }
    }
}

/// Fine-grained knobs for context assembly. The kernel reads these when a
/// headless CLI run requests ablation-style behaviour from bench_swe_lite.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HeadlessContextToggles {
    #[serde(default)]
    pub disable_memory_context: bool,
    #[serde(default)]
    pub disable_task_state_context: bool,
    #[serde(default)]
    pub disable_pool_context: bool,
    #[serde(default)]
    pub disable_project_instructions: bool,
    #[serde(default)]
    pub disable_rolling_summary: bool,
    #[serde(default)]
    pub disable_state_frame: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HeadlessCliRequest {
    pub prompt: String,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub mode: HeadlessCliMode,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub session_title: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub config_dir: Option<String>,
    #[serde(default)]
    pub pool_id: Option<String>,
    #[serde(default)]
    pub pool_name: Option<String>,
    #[serde(default)]
    pub pool_size: Option<u32>,
    #[serde(default)]
    pub koi_ids: Vec<String>,
    #[serde(default)]
    pub task_timeout_secs: Option<u32>,
    #[serde(default)]
    pub wait_for_completion: bool,
    #[serde(default)]
    pub wait_timeout_secs: Option<u64>,
    #[serde(default)]
    pub extra_system_context: Option<String>,
    #[serde(default)]
    pub context_toggles: HeadlessContextToggles,
    #[serde(default)]
    pub output: Option<String>,
}

impl HeadlessCliRequest {
    pub fn app_data_dir_override(&self) -> Option<PathBuf> {
        self.config_dir
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisabledToolInfo {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PoolWaitSummary {
    pub completed: bool,
    pub timed_out: bool,
    pub active_todos: u32,
    pub done_todos: u32,
    pub cancelled_todos: u32,
    pub blocked_todos: u32,
    pub latest_messages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadlessCliResponse {
    pub ok: bool,
    pub mode: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_id: Option<String>,
    pub response_text: String,
    pub disabled_tools: Vec<DisabledToolInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_wait: Option<PoolWaitSummary>,
}

#[cfg(test)]
mod tool_registry_handle_tests {
    use super::ToolRegistryHandle;

    // A stand-in registry to avoid a dependency back on the kernel.
    #[derive(Default)]
    struct FakeRegistry {
        names: Vec<String>,
    }

    #[test]
    fn downcast_mut_and_ref_roundtrip() {
        let mut h = ToolRegistryHandle::new(FakeRegistry::default());
        h.downcast_mut::<FakeRegistry>()
            .expect("downcast_mut")
            .names
            .push("shell".into());
        let r = h.downcast_ref::<FakeRegistry>().expect("downcast_ref");
        assert_eq!(r.names, vec!["shell".to_string()]);
    }

    #[test]
    fn with_mut_folds_downcast_and_closure() {
        let mut h = ToolRegistryHandle::new(FakeRegistry::default());
        let pushed = h
            .with_mut::<FakeRegistry, _>(|r| {
                r.names.push("file_read".into());
                r.names.len()
            })
            .expect("type matches");
        assert_eq!(pushed, 1);
    }

    #[test]
    fn with_mut_returns_none_on_type_mismatch() {
        let mut h = ToolRegistryHandle::new(FakeRegistry::default());
        // Wrong type — should not panic, just return None.
        let r: Option<()> = h.with_mut::<String, _>(|_| ());
        assert!(r.is_none());
    }

    #[test]
    fn into_inner_recovers_value_or_returns_handle() {
        let h = ToolRegistryHandle::new(FakeRegistry {
            names: vec!["shell".into()],
        });
        let recovered = h.into_inner::<FakeRegistry>().ok().expect("match");
        assert_eq!(recovered.names, vec!["shell".to_string()]);

        let h2 = ToolRegistryHandle::new(42u32);
        let err = h2.into_inner::<FakeRegistry>();
        assert!(err.is_err(), "should return Err(self) on mismatch");
        // And we can still try a different type on the returned handle.
        let still_u32: u32 = err.err().unwrap().into_inner::<u32>().ok().unwrap();
        assert_eq!(still_u32, 42);
    }

    #[test]
    fn type_name_reports_inner_type() {
        let h = ToolRegistryHandle::new(FakeRegistry::default());
        assert!(h.type_name().contains("FakeRegistry"));
    }
}
