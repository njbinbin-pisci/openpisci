pub mod app_control;
pub mod file_read;
pub mod file_write;
pub mod file_search;
pub mod file_list;
pub mod code_run;
pub mod file_diff;
pub mod process_control;
pub mod shell;
pub mod web_search;
pub mod powershell;
pub mod wmi_tool;
pub mod office;
pub mod browser;
pub mod dpi;
pub mod email;
pub mod memory_tool;
pub mod user_tool;
pub mod plan_todo;
pub mod call_fish;
pub mod mcp;
pub mod skill_search;
pub mod ssh;
pub mod pdf;

#[cfg(target_os = "windows")]
pub mod elevate;

#[cfg(target_os = "windows")]
pub mod uia;
#[cfg(target_os = "windows")]
pub mod screen;
#[cfg(target_os = "windows")]
pub mod com_tool;
#[cfg(target_os = "windows")]
pub mod com_invoke;

use crate::agent::tool::ToolRegistry;
use crate::browser::SharedBrowserManager;
use crate::skills::loader::SkillLoader;
use crate::store::{Database, Settings};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::AppHandle;
use tokio::sync::Mutex;

/// Build the default tool registry with all enabled tools.
/// Pass the shared browser manager so BrowserTool can reuse the same Chrome instance.
/// Also loads any user-installed tools from `user_tools_dir`.
pub fn build_registry(
    browser: SharedBrowserManager,
    user_tools_dir: Option<&std::path::Path>,
    db: Option<Arc<Mutex<Database>>>,
    builtin_tool_enabled: Option<&HashMap<String, bool>>,
    app_handle: Option<AppHandle>,
    settings: Option<Arc<Mutex<Settings>>>,
    app_data_dir: Option<PathBuf>,
    skill_loader: Option<Arc<Mutex<SkillLoader>>>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    let is_enabled = |name: &str| -> bool {
        builtin_tool_enabled
            .and_then(|m| m.get(name).copied())
            .unwrap_or(true)
    };

    // Cross-platform tools
    if is_enabled("file_read") {
        registry.register(Box::new(file_read::FileReadTool));
    }
    if is_enabled("file_write") {
        registry.register(Box::new(file_write::FileWriteTool));
    }
    if is_enabled("file_edit") {
        registry.register(Box::new(file_write::FileEditTool));
    }
    if is_enabled("file_diff") {
        registry.register(Box::new(file_diff::FileDiffTool));
    }
    if is_enabled("code_run") {
        registry.register(Box::new(code_run::CodeRunTool));
    }
    if is_enabled("file_search") {
        registry.register(Box::new(file_search::FileSearchTool));
    }
    if is_enabled("file_list") {
        registry.register(Box::new(file_list::FileListTool));
    }
    if is_enabled("process_control") {
        registry.register(Box::new(process_control::ProcessControlTool));
    }
    if is_enabled("shell") {
        registry.register(Box::new(shell::ShellTool));
    }
    if is_enabled("web_search") {
        registry.register(Box::new(web_search::WebSearchTool));
    }
    if is_enabled("powershell_query") {
        registry.register(Box::new(powershell::PowerShellTool));
    }
    if is_enabled("wmi") {
        registry.register(Box::new(wmi_tool::WmiTool));
    }
    if is_enabled("office") {
        registry.register(Box::new(office::OfficeTool));
    }
    if is_enabled("email") {
        registry.register(Box::new(email::EmailTool));
    }
    if is_enabled("browser") {
        registry.register(Box::new(browser::BrowserTool::new(browser)));
    }

    // Memory tool — requires DB access
    if is_enabled("memory_store") {
        if let Some(ref db_arc) = db {
            registry.register(Box::new(memory_tool::MemoryStoreTool { db: db_arc.clone() }));
        }
    }

    if is_enabled("plan_todo") {
        if let Some(ref app) = app_handle {
            registry.register(Box::new(plan_todo::PlanTodoTool { app: app.clone() }));
        }
    }

    // call_fish tool — lets the main agent delegate sub-tasks to Fish agents
    if is_enabled("call_fish") {
        if let Some(ref app) = app_handle {
            registry.register(Box::new(call_fish::CallFishTool { app: app.clone() }));
        }
    }

    // app_control tool — manage scheduled tasks, settings, and skills via Agent
    if is_enabled("app_control") {
        if let (Some(ref db_arc), Some(ref settings_arc), Some(ref data_dir)) =
            (&db, &settings, &app_data_dir)
        {
            registry.register(Box::new(app_control::AppControlTool {
                db: db_arc.clone(),
                settings: settings_arc.clone(),
                app_data_dir: data_dir.clone(),
                app_handle: app_handle.clone(),
            }));
        }
    }

    // skill_search tool — lazy-load skill instructions on demand
    if is_enabled("skill_search") {
        if let Some(loader) = skill_loader {
            registry.register(Box::new(skill_search::SkillSearchTool { loader }));
        }
    }

    // SSH tool — connect to remote servers and execute commands
    if is_enabled("ssh") {
        let ssh_settings = settings.clone();
        registry.register(Box::new(ssh::SshTool::new(ssh_settings)));
    }

    // PDF read/write
    if is_enabled("pdf") {
        registry.register(Box::new(pdf::PdfTool));
    }

    // Windows-only tools
    #[cfg(target_os = "windows")]
    {
        if is_enabled("uia") {
            registry.register(Box::new(uia::UiaTool));
        }
        if is_enabled("screen_capture") {
            registry.register(Box::new(screen::ScreenTool));
        }
        if is_enabled("com") {
            registry.register(Box::new(com_tool::ComTool));
        }
        if is_enabled("com_invoke") {
            registry.register(Box::new(com_invoke::ComInvokeTool));
        }
    }

    // Dynamically loaded user tools
    if let Some(dir) = user_tools_dir {
        let user_tools = user_tool::load_user_tools(dir);
        tracing::info!("Loaded {} user tool(s) from {}", user_tools.len(), dir.display());
        for tool in user_tools {
            registry.register(Box::new(tool));
        }
    }

    registry
}

/// Load MCP tools from configured servers and register them into an existing registry.
/// This is async because MCP connections require network/process I/O.
pub async fn register_mcp_tools(
    registry: &mut ToolRegistry,
    mcp_servers: &[mcp::McpServerConfig],
) {
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
