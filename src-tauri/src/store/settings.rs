use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A named LLM provider configuration that can be assigned to individual Koi.
/// Multiple entries allow different Koi to use different models/keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProviderConfig {
    /// Unique identifier (user-chosen, e.g. "gpt4-work", "claude-pro")
    pub id: String,
    /// Human-readable display label shown in dropdowns
    pub label: String,
    /// Provider type: "anthropic" | "openai" | "deepseek" | "qwen" | "minimax" | "zhipu" | "kimi" | "custom"
    pub provider: String,
    /// Model name (e.g. "gpt-4o", "claude-opus-4-5")
    pub model: String,
    /// API key (encrypted at rest, same scheme as global keys)
    #[serde(default)]
    pub api_key: String,
    /// Custom base URL for OpenAI-compatible endpoints (only used when provider = "custom")
    #[serde(default)]
    pub base_url: String,
    /// Max output tokens; 0 means inherit from global settings
    #[serde(default)]
    pub max_tokens: u32,
}

impl Default for LlmProviderConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            provider: "anthropic".into(),
            model: "claude-sonnet-4-5".into(),
            api_key: String::new(),
            base_url: String::new(),
            max_tokens: 0,
        }
    }
}

/// A pre-configured SSH server entry.
/// The password / private_key is stored encrypted on disk (same hex-AES scheme as API keys).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SshServerConfig {
    /// Unique alias used as `connection_id` (e.g. "prod", "dev-server")
    pub id: String,
    /// Display label (optional, falls back to id)
    #[serde(default)]
    pub label: String,
    /// Hostname or IP address
    pub host: String,
    /// SSH port (default 22)
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    /// SSH username
    pub username: String,
    /// Password (stored encrypted, empty if using key auth)
    #[serde(default)]
    pub password: String,
    /// PEM private key content (stored encrypted, empty if using password auth)
    #[serde(default)]
    pub private_key: String,
}

fn default_ssh_port() -> u16 {
    22
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Anthropic API key
    #[serde(default)]
    pub anthropic_api_key: String,
    /// OpenAI API key
    #[serde(default)]
    pub openai_api_key: String,
    /// DeepSeek API key
    #[serde(default)]
    pub deepseek_api_key: String,
    /// Qwen (通义千问) API key
    #[serde(default)]
    pub qwen_api_key: String,
    /// MiniMax API key
    #[serde(default)]
    pub minimax_api_key: String,
    /// Zhipu AI (智谱) API key
    #[serde(default)]
    pub zhipu_api_key: String,
    /// Kimi (Moonshot AI) API key
    #[serde(default)]
    pub kimi_api_key: String,
    /// Active LLM provider: "anthropic" | "openai" | "custom" | "deepseek" | "qwen"
    #[serde(default = "default_provider")]
    pub provider: String,
    /// Model name (e.g. "claude-sonnet-4-5" or "gpt-4o")
    #[serde(default = "default_model")]
    pub model: String,
    /// Custom base URL for OpenAI-compatible endpoints
    #[serde(default)]
    pub custom_base_url: String,
    /// Workspace root directory (files are restricted to this path)
    #[serde(default = "default_workspace")]
    pub workspace_root: String,
    /// When true, the agent may access files outside workspace_root (shows a warning).
    /// When false, workspace_root must be non-empty and all file access is restricted to it.
    #[serde(default)]
    pub allow_outside_workspace: bool,
    /// UI language: "zh" | "en"
    #[serde(default = "default_language")]
    pub language: String,
    /// Maximum tokens per LLM response (output only)
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Context window size in tokens (input limit).
    /// 0 means "auto" — the backend will use a conservative default based on the model.
    /// Common values: 8192, 32768, 65536, 131072, 1000000
    #[serde(default)]
    pub context_window: u32,
    /// Whether to show permission dialogs for shell commands
    #[serde(default = "default_true")]
    pub confirm_shell_commands: bool,
    /// Whether to show permission dialogs for file writes
    #[serde(default = "default_true")]
    pub confirm_file_writes: bool,
    /// Run browser in headless mode (invisible). false = user can see the browser window.
    #[serde(default = "default_true")]
    pub browser_headless: bool,
    /// Policy profile: strict | balanced | dev
    #[serde(default = "default_policy_mode")]
    pub policy_mode: String,
    /// Max tool calls per minute per session
    #[serde(default = "default_tool_rate_limit")]
    pub tool_rate_limit_per_minute: u32,

    // ── IM Gateway ──────────────────────────────────────────────────────────
    /// Feishu App ID
    #[serde(default)]
    pub feishu_app_id: String,
    /// Feishu App Secret
    #[serde(default)]
    pub feishu_app_secret: String,
    /// Feishu domain: "feishu" | "lark"
    #[serde(default = "default_feishu_domain")]
    pub feishu_domain: String,
    /// Feishu enabled
    #[serde(default)]
    pub feishu_enabled: bool,

    /// WeCom (企业微信) Corp ID
    #[serde(default)]
    pub wecom_corp_id: String,
    /// WeCom Agent Secret
    #[serde(default)]
    pub wecom_agent_secret: String,
    /// WeCom Agent ID
    #[serde(default)]
    pub wecom_agent_id: String,
    /// WeCom enabled
    #[serde(default)]
    pub wecom_enabled: bool,

    /// DingTalk App Key
    #[serde(default)]
    pub dingtalk_app_key: String,
    /// DingTalk App Secret
    #[serde(default)]
    pub dingtalk_app_secret: String,
    /// DingTalk enabled
    #[serde(default)]
    pub dingtalk_enabled: bool,

    /// Telegram Bot Token
    #[serde(default)]
    pub telegram_bot_token: String,
    /// Telegram enabled
    #[serde(default)]
    pub telegram_enabled: bool,

    /// Slack incoming webhook URL
    #[serde(default)]
    pub slack_webhook_url: String,
    #[serde(default)]
    pub slack_enabled: bool,

    /// Discord webhook URL
    #[serde(default)]
    pub discord_webhook_url: String,
    #[serde(default)]
    pub discord_enabled: bool,

    /// Microsoft Teams incoming webhook URL
    #[serde(default)]
    pub teams_webhook_url: String,
    #[serde(default)]
    pub teams_enabled: bool,

    /// Matrix homeserver base URL (e.g. https://matrix.org)
    #[serde(default)]
    pub matrix_homeserver: String,
    /// Matrix access token
    #[serde(default)]
    pub matrix_access_token: String,
    /// Matrix room id
    #[serde(default)]
    pub matrix_room_id: String,
    #[serde(default)]
    pub matrix_enabled: bool,

    /// Generic outbound webhook URL
    #[serde(default)]
    pub webhook_outbound_url: String,
    /// Optional bearer token for outbound webhook
    #[serde(default)]
    pub webhook_auth_token: String,
    #[serde(default)]
    pub webhook_enabled: bool,

    /// Optional local relay inbox file for WeCom inbound bridging
    #[serde(default)]
    pub wecom_inbox_file: String,

    // ── Email (SMTP / IMAP) ──────────────────────────────────────────────────
    /// SMTP server hostname (e.g. smtp.gmail.com)
    #[serde(default)]
    pub smtp_host: String,
    /// SMTP port (default 587 for STARTTLS, 465 for SSL)
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    /// SMTP/IMAP account username (usually the email address)
    #[serde(default)]
    pub smtp_username: String,
    /// SMTP account password or app-password (encrypted at rest)
    #[serde(default)]
    pub smtp_password: String,
    /// IMAP server hostname (e.g. imap.gmail.com)
    #[serde(default)]
    pub imap_host: String,
    /// IMAP port (default 993 for SSL)
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    /// Sender display name shown in the From header (optional)
    #[serde(default)]
    pub smtp_from_name: String,
    /// Whether email tool is enabled
    #[serde(default)]
    pub email_enabled: bool,

    // ── Agent Loop ──────────────────────────────────────────────────────────
    /// Maximum tool-call iterations per agent run (default 50)
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,

    // ── Heartbeat ───────────────────────────────────────────────────────────
    /// Whether the heartbeat runner is enabled
    #[serde(default)]
    pub heartbeat_enabled: bool,
    /// Heartbeat interval in minutes (default 30)
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_mins: u32,
    /// Prompt sent to the agent on each heartbeat
    #[serde(default = "default_heartbeat_prompt")]
    pub heartbeat_prompt: String,
    /// Whether the app has already seeded the first-run starter Koi set.
    #[serde(default)]
    pub starter_kois_initialized: bool,

    // ── User Tools ──────────────────────────────────────────────────────────
    /// Per-user-tool config values, keyed by tool name.
    /// Each value is a JSON object with the fields from the tool's config_schema.
    /// Password fields are stored encrypted (same hex-AES scheme as API keys).
    #[serde(default)]
    pub user_tool_configs: HashMap<String, Value>,
    /// Built-in tool enable switches, keyed by tool name.
    /// Missing key means enabled by default.
    #[serde(default)]
    pub builtin_tool_enabled: HashMap<String, bool>,

    // ── MCP Servers ─────────────────────────────────────────────────────────
    /// Configured MCP (Model Context Protocol) servers.
    /// Each server exposes tools that are dynamically registered into the agent's tool registry.
    #[serde(default)]
    pub mcp_servers: Vec<crate::tools::mcp::McpServerConfig>,

    // ── SSH Servers ──────────────────────────────────────────────────────────
    /// Pre-configured SSH servers. The agent can connect by alias (connection_id)
    /// without needing to know the credentials.
    #[serde(default)]
    pub ssh_servers: Vec<SshServerConfig>,

    // ── Named LLM Providers ──────────────────────────────────────────────────
    /// Named LLM provider configurations. Each Koi can reference one by id.
    /// When a Koi has no provider assigned, the global provider/model/key is used.
    #[serde(default)]
    pub llm_providers: Vec<LlmProviderConfig>,

    // ── Runtime Paths ───────────────────────────────────────────────────────
    /// User-specified executable paths for runtimes not found on PATH.
    /// Keys: "node", "npm", "python", "pip", "git"
    /// Values: absolute path to the executable (e.g. "C:\\Python312\\python.exe")
    #[serde(default)]
    pub runtime_paths: HashMap<String, String>,

    // ── Vision / Multimodal ─────────────────────────────────────────────────
    /// Whether the current model supports vision (image input).
    /// When true, inbound IM images are passed as visual content blocks to the LLM.
    /// Auto-detected for known models (Claude 3+, GPT-4o, Gemini, etc.); set this
    /// manually when using a custom model name that isn't auto-recognised.
    #[serde(default)]
    pub vision_enabled: bool,

    // ── Overlay position ─────────────────────────────────────────────────────
    /// Last saved X position of the overlay window (physical pixels).
    /// None means "first launch — center relative to main window".
    #[serde(default)]
    pub overlay_x: Option<i32>,
    /// Last saved Y position of the overlay window (physical pixels).
    #[serde(default)]
    pub overlay_y: Option<i32>,

    /// When true, multiple instances of the app may run simultaneously.
    /// When false (default), launching a second instance will focus the existing window
    /// and exit the new process immediately.
    #[serde(default)]
    pub allow_multiple_instances: bool,

    /// Internal: path to the config file (not serialized)
    #[serde(skip)]
    pub config_path: PathBuf,
}

fn default_feishu_domain() -> String {
    "feishu".into()
}
fn default_smtp_port() -> u16 {
    587
}
fn default_imap_port() -> u16 {
    993
}
fn default_max_iterations() -> u32 {
    50
}
fn default_heartbeat_interval() -> u32 {
    30
}
pub fn default_heartbeat_prompt() -> String {
    "这是你的例行心跳巡查。按以下清单逐项完成，然后回复 HEARTBEAT_OK。\n\n\
     ## 1. 活跃项目巡查\n\
     用 pool_org(action=\"list\") 列出所有项目池。对每个 active 的池：\n\
     - pool_org(action=\"get_todos\", pool_id=...) — 查看任务板状态\n\
     - pool_chat(action=\"read\", pool_id=...) — 阅读最新消息\n\n\
     根据任务板状态判断并执行：\n\
     - 若所有 todo 状态均为 done 或 cancelled（无 todo/in_progress/blocked）：\n\
       立即执行 pool_org(action=\"archive\", pool_id=...) 归档项目，并在 pool_chat 发送收尾总结。\n\
     - 若有 blocked 任务：主动解除阻塞或重新分配。\n\
     - 若 Koi 在空转讨论无产出：在 pool_chat 推动下一步或分配具体任务。\n\
     - 若项目卡住无进展：介入协调。\n\n\
     ⚠️ 重要：在心跳巡查中，**绝对不要创建新的项目池**。\n\
     如果需要追加工作，应在现有项目池中用 pool_org(action=\"create_todo\") 创建任务，\n\
     或通过 pool_chat @mention 分配给相关 Koi。\n\
     只有用户明确要求启动新项目时，才能创建新的项目池。\n\n\
     ## 2. Koi 状态检查\n\
     查看是否有 Koi 异常（长时间 busy 但无活跃 todo）。如有，在对应 pool_chat 通知或分配新任务。\n\n\
     ## 3. 定时任务检查\n\
     用 app_control(action=\"list_scheduled_tasks\") 查看是否有应运行的计划任务，按需处理。\n\n\
     完成以上全部巡查后，回复 HEARTBEAT_OK。"
        .into()
}

fn default_provider() -> String {
    "anthropic".into()
}
fn default_model() -> String {
    "claude-sonnet-4-5".into()
}
pub fn default_workspace_path() -> String {
    dirs::document_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Pisci")
        .to_string_lossy()
        .into_owned()
}

fn default_workspace() -> String {
    default_workspace_path()
}
fn default_language() -> String {
    "zh".into()
}
fn default_max_tokens() -> u32 {
    4096
}
fn default_true() -> bool {
    true
}
fn default_policy_mode() -> String {
    "balanced".into()
}
fn default_tool_rate_limit() -> u32 {
    120
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            anthropic_api_key: String::new(),
            openai_api_key: String::new(),
            deepseek_api_key: String::new(),
            qwen_api_key: String::new(),
            minimax_api_key: String::new(),
            zhipu_api_key: String::new(),
            kimi_api_key: String::new(),
            provider: default_provider(),
            model: default_model(),
            custom_base_url: String::new(),
            workspace_root: default_workspace(),
            allow_outside_workspace: false,
            language: default_language(),
            max_tokens: default_max_tokens(),
            context_window: 0,
            confirm_shell_commands: true,
            confirm_file_writes: true,
            browser_headless: true,
            policy_mode: default_policy_mode(),
            tool_rate_limit_per_minute: default_tool_rate_limit(),
            feishu_app_id: String::new(),
            feishu_app_secret: String::new(),
            feishu_domain: default_feishu_domain(),
            feishu_enabled: false,
            wecom_corp_id: String::new(),
            wecom_agent_secret: String::new(),
            wecom_agent_id: String::new(),
            wecom_enabled: false,
            dingtalk_app_key: String::new(),
            dingtalk_app_secret: String::new(),
            dingtalk_enabled: false,
            telegram_bot_token: String::new(),
            telegram_enabled: false,
            slack_webhook_url: String::new(),
            slack_enabled: false,
            discord_webhook_url: String::new(),
            discord_enabled: false,
            teams_webhook_url: String::new(),
            teams_enabled: false,
            matrix_homeserver: String::new(),
            matrix_access_token: String::new(),
            matrix_room_id: String::new(),
            matrix_enabled: false,
            webhook_outbound_url: String::new(),
            webhook_auth_token: String::new(),
            webhook_enabled: false,
            wecom_inbox_file: String::new(),
            smtp_host: String::new(),
            smtp_port: default_smtp_port(),
            smtp_username: String::new(),
            smtp_password: String::new(),
            imap_host: String::new(),
            imap_port: default_imap_port(),
            smtp_from_name: String::new(),
            email_enabled: false,
            max_iterations: default_max_iterations(),
            heartbeat_enabled: false,
            heartbeat_interval_mins: default_heartbeat_interval(),
            heartbeat_prompt: default_heartbeat_prompt(),
            starter_kois_initialized: false,
            user_tool_configs: HashMap::new(),
            builtin_tool_enabled: HashMap::new(),
            mcp_servers: Vec::new(),
            ssh_servers: Vec::new(),
            llm_providers: Vec::new(),
            runtime_paths: HashMap::new(),
            vision_enabled: false,
            overlay_x: None,
            overlay_y: None,
            allow_multiple_instances: false,
            config_path: PathBuf::new(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        let mut settings = if path.exists() {
            let content = std::fs::read_to_string(path)?;
            match serde_json::from_str::<Settings>(&content) {
                Ok(s) => s,
                Err(e) => {
                    // Log the parse error and keep the broken file as a backup
                    tracing::error!(
                        "Failed to parse settings file at '{}': {}. \
                         Falling back to defaults. Original file preserved.",
                        path.display(),
                        e
                    );
                    // Rename broken file so it isn't overwritten silently
                    let backup = path.with_extension("json.bak");
                    let _ = std::fs::copy(path, &backup);
                    Settings::default()
                }
            }
        } else {
            Settings::default()
        };
        settings.config_path = path.to_path_buf();

        // Decrypt API keys (hex-encoded ciphertext on disk).
        // If decryption fails the value is likely still plaintext (pre-migration);
        // keep it as-is and the next save() will encrypt it.
        if let Some(store) = Self::secret_store(path) {
            Self::try_decrypt_field(&store, &mut settings.anthropic_api_key);
            Self::try_decrypt_field(&store, &mut settings.openai_api_key);
            Self::try_decrypt_field(&store, &mut settings.deepseek_api_key);
            Self::try_decrypt_field(&store, &mut settings.qwen_api_key);
            Self::try_decrypt_field(&store, &mut settings.minimax_api_key);
            Self::try_decrypt_field(&store, &mut settings.zhipu_api_key);
            Self::try_decrypt_field(&store, &mut settings.kimi_api_key);
            Self::try_decrypt_field(&store, &mut settings.feishu_app_secret);
            Self::try_decrypt_field(&store, &mut settings.wecom_agent_secret);
            Self::try_decrypt_field(&store, &mut settings.dingtalk_app_secret);
            Self::try_decrypt_field(&store, &mut settings.telegram_bot_token);
            Self::try_decrypt_field(&store, &mut settings.matrix_access_token);
            Self::try_decrypt_field(&store, &mut settings.webhook_auth_token);
            Self::try_decrypt_field(&store, &mut settings.smtp_password);
            // Decrypt SSH server credentials
            for srv in &mut settings.ssh_servers {
                Self::try_decrypt_field(&store, &mut srv.password);
                Self::try_decrypt_field(&store, &mut srv.private_key);
            }
            // Decrypt named LLM provider API keys
            for p in &mut settings.llm_providers {
                Self::try_decrypt_field(&store, &mut p.api_key);
            }
        }
        Ok(settings)
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut clone = self.clone();
        // Encrypt secret fields before writing to disk
        if let Some(store) = Self::secret_store(&self.config_path) {
            Self::encrypt_field(&store, &mut clone.anthropic_api_key);
            Self::encrypt_field(&store, &mut clone.openai_api_key);
            Self::encrypt_field(&store, &mut clone.deepseek_api_key);
            Self::encrypt_field(&store, &mut clone.qwen_api_key);
            Self::encrypt_field(&store, &mut clone.minimax_api_key);
            Self::encrypt_field(&store, &mut clone.zhipu_api_key);
            Self::encrypt_field(&store, &mut clone.kimi_api_key);
            Self::encrypt_field(&store, &mut clone.feishu_app_secret);
            Self::encrypt_field(&store, &mut clone.wecom_agent_secret);
            Self::encrypt_field(&store, &mut clone.dingtalk_app_secret);
            Self::encrypt_field(&store, &mut clone.telegram_bot_token);
            Self::encrypt_field(&store, &mut clone.matrix_access_token);
            Self::encrypt_field(&store, &mut clone.webhook_auth_token);
            Self::encrypt_field(&store, &mut clone.smtp_password);
            // Encrypt SSH server credentials
            for srv in &mut clone.ssh_servers {
                Self::encrypt_field(&store, &mut srv.password);
                Self::encrypt_field(&store, &mut srv.private_key);
            }
            // Encrypt named LLM provider API keys
            for p in &mut clone.llm_providers {
                Self::encrypt_field(&store, &mut p.api_key);
            }
        }
        let json = serde_json::to_string_pretty(&clone)?;
        std::fs::write(&self.config_path, json)?;
        Ok(())
    }

    fn secret_store(config_path: &Path) -> Option<crate::security::secrets::SecretStore> {
        config_path
            .parent()
            .and_then(|dir| crate::security::secrets::SecretStore::new(dir).ok())
    }

    fn encrypt_field(store: &crate::security::secrets::SecretStore, field: &mut String) {
        if field.is_empty() {
            return;
        }
        if let Ok(encrypted) = store.encrypt_hex(field) {
            *field = encrypted;
        }
    }

    fn try_decrypt_field(store: &crate::security::secrets::SecretStore, field: &mut String) {
        if field.is_empty() {
            return;
        }
        if let Ok(decrypted) = store.decrypt_hex(field) {
            *field = decrypted;
        }
        // If decrypt fails, the field is probably still plaintext (legacy) — keep as-is
    }

    /// Returns true if at least one API key is configured
    pub fn is_configured(&self) -> bool {
        !self.anthropic_api_key.trim().is_empty()
            || !self.openai_api_key.trim().is_empty()
            || !self.deepseek_api_key.trim().is_empty()
            || !self.qwen_api_key.trim().is_empty()
            || !self.minimax_api_key.trim().is_empty()
            || !self.zhipu_api_key.trim().is_empty()
            || !self.kimi_api_key.trim().is_empty()
    }

    /// Returns the active API key for the configured provider
    pub fn active_api_key(&self) -> &str {
        match self.provider.as_str() {
            "openai" | "custom" => &self.openai_api_key,
            "deepseek" => &self.deepseek_api_key,
            "qwen" | "tongyi" => &self.qwen_api_key,
            "minimax" => &self.minimax_api_key,
            "zhipu" => &self.zhipu_api_key,
            "kimi" | "moonshot" => &self.kimi_api_key,
            _ => &self.anthropic_api_key,
        }
    }

    /// Look up a named LLM provider by its id.
    /// Returns `None` if no provider with that id exists.
    pub fn find_llm_provider(&self, id: &str) -> Option<&LlmProviderConfig> {
        self.llm_providers.iter().find(|p| p.id == id)
    }

    /// Returns the default config file path used by the app before Tauri's AppHandle is available.
    /// Mirrors the path that `AppState::new_sync` computes via `app.path().app_data_dir()`.
    /// On Windows this is `%APPDATA%\com.pisci.desktop\config.json`.
    pub fn default_config_path() -> PathBuf {
        // Tauri v2 uses `<data_dir>/<bundle_identifier>` on Windows (APPDATA\<id>).
        // The bundle identifier is "com.pisci.desktop" (from tauri.conf.json).
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("com.pisci.desktop")
            .join("config.json")
    }
}
