/// MCP (Model Context Protocol) server management commands.
use crate::store::AppState;
use crate::tools::mcp::{McpClient, McpServerConfig, McpToolInfo};
use serde::{Deserialize, Serialize};
use tauri::State;
use tracing::info;

/// Summary of a connected MCP tool (for the test connection response)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTestResult {
    pub success: bool,
    pub tools: Vec<McpToolInfo>,
    pub error: Option<String>,
}

/// Return the current list of configured MCP servers.
#[tauri::command]
pub async fn list_mcp_servers(state: State<'_, AppState>) -> Result<Vec<McpServerConfig>, String> {
    let settings = state.settings.lock().await;
    Ok(settings.mcp_servers.clone())
}

/// Save the full list of MCP server configurations (replaces existing list).
#[tauri::command]
pub async fn save_mcp_servers(
    state: State<'_, AppState>,
    servers: Vec<McpServerConfig>,
) -> Result<(), String> {
    info!("Saving {} MCP server(s)", servers.len());
    let mut settings = state.settings.lock().await;
    settings.mcp_servers = servers;
    settings.save().map_err(|e| e.to_string())
}

/// Test a single MCP server configuration by connecting and listing its tools.
#[tauri::command]
pub async fn test_mcp_server(config: McpServerConfig) -> Result<McpTestResult, String> {
    info!(
        "Testing MCP server '{}' (transport={})",
        config.name, config.transport
    );
    let client = McpClient::new(config);
    match client.list_tools().await {
        Ok(tools) => Ok(McpTestResult {
            success: true,
            tools,
            error: None,
        }),
        Err(e) => Ok(McpTestResult {
            success: false,
            tools: vec![],
            error: Some(e.to_string()),
        }),
    }
}
