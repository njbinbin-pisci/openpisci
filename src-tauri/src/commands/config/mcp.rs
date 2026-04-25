/// MCP (Model Context Protocol) server management commands.
use crate::store::AppState;
use crate::store::Settings;
use crate::tools::mcp::{McpClient, McpServerConfig, McpToolInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tauri::State;
use tracing::info;

/// Summary of a connected MCP tool (for the test connection response)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTestResult {
    pub success: bool,
    pub tools: Vec<McpToolInfo>,
    pub error: Option<String>,
}

/// Resolves `${settings:<field>}` placeholders inside MCP server configs.
///
/// Only a small whitelist of IM application credentials is exposed. This
/// keeps the placeholder mechanism scoped to enterprise-capability MCPs
/// that legitimately share credentials with IM channels.
struct SettingsPlaceholderResolver {
    values: HashMap<&'static str, String>,
}

impl SettingsPlaceholderResolver {
    fn from_settings(settings: &Settings) -> Self {
        let mut values = HashMap::new();
        values.insert("wecom_bot_id", settings.wecom_bot_id.clone());
        values.insert("wecom_bot_secret", settings.wecom_bot_secret.clone());
        values.insert("feishu_app_id", settings.feishu_app_id.clone());
        values.insert("feishu_app_secret", settings.feishu_app_secret.clone());
        values.insert("feishu_domain", settings.feishu_domain.clone());
        values.insert("dingtalk_app_key", settings.dingtalk_app_key.clone());
        values.insert("dingtalk_app_secret", settings.dingtalk_app_secret.clone());
        values.insert("dingtalk_corp_id", settings.dingtalk_corp_id.clone());
        values.insert("dingtalk_agent_id", settings.dingtalk_agent_id.clone());
        values.insert("dingtalk_mcp_url", settings.dingtalk_mcp_url.clone());
        Self { values }
    }

    fn expand(&self, raw: &str, server_name: &str, field_name: &str) -> String {
        const PREFIX: &str = "${settings:";
        if !raw.contains(PREFIX) {
            return raw.to_string();
        }

        let mut out = String::with_capacity(raw.len());
        let mut rest = raw;
        while let Some(start) = rest.find(PREFIX) {
            out.push_str(&rest[..start]);
            let after_prefix = &rest[start + PREFIX.len()..];
            if let Some(end) = after_prefix.find('}') {
                let field = after_prefix[..end].trim();
                match self.values.get(field) {
                    Some(value) if !value.is_empty() => out.push_str(value),
                    Some(_) => tracing::warn!(
                        "MCP server '{}' field '{}' references settings field '{}' but it is empty",
                        server_name,
                        field_name,
                        field
                    ),
                    None => {
                        tracing::warn!(
                            "MCP server '{}' field '{}' references unknown / non-whitelisted settings field '{}'",
                            server_name,
                            field_name,
                            field
                        );
                        out.push_str(&rest[start..start + PREFIX.len() + end + 1]);
                    }
                }
                rest = &after_prefix[end + 1..];
            } else {
                out.push_str(&rest[start..]);
                rest = "";
                break;
            }
        }
        out.push_str(rest);
        out
    }
}

pub fn resolve_settings_placeholders_in_mcp_config(
    config: &McpServerConfig,
    settings: &Settings,
) -> McpServerConfig {
    let resolver = SettingsPlaceholderResolver::from_settings(settings);
    let mut resolved = config.clone();
    resolved.command = resolver.expand(&resolved.command, &resolved.name, "command");
    resolved.url = resolver.expand(&resolved.url, &resolved.name, "url");
    resolved.args = resolved
        .args
        .iter()
        .map(|arg| resolver.expand(arg, &resolved.name, "args"))
        .collect();
    resolved.env = resolved
        .env
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                resolver.expand(value, &resolved.name, &format!("env.{key}")),
            )
        })
        .collect();
    resolved
}

pub async fn test_mcp_server_config(config: McpServerConfig) -> McpTestResult {
    let client = McpClient::new(config);
    match client.list_tools().await {
        Ok(tools) => McpTestResult {
            success: true,
            tools,
            error: None,
        },
        Err(e) => McpTestResult {
            success: false,
            tools: vec![],
            error: Some(e.to_string()),
        },
    }
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
pub async fn test_mcp_server(
    state: State<'_, AppState>,
    config: McpServerConfig,
) -> Result<McpTestResult, String> {
    info!(
        "Testing MCP server '{}' (transport={})",
        config.name, config.transport
    );
    let settings = state.settings.lock().await;
    let config = resolve_settings_placeholders_in_mcp_config(&config, &settings);
    drop(settings);
    Ok(test_mcp_server_config(config).await)
}

#[cfg(test)]
mod tests {
    use super::resolve_settings_placeholders_in_mcp_config;
    use crate::store::Settings;
    use crate::tools::mcp::McpServerConfig;
    use std::collections::HashMap;

    #[test]
    fn resolves_settings_placeholders_in_args_and_env() {
        let mut settings = Settings::default();
        settings.feishu_app_id = "cli_123".into();
        settings.feishu_app_secret = "secret_456".into();

        let config = McpServerConfig {
            name: "feishu-enterprise".into(),
            transport: "stdio".into(),
            command: "npx".into(),
            args: vec![
                "-a".into(),
                "${settings:feishu_app_id}".into(),
                "-s=${settings:feishu_app_secret}".into(),
            ],
            url: String::new(),
            env: HashMap::from([("FEISHU_APP_ID".into(), "${settings:feishu_app_id}".into())]),
            enabled: true,
        };

        let resolved = resolve_settings_placeholders_in_mcp_config(&config, &settings);
        assert_eq!(resolved.args[1], "cli_123");
        assert_eq!(resolved.args[2], "-s=secret_456");
        assert_eq!(resolved.env["FEISHU_APP_ID"], "cli_123");
    }

    #[test]
    fn leaves_unknown_placeholders_untouched() {
        let settings = Settings::default();
        let config = McpServerConfig {
            name: "bad".into(),
            transport: "stdio".into(),
            command: "npx".into(),
            args: vec!["${settings:anthropic_api_key}".into()],
            url: String::new(),
            env: HashMap::new(),
            enabled: true,
        };

        let resolved = resolve_settings_placeholders_in_mcp_config(&config, &settings);
        assert_eq!(resolved.args[0], "${settings:anthropic_api_key}");
    }
}
