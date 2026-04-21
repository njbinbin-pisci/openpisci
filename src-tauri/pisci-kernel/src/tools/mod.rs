//! Platform-neutral tools provided by the agent kernel.
//!
//! These tools depend only on `pisci-kernel` and `pisci-core`, and do not
//! require any UI (Tauri) or OS-specific crates. Hosts (desktop, cli) register
//! the ones they need into the `ToolRegistry` returned by
//! [`crate::agent::tool::ToolRegistry`].
//!
//! Platform-specific tools (UI Automation, screen capture, PowerShell,
//! WMI/COM, browser control, call_koi/call_fish/chat_ui, etc.) remain in the
//! host crates and are plugged in via the `HostTools` host trait.

pub mod code_run;
#[cfg(target_os = "windows")]
pub mod elevate;
pub mod email;
pub mod file_diff;
pub mod file_list;
pub mod file_read;
pub mod file_search;
pub mod file_write;
pub mod mcp;
pub mod memory_tool;
pub mod pdf;
pub mod process_control;
pub mod recall_tool;
pub mod shell;
pub mod ssh;
pub mod user_tool;
pub mod vision_context;
pub mod web_search;

use crate::agent::tool::{Tool, ToolRegistry, ToolRegistryHandleExt};
use crate::store::{Database, Settings};
use pisci_core::host::ToolRegistryHandle;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Configuration for [`register_neutral_tools`].
///
/// All kernel-neutral tools accept their dependencies through this struct so
/// both desktop and CLI hosts share the exact same registration path.
#[derive(Default, Clone)]
pub struct NeutralToolsConfig {
    /// Optional shared database (required for `memory_store` and
    /// `recall_tool_result`).
    pub db: Option<Arc<Mutex<Database>>>,
    /// Optional shared settings handle (required for `ssh`).
    pub settings: Option<Arc<Mutex<Settings>>>,
    /// Per-tool enable override from user settings. Any name missing from the
    /// map defaults to `true`.
    pub builtin_tool_enabled: Option<HashMap<String, bool>>,
    /// Directory holding user-authored tool definitions (JSON). `None`
    /// disables dynamic user-tool loading.
    pub user_tools_dir: Option<PathBuf>,
}

impl NeutralToolsConfig {
    fn is_enabled(&self, name: &str) -> bool {
        self.builtin_tool_enabled
            .as_ref()
            .and_then(|m| m.get(name).copied())
            .unwrap_or(true)
    }
}

/// Register every kernel-neutral tool into `handle` according to `cfg`.
///
/// Hosts typically call this from their [`pisci_core::host::HostTools::register`]
/// implementation before adding platform-specific tools.
pub fn register_neutral_tools(handle: &mut ToolRegistryHandle, cfg: &NeutralToolsConfig) {
    let Some(registry) = handle.as_registry_mut() else {
        tracing::error!(
            "register_neutral_tools: handle is not a ToolRegistry ({})",
            handle.type_name()
        );
        return;
    };
    register_neutral_into(registry, cfg);
}

/// Same as [`register_neutral_tools`] but for callers that already hold a
/// concrete [`ToolRegistry`] and want to skip the handle dance (scene
/// tests, in-process koi spawners, …). Both entry points share this
/// body so they never drift.
pub fn register_neutral_into(registry: &mut ToolRegistry, cfg: &NeutralToolsConfig) {
    // ── Pure file / exec / network helpers ──────────────────────────
    if cfg.is_enabled("file_read") {
        registry.register(Box::new(file_read::FileReadTool));
    }
    if cfg.is_enabled("file_write") {
        registry.register(Box::new(file_write::FileWriteTool));
    }
    if cfg.is_enabled("file_edit") {
        registry.register(Box::new(file_write::FileEditTool));
    }
    if cfg.is_enabled("file_diff") {
        registry.register(Box::new(file_diff::FileDiffTool));
    }
    if cfg.is_enabled("code_run") {
        registry.register(Box::new(code_run::CodeRunTool));
    }
    if cfg.is_enabled("file_search") {
        registry.register(Box::new(file_search::FileSearchTool));
    }
    if cfg.is_enabled("file_list") {
        registry.register(Box::new(file_list::FileListTool));
    }
    if cfg.is_enabled("process_control") {
        registry.register(Box::new(process_control::ProcessControlTool));
    }
    if cfg.is_enabled("shell") {
        registry.register(Box::new(shell::ShellTool));
    }
    if cfg.is_enabled("web_search") {
        registry.register(Box::new(web_search::WebSearchTool));
    }
    if cfg.is_enabled("email") {
        registry.register(Box::new(email::EmailTool));
    }

    // ── DB-backed tools ─────────────────────────────────────────────
    if cfg.is_enabled("memory_store") {
        if let Some(ref db) = cfg.db {
            registry.register(Box::new(memory_tool::MemoryStoreTool { db: db.clone() }));
        }
    }
    if cfg.is_enabled("recall_tool_result") {
        if let Some(ref db) = cfg.db {
            registry.register(Box::new(recall_tool::RecallToolResultTool {
                db: db.clone(),
            }));
        }
    }

    // ── Settings-backed tools ───────────────────────────────────────
    if cfg.is_enabled("ssh") {
        registry.register(Box::new(ssh::SshTool::new(cfg.settings.clone())));
    }

    // ── Stateless multimedia / document ─────────────────────────────
    if cfg.is_enabled("vision_context") {
        registry.register(Box::new(vision_context::VisionContextTool));
    }
    if cfg.is_enabled("pdf") {
        registry.register(Box::new(pdf::PdfTool));
    }

    // ── User-authored JSON tools ────────────────────────────────────
    if let Some(ref dir) = cfg.user_tools_dir {
        let user_tools = user_tool::load_user_tools(dir);
        tracing::info!(
            "Loaded {} user tool(s) from {}",
            user_tools.len(),
            dir.display()
        );
        for tool in user_tools {
            registry.register(Box::new(tool) as Box<dyn Tool>);
        }
    }
}

/// Load MCP tools from configured servers and register them into an existing
/// registry. Async because MCP connections require network / subprocess I/O.
pub async fn register_mcp_tools(registry: &mut ToolRegistry, mcp_servers: &[mcp::McpServerConfig]) {
    for server in mcp_servers {
        if !server.enabled {
            continue;
        }
        let tools = mcp::build_mcp_tools(server).await;
        for tool in tools {
            registry.register(Box::new(tool));
        }
    }
}
