//! Shared helpers for running a single pisci-mode agent turn from any
//! headless host.
//!
//! Both `openpisci-headless` (pure CLI) and `openpisci` (desktop CLI
//! fallback, for callers that want to bypass the Tauri boot when the
//! request is pisci-only) dispatch into [`run_pisci_once`] so the two
//! entry points are guaranteed to stream through the exact same kernel
//! code path — same tool registry, same event sink wiring, same timeout
//! semantics.
//!
//! Pool mode is intentionally *not* supported here; it still requires a
//! Tauri-backed `AppState` for koi coordination and stays in the desktop
//! crate. Callers must reject pool requests before invoking this helper.

use std::path::PathBuf;

use pisci_core::host::{
    HeadlessCliMode, HeadlessCliRequest, HeadlessCliResponse, HostRuntime, ToolRegistryHandle,
};
use pisci_kernel::agent::tool::{new_tool_registry_handle, ToolRegistryHandleExt};
use pisci_kernel::headless::{self, HeadlessDeps};
use pisci_kernel::tools::NeutralToolsConfig;

use crate::host::CliHost;

/// Resolve the app data directory for a headless run. Honours the
/// per-request `config_dir` override first, then the
/// `OPENPISCI_CONFIG_DIR` env var, then the OS default (`dirs::data_dir`).
pub fn resolve_app_data_dir(request: Option<&HeadlessCliRequest>) -> PathBuf {
    if let Some(req) = request {
        if let Some(dir) = req.app_data_dir_override() {
            return dir;
        }
    }
    std::env::var("OPENPISCI_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("pisci")
        })
}

/// Execute a single pisci-mode turn end-to-end.
///
/// This is the canonical `pisci-once` pipeline:
///
/// 1. Reject pool mode (desktop-only).
/// 2. Open kernel state (`pisci.db` + `config.json`) under
///    `resolve_app_data_dir`.
/// 3. Register the full neutral tool set; let the [`CliHost`] layer any
///    additional host tools (a no-op today).
/// 4. Build [`HeadlessDeps`] with the `CliEventSink` — stdout NDJSON.
/// 5. Drive [`pisci_kernel::headless::run_pisci_turn`] on a fresh
///    multi-threaded tokio runtime and return the response.
///
/// Errors are flattened into `String` so the binary entry points can
/// `eprintln!` them directly without extra wrapping.
pub fn run_pisci_once(request: HeadlessCliRequest) -> Result<HeadlessCliResponse, String> {
    if matches!(request.mode, HeadlessCliMode::Pool) {
        return Err(
            "openpisci-headless does not support pool mode (needs desktop \
             AppState). Use the desktop `openpisci` binary instead."
                .to_string(),
        );
    }

    let app_data_dir = resolve_app_data_dir(Some(&request));
    let host = CliHost::new(app_data_dir.clone());

    let (db, settings) = headless::open_kernel_state(&app_data_dir)
        .map_err(|e| format!("Failed to initialise kernel state: {e}"))?;

    let mut handle: ToolRegistryHandle = new_tool_registry_handle();
    let neutral_cfg = NeutralToolsConfig {
        db: Some(db.clone()),
        settings: Some(settings.clone()),
        builtin_tool_enabled: None,
        user_tools_dir: Some(app_data_dir.join("user_tools")),
    };
    pisci_kernel::tools::register_neutral_tools(&mut handle, &neutral_cfg);
    host.host_tools().register(&mut handle);
    let registry = handle
        .into_registry()
        .map_err(|_| "internal: registry handle type mismatch".to_string())?;

    let deps = HeadlessDeps::new(db, settings, registry, host.event_sink());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to start tokio runtime: {e}"))?;

    runtime
        .block_on(headless::run_pisci_turn(request, deps))
        .map_err(|e| format!("run_pisci_turn failed: {e}"))
}
