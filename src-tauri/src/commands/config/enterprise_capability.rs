//! Enterprise capability setup commands.
//!
//! The UI entry point lives next to each IM channel, but runtime capability
//! setup follows the official integration shape of each platform.

use crate::commands::config::mcp::{
    resolve_settings_placeholders_in_mcp_config, test_mcp_server_config, McpTestResult,
};
use crate::store::{AppState, Settings};
use crate::tools::mcp::{McpServerConfig, McpToolInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Stdio;
use tauri::State;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

const FEISHU_PLATFORM: &str = "feishu";
const WECOM_PLATFORM: &str = "wecom";
const DINGTALK_PLATFORM: &str = "dingtalk";
const FEISHU_ENTERPRISE_SERVER: &str = "feishu-enterprise";
const WECOM_ENTERPRISE_SERVER: &str = "wecom-cli";
const DINGTALK_ENTERPRISE_SERVER: &str = "dingtalk-enterprise";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnterpriseCapabilityTemplate {
    pub platform: String,
    pub title: String,
    pub description: String,
    pub supported: bool,
    pub mcp_server_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnterpriseCapabilityStatus {
    pub platform: String,
    pub supported: bool,
    pub configured: bool,
    pub enabled: bool,
    pub mcp_configured: bool,
    pub mcp_enabled: bool,
    pub mcp_server_name: String,
    pub missing_credentials: Vec<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnterpriseCapabilityTestResult {
    pub status: EnterpriseCapabilityStatus,
    pub success: bool,
    pub tools: Vec<McpToolInfo>,
    pub error: Option<String>,
    pub diagnostics: Vec<String>,
}

#[tauri::command]
pub async fn list_enterprise_capability_templates(
) -> Result<Vec<EnterpriseCapabilityTemplate>, String> {
    Ok(vec![
        feishu_template(),
        wecom_template(),
        dingtalk_template(),
    ])
}

#[tauri::command]
pub async fn get_enterprise_capability_status(
    state: State<'_, AppState>,
    platform: String,
) -> Result<EnterpriseCapabilityStatus, String> {
    let settings = state.settings.lock().await;
    build_status(&settings, &platform)
}

#[tauri::command]
pub async fn enable_enterprise_capability(
    state: State<'_, AppState>,
    platform: String,
) -> Result<EnterpriseCapabilityStatus, String> {
    let mut settings = state.settings.lock().await;
    ensure_supported(&platform)?;
    let status = build_status(&settings, &platform)?;
    if !status.configured {
        return Err(format!(
            "Cannot enable {} enterprise capability: {}",
            platform, status.message
        ));
    }

    match parse_platform(&platform)? {
        EnterprisePlatform::Feishu => {
            let server = build_feishu_mcp_server(&settings);
            upsert_mcp_server(&mut settings.mcp_servers, server);
            settings.save().map_err(|e| e.to_string())?;
            build_status(&settings, &platform)
        }
        EnterprisePlatform::DingTalk => {
            let server = build_dingtalk_mcp_server(&settings);
            upsert_mcp_server(&mut settings.mcp_servers, server);
            settings.save().map_err(|e| e.to_string())?;
            build_status(&settings, &platform)
        }
        EnterprisePlatform::WeCom => build_status(&settings, &platform),
    }
}

#[tauri::command]
pub async fn test_enterprise_capability(
    state: State<'_, AppState>,
    platform: String,
) -> Result<EnterpriseCapabilityTestResult, String> {
    let settings = state.settings.lock().await;
    ensure_supported(&platform)?;
    let status = build_status(&settings, &platform)?;
    if !status.configured {
        let platform = status.platform.clone();
        return Ok(EnterpriseCapabilityTestResult {
            status,
            success: false,
            tools: Vec::new(),
            error: Some(missing_test_error(&platform)),
            diagnostics: missing_test_diagnostics(&platform),
        });
    }

    let platform_kind = parse_platform(&platform)?;
    match platform_kind {
        EnterprisePlatform::WeCom => {
            drop(settings);
            let cli = test_wecom_cli().await;
            let success = cli.is_ok();
            let error = cli.err();
            let settings = state.settings.lock().await;
            let status = build_status(&settings, &platform)?;
            Ok(EnterpriseCapabilityTestResult {
                status,
                success,
                tools: Vec::new(),
                error,
                diagnostics: build_wecom_cli_diagnostics(success),
            })
        }
        EnterprisePlatform::Feishu | EnterprisePlatform::DingTalk => {
            let server = match platform_kind {
                EnterprisePlatform::Feishu => settings
                    .mcp_servers
                    .iter()
                    .find(|s| s.name == FEISHU_ENTERPRISE_SERVER)
                    .cloned()
                    .unwrap_or_else(|| build_feishu_mcp_server(&settings)),
                EnterprisePlatform::DingTalk => settings
                    .mcp_servers
                    .iter()
                    .find(|s| s.name == DINGTALK_ENTERPRISE_SERVER)
                    .cloned()
                    .unwrap_or_else(|| build_dingtalk_mcp_server(&settings)),
                EnterprisePlatform::WeCom => unreachable!(),
            };
            let server = resolve_settings_placeholders_in_mcp_config(&server, &settings);
            drop(settings);

            let McpTestResult {
                success,
                tools,
                error,
            } = test_mcp_server_config(server).await;

            let diagnostics = build_test_diagnostics(&platform_kind, success, error.as_deref());
            let settings = state.settings.lock().await;
            let status = build_status(&settings, &platform)?;
            Ok(EnterpriseCapabilityTestResult {
                status,
                success,
                tools,
                error,
                diagnostics,
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnterprisePlatform {
    Feishu,
    WeCom,
    DingTalk,
}

fn parse_platform(platform: &str) -> Result<EnterprisePlatform, String> {
    let platform = platform.trim();
    if platform.eq_ignore_ascii_case(FEISHU_PLATFORM) {
        Ok(EnterprisePlatform::Feishu)
    } else if platform.eq_ignore_ascii_case(WECOM_PLATFORM) {
        Ok(EnterprisePlatform::WeCom)
    } else if platform.eq_ignore_ascii_case(DINGTALK_PLATFORM) {
        Ok(EnterprisePlatform::DingTalk)
    } else {
        Err(format!(
            "Enterprise capability template for '{platform}' is not available yet"
        ))
    }
}

fn ensure_supported(platform: &str) -> Result<(), String> {
    parse_platform(platform).map(|_| ())
}

fn feishu_template() -> EnterpriseCapabilityTemplate {
    EnterpriseCapabilityTemplate {
        platform: FEISHU_PLATFORM.into(),
        title: "Feishu / Lark Enterprise Capability".into(),
        description: "Official @larksuiteoapi/lark-mcp stdio server using the Feishu credentials already configured for the IM channel.".into(),
        supported: true,
        mcp_server_name: FEISHU_ENTERPRISE_SERVER.into(),
    }
}

fn wecom_template() -> EnterpriseCapabilityTemplate {
    EnterpriseCapabilityTemplate {
        platform: WECOM_PLATFORM.into(),
        title: "WeCom Enterprise Capability".into(),
        description: "Official @wecom/cli integration using the WeCom smart robot credentials already configured for the IM channel.".into(),
        supported: true,
        mcp_server_name: WECOM_ENTERPRISE_SERVER.into(),
    }
}

fn dingtalk_template() -> EnterpriseCapabilityTemplate {
    EnterpriseCapabilityTemplate {
        platform: DINGTALK_PLATFORM.into(),
        title: "DingTalk Enterprise Capability".into(),
        description: "Official DingTalk MCP Marketplace / AIHub remote MCP URL, registered as a Pisci MCP server.".into(),
        supported: true,
        mcp_server_name: DINGTALK_ENTERPRISE_SERVER.into(),
    }
}

fn build_status(settings: &Settings, platform: &str) -> Result<EnterpriseCapabilityStatus, String> {
    match parse_platform(platform)? {
        EnterprisePlatform::Feishu => build_feishu_status(settings),
        EnterprisePlatform::WeCom => build_wecom_status(settings),
        EnterprisePlatform::DingTalk => build_dingtalk_status(settings),
    }
}

fn build_feishu_status(settings: &Settings) -> Result<EnterpriseCapabilityStatus, String> {
    let mut missing = Vec::new();
    if settings.feishu_app_id.trim().is_empty() {
        missing.push("feishu_app_id".into());
    }
    if settings.feishu_app_secret.trim().is_empty() {
        missing.push("feishu_app_secret".into());
    }

    let mcp_server = settings
        .mcp_servers
        .iter()
        .find(|server| server.name == FEISHU_ENTERPRISE_SERVER);
    let configured = missing.is_empty();
    let mcp_configured = mcp_server.is_some();
    let mcp_enabled = mcp_server.map(|s| s.enabled).unwrap_or(false);
    let message = if !configured {
        format!("Missing credentials: {}", missing.join(", "))
    } else if !mcp_configured {
        "Feishu credentials are configured. Enterprise capability MCP is not enabled yet.".into()
    } else if !mcp_enabled {
        "Feishu enterprise MCP is configured but disabled.".into()
    } else {
        "Feishu enterprise MCP is configured and enabled.".into()
    };

    Ok(EnterpriseCapabilityStatus {
        platform: FEISHU_PLATFORM.into(),
        supported: true,
        configured,
        enabled: configured && mcp_configured && mcp_enabled,
        mcp_configured,
        mcp_enabled,
        mcp_server_name: FEISHU_ENTERPRISE_SERVER.into(),
        missing_credentials: missing,
        message,
    })
}

fn build_wecom_status(settings: &Settings) -> Result<EnterpriseCapabilityStatus, String> {
    let mut missing = Vec::new();
    if settings.wecom_bot_id.trim().is_empty() {
        missing.push("wecom_bot_id".into());
    }
    if settings.wecom_bot_secret.trim().is_empty() {
        missing.push("wecom_bot_secret".into());
    }
    let configured = missing.is_empty();
    let message = if configured {
        "WeCom credentials are configured. Official enterprise capabilities are provided by @wecom/cli; click Test to verify Node/npx and CLI availability.".into()
    } else {
        format!("Missing credentials: {}", missing.join(", "))
    };
    Ok(EnterpriseCapabilityStatus {
        platform: WECOM_PLATFORM.into(),
        supported: true,
        configured,
        enabled: false,
        mcp_configured: false,
        mcp_enabled: false,
        mcp_server_name: WECOM_ENTERPRISE_SERVER.into(),
        missing_credentials: missing,
        message,
    })
}

fn build_dingtalk_status(settings: &Settings) -> Result<EnterpriseCapabilityStatus, String> {
    let mut missing = Vec::new();
    if settings.dingtalk_mcp_url.trim().is_empty() {
        missing.push("dingtalk_mcp_url".into());
    }
    let mcp_server = settings
        .mcp_servers
        .iter()
        .find(|server| server.name == DINGTALK_ENTERPRISE_SERVER);
    let configured = missing.is_empty();
    let mcp_configured = mcp_server.is_some();
    let mcp_enabled = mcp_server.map(|s| s.enabled).unwrap_or(false);
    let message = if !configured {
        "Paste the official DingTalk MCP Marketplace / AIHub URL before enabling enterprise capability.".into()
    } else if !mcp_configured {
        "DingTalk MCP URL is configured. Enterprise capability MCP is not enabled yet.".into()
    } else if !mcp_enabled {
        "DingTalk enterprise MCP is configured but disabled.".into()
    } else {
        "DingTalk enterprise MCP is configured and enabled.".into()
    };

    Ok(EnterpriseCapabilityStatus {
        platform: DINGTALK_PLATFORM.into(),
        supported: true,
        configured,
        enabled: configured && mcp_configured && mcp_enabled,
        mcp_configured,
        mcp_enabled,
        mcp_server_name: DINGTALK_ENTERPRISE_SERVER.into(),
        missing_credentials: missing,
        message,
    })
}

fn build_feishu_mcp_server(settings: &Settings) -> McpServerConfig {
    let mut args = vec![
        "-y".into(),
        "@larksuiteoapi/lark-mcp".into(),
        "mcp".into(),
        "-a".into(),
        "${settings:feishu_app_id}".into(),
        "-s".into(),
        "${settings:feishu_app_secret}".into(),
    ];

    if settings.feishu_domain.trim().eq_ignore_ascii_case("lark") {
        args.push("--domain".into());
        args.push("https://open.larksuite.com".into());
    }

    McpServerConfig {
        name: FEISHU_ENTERPRISE_SERVER.into(),
        transport: "stdio".into(),
        command: if cfg!(windows) { "npx.cmd" } else { "npx" }.into(),
        args,
        url: String::new(),
        env: HashMap::new(),
        enabled: true,
    }
}

fn build_dingtalk_mcp_server(_settings: &Settings) -> McpServerConfig {
    McpServerConfig {
        name: DINGTALK_ENTERPRISE_SERVER.into(),
        transport: "sse".into(),
        command: String::new(),
        args: Vec::new(),
        url: "${settings:dingtalk_mcp_url}".into(),
        env: HashMap::new(),
        enabled: true,
    }
}

fn upsert_mcp_server(servers: &mut Vec<McpServerConfig>, server: McpServerConfig) {
    if let Some(existing) = servers.iter_mut().find(|s| s.name == server.name) {
        *existing = server;
    } else {
        servers.push(server);
    }
}

async fn test_wecom_cli() -> Result<(), String> {
    let mut command = Command::new(if cfg!(windows) { "npx.cmd" } else { "npx" });
    command
        .args(["-y", "@wecom/cli", "--help"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = timeout(Duration::from_secs(60), command.output())
        .await
        .map_err(|_| "Timed out while starting @wecom/cli via npx.".to_string())?
        .map_err(|e| format!("Failed to spawn npx for @wecom/cli: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

fn build_wecom_cli_diagnostics(success: bool) -> Vec<String> {
    if success {
        vec![
            "企业微信官方 CLI 可启动；首次实际调用企业能力前仍需执行一次 wecom-cli init 完成官方本地凭据初始化。".into(),
            "官方 CLI 会使用 @wecom/cli，并通过 contact/doc/meeting/msg/schedule/todo 等子命令提供能力。".into(),
        ]
    } else {
        vec![
            "无法启动企业微信官方 CLI。请确认本机已安装 Node.js，并且 npm/npx 在 PATH 中。".into(),
            "如果 CLI 已安装但未初始化，请在终端运行 wecom-cli init，按官方流程绑定 Bot ID / Secret。".into(),
        ]
    }
}

fn missing_test_error(platform: &str) -> String {
    match platform {
        FEISHU_PLATFORM => "Feishu App ID / App Secret are required before testing.".into(),
        WECOM_PLATFORM => "WeCom Bot ID / Secret are required before testing.".into(),
        DINGTALK_PLATFORM => "DingTalk MCP URL is required before testing.".into(),
        _ => "Enterprise capability is not configured.".into(),
    }
}

fn missing_test_diagnostics(platform: &str) -> Vec<String> {
    match platform {
        FEISHU_PLATFORM => vec!["请先在飞书卡片中填写 App ID 和 App Secret，并保存设置。".into()],
        WECOM_PLATFORM => vec!["请先在企业微信卡片中填写 Bot ID 和 Secret，并保存设置。".into()],
        DINGTALK_PLATFORM => {
            vec!["请先从钉钉 MCP 广场 / AIHub 复制官方 MCP URL，填入钉钉卡片并保存。".into()]
        }
        _ => vec!["请先完成该平台的企业能力配置。".into()],
    }
}

fn build_test_diagnostics(
    platform: &EnterprisePlatform,
    success: bool,
    error: Option<&str>,
) -> Vec<String> {
    if success {
        return vec![match platform {
            EnterprisePlatform::Feishu => {
                "飞书企业能力 MCP 已可用，Agent 可以加载并调用其暴露的工具。"
            }
            EnterprisePlatform::DingTalk => {
                "钉钉企业能力 MCP 已可用，Agent 可以加载并调用其暴露的工具。"
            }
            EnterprisePlatform::WeCom => unreachable!(),
        }
        .into()];
    }

    let Some(error) = error else {
        return vec!["测试失败，但 MCP 未返回具体错误。".into()];
    };
    let lower = error.to_ascii_lowercase();
    if lower.contains("failed to spawn") || lower.contains("program not found") {
        vec![
            "无法启动 npx。请确认本机已安装 Node.js，并且 npm/npx 在 PATH 中。".into(),
            "安装 Node.js 后重新点击“测试连接”。".into(),
        ]
    } else if lower.contains("timed out") || lower.contains("timeout") {
        vec![
            match platform {
                EnterprisePlatform::Feishu => "启动或连接飞书 MCP 超时。首次运行 npx 需要下载 @larksuiteoapi/lark-mcp，可能受网络或 npm 源影响。",
                EnterprisePlatform::DingTalk => "连接钉钉远程 MCP 超时。请确认 MCP URL 可访问，且浏览器侧授权流程已经完成。",
                EnterprisePlatform::WeCom => unreachable!(),
            }
            .into(),
        ]
    } else if lower.contains("permission")
        || lower.contains("scope")
        || lower.contains("unauthorized")
    {
        vec![match platform {
            EnterprisePlatform::Feishu => {
                "飞书开放平台权限或授权不足。请确认应用已开通需要的机器人、IM、群聊、日历等权限，并已发布。"
            }
            EnterprisePlatform::DingTalk => {
                "钉钉 MCP 授权不足。请从钉钉 MCP 广场 / AIHub 重新复制 URL，或完成页面 OAuth 授权。"
            }
            EnterprisePlatform::WeCom => unreachable!(),
        }
        .into()]
    } else {
        vec![match platform {
            EnterprisePlatform::Feishu => {
                "请检查飞书 App ID / App Secret、网络以及 npm registry 访问。"
            }
            EnterprisePlatform::DingTalk => {
                "请检查钉钉 MCP URL 是否完整、是否过期，以及网络是否能访问 mcp-gw.dingtalk.com。"
            }
            EnterprisePlatform::WeCom => unreachable!(),
        }
        .into()]
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_feishu_mcp_server, build_status, upsert_mcp_server, FEISHU_ENTERPRISE_SERVER,
    };
    use crate::commands::config::mcp::{
        resolve_settings_placeholders_in_mcp_config, test_mcp_server_config,
    };
    use crate::store::Settings;

    #[test]
    fn feishu_template_keeps_secret_as_placeholder() {
        let mut settings = Settings::default();
        settings.feishu_app_id = "cli_real".into();
        settings.feishu_app_secret = "secret_real".into();

        let server = build_feishu_mcp_server(&settings);
        assert_eq!(server.name, FEISHU_ENTERPRISE_SERVER);
        assert!(server.args.contains(&"${settings:feishu_app_id}".into()));
        assert!(server
            .args
            .contains(&"${settings:feishu_app_secret}".into()));
        assert!(!server.args.iter().any(|arg| arg.contains("secret_real")));
    }

    #[test]
    fn upsert_mcp_server_is_idempotent() {
        let mut settings = Settings::default();
        settings.feishu_app_id = "cli_real".into();
        settings.feishu_app_secret = "secret_real".into();
        let server = build_feishu_mcp_server(&settings);

        let mut servers = Vec::new();
        upsert_mcp_server(&mut servers, server.clone());
        upsert_mcp_server(&mut servers, server);

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, FEISHU_ENTERPRISE_SERVER);
    }

    #[test]
    fn feishu_status_reports_missing_credentials() {
        let settings = Settings::default();
        let status = build_status(&settings, "feishu").expect("status");
        assert!(!status.configured);
        assert_eq!(
            status.missing_credentials,
            vec!["feishu_app_id".to_string(), "feishu_app_secret".to_string()]
        );
    }

    #[tokio::test]
    async fn real_feishu_lark_mcp_lists_tools_when_enabled() {
        if std::env::var("PISCI_REAL_FEISHU_MCP_TEST").ok().as_deref() != Some("1") {
            eprintln!("skipping real Feishu MCP test; set PISCI_REAL_FEISHU_MCP_TEST=1");
            return;
        }

        let config_path = std::env::var("PISCI_REAL_FEISHU_CONFIG_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                let roaming = dirs::data_dir()
                    .unwrap_or_else(crate::app::logging::default_app_data_dir)
                    .join("com.pisci.desktop")
                    .join("config.json");
                if roaming.exists() {
                    roaming
                } else {
                    crate::app::logging::default_app_data_dir().join("config.json")
                }
            });
        let settings = Settings::load(&config_path).expect("load desktop settings");
        assert!(
            !settings.feishu_app_id.trim().is_empty()
                && !settings.feishu_app_secret.trim().is_empty(),
            "desktop settings must contain Feishu App ID and App Secret"
        );

        let server = build_feishu_mcp_server(&settings);
        let server = resolve_settings_placeholders_in_mcp_config(&server, &settings);
        assert!(
            !server.args.iter().any(|arg| arg.contains("${settings:")),
            "all placeholders should be resolved before spawning lark-mcp"
        );

        let result = test_mcp_server_config(server).await;
        assert!(
            result.success,
            "lark-mcp tools/list failed: {:?}",
            result.error
        );
        assert!(!result.tools.is_empty(), "lark-mcp returned no tools");
    }
}
