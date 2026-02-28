use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    /// UI language: "zh" | "en"
    #[serde(default = "default_language")]
    pub language: String,
    /// Maximum tokens per LLM response
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
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

    // ── User Tools ──────────────────────────────────────────────────────────
    /// Per-user-tool config values, keyed by tool name.
    /// Each value is a JSON object with the fields from the tool's config_schema.
    /// Password fields are stored encrypted (same hex-AES scheme as API keys).
    #[serde(default)]
    pub user_tool_configs: HashMap<String, Value>,

    /// Internal: path to the config file (not serialized)
    #[serde(skip)]
    pub config_path: PathBuf,
}

fn default_feishu_domain() -> String { "feishu".into() }
fn default_smtp_port() -> u16 { 587 }
fn default_imap_port() -> u16 { 993 }
fn default_max_iterations() -> u32 { 50 }
fn default_heartbeat_interval() -> u32 { 30 }
fn default_heartbeat_prompt() -> String { "检查是否有待处理任务，如无则回复 HEARTBEAT_OK".into() }

fn default_provider() -> String { "anthropic".into() }
fn default_model() -> String { "claude-sonnet-4-5".into() }
fn default_workspace() -> String {
    dirs::document_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Pisci")
        .to_string_lossy()
        .into_owned()
}
fn default_language() -> String { "zh".into() }
fn default_max_tokens() -> u32 { 4096 }
fn default_true() -> bool { true }
fn default_policy_mode() -> String { "balanced".into() }
fn default_tool_rate_limit() -> u32 { 120 }

impl Default for Settings {
    fn default() -> Self {
        Self {
            anthropic_api_key: String::new(),
            openai_api_key: String::new(),
            deepseek_api_key: String::new(),
            qwen_api_key: String::new(),
            provider: default_provider(),
            model: default_model(),
            custom_base_url: String::new(),
            workspace_root: default_workspace(),
            language: default_language(),
            max_tokens: default_max_tokens(),
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
            user_tool_configs: HashMap::new(),
            config_path: PathBuf::new(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        let mut settings = if path.exists() {
            let content = std::fs::read_to_string(path)?;
            serde_json::from_str::<Settings>(&content).unwrap_or_default()
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
            Self::try_decrypt_field(&store, &mut settings.feishu_app_secret);
            Self::try_decrypt_field(&store, &mut settings.wecom_agent_secret);
            Self::try_decrypt_field(&store, &mut settings.dingtalk_app_secret);
            Self::try_decrypt_field(&store, &mut settings.telegram_bot_token);
            Self::try_decrypt_field(&store, &mut settings.matrix_access_token);
            Self::try_decrypt_field(&store, &mut settings.webhook_auth_token);
            Self::try_decrypt_field(&store, &mut settings.smtp_password);
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
            Self::encrypt_field(&store, &mut clone.feishu_app_secret);
            Self::encrypt_field(&store, &mut clone.wecom_agent_secret);
            Self::encrypt_field(&store, &mut clone.dingtalk_app_secret);
            Self::encrypt_field(&store, &mut clone.telegram_bot_token);
            Self::encrypt_field(&store, &mut clone.matrix_access_token);
            Self::encrypt_field(&store, &mut clone.webhook_auth_token);
            Self::encrypt_field(&store, &mut clone.smtp_password);
        }
        let json = serde_json::to_string_pretty(&clone)?;
        std::fs::write(&self.config_path, json)?;
        Ok(())
    }

    fn secret_store(config_path: &Path) -> Option<crate::security::secrets::SecretStore> {
        config_path.parent().and_then(|dir| {
            crate::security::secrets::SecretStore::new(dir).ok()
        })
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
    }

    /// Returns the active API key for the configured provider
    pub fn active_api_key(&self) -> &str {
        match self.provider.as_str() {
            "openai" | "custom" => &self.openai_api_key,
            "deepseek" => &self.deepseek_api_key,
            "qwen" | "tongyi" => &self.qwen_api_key,
            _ => &self.anthropic_api_key,
        }
    }
}
