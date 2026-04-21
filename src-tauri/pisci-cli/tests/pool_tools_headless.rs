//! Confirms that the `openpisci-headless` tool surface includes the pool
//! and plan tools after Phase 1.7.
//!
//! Rather than spawn the binary, we rebuild the same
//! `NeutralToolsConfig` the CLI runner feeds into
//! `register_neutral_tools` and inspect the resulting registry. This
//! guarantees a single `register_neutral_tools` invocation on the kernel
//! side ends up exposing `plan_todo`, `pool_org`, and `pool_chat` with
//! only `CliEventSink` / `PlanStore` plumbing — no Tauri AppState, no
//! real Koi runtime.

use std::path::PathBuf;
use std::sync::Arc;

use pisci_cli::host::CliHost;
use pisci_core::host::{HostRuntime, ToolRegistryHandle};
use pisci_kernel::agent::plan::new_plan_store;
use pisci_kernel::agent::tool::{new_tool_registry_handle, ToolRegistryHandleExt};
use pisci_kernel::store::{Database, Settings};
use pisci_kernel::tools::{register_neutral_tools, NeutralToolsConfig};
use tokio::sync::Mutex;

fn scratch_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("pisci-cli-pool-tools-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn build_handle() -> ToolRegistryHandle {
    let app_dir = scratch_dir();
    let db = Database::open_in_memory().expect("in-memory db");
    let settings = Settings::load(&app_dir.join("config.json")).expect("load settings");
    let host = CliHost::new(app_dir.clone());

    let cfg = NeutralToolsConfig {
        db: Some(Arc::new(Mutex::new(db))),
        settings: Some(Arc::new(Mutex::new(settings))),
        builtin_tool_enabled: None,
        user_tools_dir: None,
        event_sink: Some(host.event_sink()),
        plan_store: Some(new_plan_store()),
        pool_event_sink: Some(host.pool_event_sink()),
        subagent_runtime: None,
        coordinator_config: Default::default(),
    };
    let mut handle = new_tool_registry_handle();
    register_neutral_tools(&mut handle, &cfg);
    handle
}

fn registered_names(handle: &mut ToolRegistryHandle) -> Vec<String> {
    handle
        .as_registry_mut()
        .expect("registry handle")
        .all()
        .iter()
        .map(|t| t.name().to_string())
        .collect()
}

#[test]
fn pool_and_plan_tools_are_registered_with_full_cli_deps() {
    let mut handle = build_handle();
    let names = registered_names(&mut handle);
    for tool in ["pool_org", "pool_chat", "plan_todo"] {
        assert!(
            names.iter().any(|n| n == tool),
            "expected `{tool}` in CLI registry, got {names:?}"
        );
    }
}

#[test]
fn pool_tools_are_skipped_when_sink_missing() {
    // Mirror the old Phase-1.6 code path: if a host forgets to plumb
    // `pool_event_sink` the kernel must keep `pool_org` / `pool_chat`
    // out of the registry rather than register a broken tool that
    // would silently drop its events.
    let app_dir = scratch_dir();
    let db = Database::open_in_memory().expect("in-memory db");
    let settings = Settings::load(&app_dir.join("config.json")).expect("load settings");
    let cfg = NeutralToolsConfig {
        db: Some(Arc::new(Mutex::new(db))),
        settings: Some(Arc::new(Mutex::new(settings))),
        builtin_tool_enabled: None,
        user_tools_dir: None,
        event_sink: None,
        plan_store: None,
        pool_event_sink: None,
        subagent_runtime: None,
        coordinator_config: Default::default(),
    };
    let mut handle = new_tool_registry_handle();
    register_neutral_tools(&mut handle, &cfg);
    let names = registered_names(&mut handle);
    for forbidden in ["pool_org", "pool_chat", "plan_todo"] {
        assert!(
            !names.iter().any(|n| n == forbidden),
            "`{forbidden}` should stay unregistered without a sink; got {names:?}"
        );
    }
}
