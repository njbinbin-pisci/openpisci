pub mod file_read;
pub mod file_write;
pub mod shell;
pub mod web_search;
pub mod powershell;
pub mod wmi_tool;
pub mod office;
pub mod browser;

#[cfg(target_os = "windows")]
pub mod uia;
#[cfg(target_os = "windows")]
pub mod screen;
#[cfg(target_os = "windows")]
pub mod com_tool;

use crate::agent::tool::ToolRegistry;
use crate::browser::SharedBrowserManager;

/// Build the default tool registry with all enabled tools.
/// Pass the shared browser manager so BrowserTool can reuse the same Chrome instance.
pub fn build_registry(browser: SharedBrowserManager) -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    // Cross-platform tools
    registry.register(Box::new(file_read::FileReadTool));
    registry.register(Box::new(file_write::FileWriteTool));
    registry.register(Box::new(shell::ShellTool));
    registry.register(Box::new(web_search::WebSearchTool));
    registry.register(Box::new(powershell::PowerShellTool));
    registry.register(Box::new(wmi_tool::WmiTool));
    registry.register(Box::new(office::OfficeTool));
    registry.register(Box::new(browser::BrowserTool::new(browser)));

    // Windows-only tools
    #[cfg(target_os = "windows")]
    {
        registry.register(Box::new(uia::UiaTool));
        registry.register(Box::new(screen::ScreenTool));
        registry.register(Box::new(com_tool::ComTool));
    }

    registry
}
