pub mod file_read;
pub mod file_write;
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

#[cfg(target_os = "windows")]
pub mod uia;
#[cfg(target_os = "windows")]
pub mod screen;
#[cfg(target_os = "windows")]
pub mod com_tool;

use crate::agent::tool::ToolRegistry;
use crate::browser::SharedBrowserManager;
use crate::store::Database;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Build the default tool registry with all enabled tools.
/// Pass the shared browser manager so BrowserTool can reuse the same Chrome instance.
/// Also loads any user-installed tools from `user_tools_dir`.
pub fn build_registry(
    browser: SharedBrowserManager,
    user_tools_dir: Option<&std::path::Path>,
    db: Option<Arc<Mutex<Database>>>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    // Cross-platform tools
    registry.register(Box::new(file_read::FileReadTool));
    registry.register(Box::new(file_write::FileWriteTool));
    registry.register(Box::new(shell::ShellTool));
    registry.register(Box::new(web_search::WebSearchTool));
    registry.register(Box::new(powershell::PowerShellTool));
    registry.register(Box::new(wmi_tool::WmiTool));
    registry.register(Box::new(office::OfficeTool));
    registry.register(Box::new(email::EmailTool));
    registry.register(Box::new(browser::BrowserTool::new(browser)));

    // Memory tool — requires DB access
    if let Some(db_arc) = db {
        registry.register(Box::new(memory_tool::MemoryStoreTool { db: db_arc }));
    }

    // Windows-only tools
    #[cfg(target_os = "windows")]
    {
        registry.register(Box::new(uia::UiaTool));
        registry.register(Box::new(screen::ScreenTool));
        registry.register(Box::new(com_tool::ComTool));
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
