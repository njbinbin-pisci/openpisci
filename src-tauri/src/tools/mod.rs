pub mod file_read;
pub mod file_write;
pub mod shell;
pub mod web_search;

#[cfg(target_os = "windows")]
pub mod uia;
#[cfg(target_os = "windows")]
pub mod screen;

use crate::agent::tool::ToolRegistry;

/// Build the default tool registry with all enabled tools
pub fn build_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    registry.register(Box::new(file_read::FileReadTool));
    registry.register(Box::new(file_write::FileWriteTool));
    registry.register(Box::new(shell::ShellTool));
    registry.register(Box::new(web_search::WebSearchTool));

    #[cfg(target_os = "windows")]
    {
        registry.register(Box::new(uia::UiaTool));
        registry.register(Box::new(screen::ScreenTool));
    }

    registry
}
