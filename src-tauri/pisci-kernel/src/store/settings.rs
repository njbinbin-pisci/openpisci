use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Configuration for an MCP (Model Context Protocol) server.
///
/// This lives in the kernel's settings module (rather than the desktop
/// `tools/mcp.rs`) so that the serialized shape of [`Settings`] has no
/// dependency on tool implementation crates. Desktop code that actually
/// runs an MCP server still lives in `src/tools/mcp.rs` and re-uses this
/// struct via `pisci_kernel::store::settings::McpServerConfig`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpServerConfig {
    /// Display name for this server
    pub name: String,
    /// "stdio" or "sse"
    pub transport: String,
    /// For stdio: the executable command
    #[serde(default)]
    pub command: String,
    /// For stdio: command arguments
    #[serde(default)]
    pub args: Vec<String>,
    /// For sse: the HTTP base URL (e.g. "http://localhost:3000")
    #[serde(default)]
    pub url: String,
    /// Extra environment variables for stdio processes
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Whether this server is enabled
    #[serde(default = "mcp_default_true")]
    pub enabled: bool,
}

fn mcp_default_true() -> bool {
    true
}

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

impl LlmProviderConfig {
    /// Returns the API key value to pass to provider clients.
    pub fn effective_api_key(&self) -> &str {
        if self.api_key.trim().is_empty()
            && is_local_ollama_openai_provider(&self.provider, &self.base_url)
        {
            return "ollama";
        }
        &self.api_key
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

fn has_local_ollama_prefix(url: &str, prefix: &str) -> bool {
    url.strip_prefix(prefix)
        .map(|rest| rest.is_empty() || rest.starts_with('/'))
        .unwrap_or(false)
}

/// Ollama's local OpenAI-compatible API ignores bearer tokens, but OpenAI-style
/// clients still expect a non-empty API key value.
pub fn is_local_ollama_openai_provider(provider: &str, base_url: &str) -> bool {
    let provider = provider.trim().to_ascii_lowercase();
    if provider != "custom" && provider != "ollama" {
        return false;
    }

    let url = base_url.trim().trim_end_matches('/').to_ascii_lowercase();
    [
        "http://localhost:11434",
        "http://127.0.0.1:11434",
        "http://[::1]:11434",
    ]
    .iter()
    .any(|prefix| has_local_ollama_prefix(&url, prefix))
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
    /// Fallback models tried in order when the primary model fails with
    /// rate_limit / overloaded / model_not_found errors.
    /// Format: "provider/model" e.g. ["anthropic/claude-haiku-3-5", "openai/gpt-4o-mini"]
    #[serde(default)]
    pub fallback_models: Vec<String>,
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
    //
    // Layered architecture (see `pisci-core::scene::SceneKind::IMHeadless`
    // and `crate::tools::mcp`):
    //   * Credential layer  — `*_app_id` / `*_bot_id` / `*_app_secret` are
    //     the application-level credentials shared by the IM channel and
    //     by any enterprise-capability MCP that uses the same platform.
    //   * Channel layer    — `*_enabled` flips the WebSocket / Stream
    //     long-connection on or off; channels only carry message
    //     transport.
    //   * Capability layer — enterprise APIs (org chart, calendar, group
    //     chat, …) live in `mcp_servers` (see below) and reference the
    //     credentials above through `${settings:*}` placeholders so users
    //     never duplicate secrets between the channel and the MCP.
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

    /// WeCom smart robot Bot ID (long-connection mode)
    #[serde(default)]
    pub wecom_bot_id: String,
    /// WeCom smart robot Secret (long-connection mode)
    #[serde(default)]
    pub wecom_bot_secret: String,
    /// WeCom enabled
    #[serde(default)]
    pub wecom_enabled: bool,

    /// DingTalk App Key
    #[serde(default)]
    pub dingtalk_app_key: String,
    /// DingTalk App Secret
    #[serde(default)]
    pub dingtalk_app_secret: String,
    /// DingTalk robot code
    #[serde(default)]
    pub dingtalk_robot_code: String,
    /// DingTalk Corp ID (kept for official OpenClaw-compatible config)
    #[serde(default)]
    pub dingtalk_corp_id: String,
    /// DingTalk Agent ID (kept for official OpenClaw-compatible config)
    #[serde(default)]
    pub dingtalk_agent_id: String,
    /// DingTalk official MCP Marketplace / AIHub URL (Streamable HTTP/SSE)
    #[serde(default)]
    pub dingtalk_mcp_url: String,
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

    /// WeChat (iLink Bot HTTP server) enabled
    #[serde(default)]
    pub wechat_enabled: bool,
    /// Optional Bearer token for the local iLink Bot HTTP server (guards the listener)
    #[serde(default)]
    pub wechat_gateway_token: String,
    /// Local HTTP server port for the iLink Bot API (default 18788)
    #[serde(default = "default_wechat_gateway_port")]
    pub wechat_gateway_port: u16,
    /// bot_token obtained after QR-code login (encrypted at rest)
    #[serde(default)]
    pub wechat_bot_token: String,
    /// baseurl returned by the iLink login API (e.g. https://ilinkai.weixin.qq.com)
    #[serde(default)]
    pub wechat_base_url: String,
    /// ilink_bot_id of the bound WeChat account
    #[serde(default)]
    pub wechat_bot_id: String,

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
    /// Cumulative-input-tokens cap after which an auto summary/compaction
    /// kicks in. Set to 0 to disable.
    ///
    /// Works alongside — not replaced by — the three-tier percent scheme
    /// ([`Self::compaction_micro_percent`] / `compaction_auto_percent` /
    /// `compaction_full_percent`): the percent tiers shape *how* compaction
    /// behaves at each utilisation band, while this absolute threshold
    /// decides *when* the threshold-driven path fires (scene policy can
    /// override via `auto_compact_threshold_override`).
    #[serde(default = "default_auto_compact_input_tokens_threshold")]
    pub auto_compact_input_tokens_threshold: u32,

    /// p5a / Claw-inspired three-tier compaction thresholds, expressed as
    /// *percentage of the effective input-token budget* (= context window
    /// minus `max_tokens`).
    ///
    /// *   Below `compaction_micro_percent` — nothing extra happens, just
    ///     the default receipt demotion from p5.
    /// *   `compaction_micro_percent`..`compaction_auto_percent` — MICRO:
    ///     aggressive receipt demotion + per-result truncation (see
    ///     `max_tool_result_tokens`).
    /// *   `compaction_auto_percent`..`compaction_full_percent` — AUTO:
    ///     trigger an async summary run (p7), replace the oldest non-recent
    ///     history with a rolling summary.
    /// *   `compaction_full_percent`+ — FULL: synchronous summary before
    ///     the next LLM call; last line of defence against overflow.
    #[serde(default = "default_compaction_micro_percent")]
    pub compaction_micro_percent: u8,
    #[serde(default = "default_compaction_auto_percent")]
    pub compaction_auto_percent: u8,
    #[serde(default = "default_compaction_full_percent")]
    pub compaction_full_percent: u8,

    /// Per-tool-result truncation cap in tokens (p5a). Tool results whose
    /// estimated tokens exceed this value are written to the minimal
    /// receipt form at the time the history is constructed, regardless of
    /// how recent they are. 0 disables the cap.
    #[serde(default = "default_max_tool_result_tokens")]
    pub max_tool_result_tokens: u32,

    /// Optional override for the model used to produce rolling summaries
    /// (p7). When `None` or empty, the main chat model is reused.
    #[serde(default)]
    pub summary_model: Option<String>,
    /// Maximum characters from project instruction files injected into the system prompt.
    #[serde(default = "default_project_instruction_budget_chars")]
    pub project_instruction_budget_chars: u32,
    /// Whether project-level instruction files (PISCI.md, .pisci/instructions.md, etc.)
    /// should be discovered and injected into the system prompt.
    #[serde(default = "default_true")]
    pub enable_project_instructions: bool,
    /// LLM read timeout in seconds (default 120). Increase for slow models.
    #[serde(default = "default_llm_read_timeout_secs")]
    pub llm_read_timeout_secs: u32,
    /// Koi task execution timeout in seconds (default 600 = 10 min). Increase for complex tasks.
    #[serde(default = "default_koi_timeout_secs")]
    pub koi_timeout_secs: u32,

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
    pub mcp_servers: Vec<McpServerConfig>,

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

    // ── Streaming output ────────────────────────────────────────────────────
    /// Stream LLM text deltas to the UI as they arrive instead of waiting
    /// for the full response. Only wired into the main chat / Koi task
    /// scenes; headless / coordinator / heartbeat paths ignore this flag
    /// because they do not surface text to a live UI.
    #[serde(default)]
    pub enable_streaming: bool,

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
fn default_wechat_gateway_port() -> u16 {
    18789
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
fn default_auto_compact_input_tokens_threshold() -> u32 {
    // Bumped from 100k → 200k after introducing dual-version tool results.
    // With minimal receipts in middle-tier turns, sessions can accumulate more
    // raw input tokens before a forced Level-2 summarisation buys us anything,
    // so the old default was firing far too often on long research sessions.
    200_000
}
fn default_compaction_micro_percent() -> u8 {
    // Align with `crate::agent::harness::budget::DEFAULT_TIER_MICRO_PERCENT`.
    // Duplicated here (rather than re-exported) so the store crate stays
    // leaf-level with no agent dependency.
    60
}
fn default_compaction_auto_percent() -> u8 {
    80
}
fn default_compaction_full_percent() -> u8 {
    95
}
fn default_max_tool_result_tokens() -> u32 {
    // 8k tokens ≈ 32 KB at ~4 chars/token; enough to hold a full file_read
    // of a medium source file but small enough that one oversized result
    // never dominates a 200k-token budget.
    8_000
}
fn default_project_instruction_budget_chars() -> u32 {
    8_000
}
fn default_llm_read_timeout_secs() -> u32 {
    120
}
fn default_koi_timeout_secs() -> u32 {
    600
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
       不要自动归档。先在 pool_chat 发送收尾总结，明确说明项目已进入“待用户确认归档”状态；只有用户明确要求归档时，Pisci 才能执行 pool_org(action=\"archive\", pool_id=...)。\n\
     - 若有 blocked 任务：主动解除阻塞或重新分配。\n\
     - 若 Koi 在空转讨论无产出：在 pool_chat 推动下一步或分配具体任务。\n\
     - 若项目卡住无进展：介入协调。\n\n\
     ⚠️ 重要：在心跳巡查中，**绝对不要创建新的项目池**。\n\
     如果需要追加工作，应在现有项目池中用 pool_org(action=\"create_todo\") 创建任务，\n\
     或通过 pool_chat @!mention 分配给相关 Koi。\n\
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
            fallback_models: vec![],
            confirm_shell_commands: true,
            confirm_file_writes: true,
            browser_headless: true,
            policy_mode: default_policy_mode(),
            tool_rate_limit_per_minute: default_tool_rate_limit(),
            feishu_app_id: String::new(),
            feishu_app_secret: String::new(),
            feishu_domain: default_feishu_domain(),
            feishu_enabled: false,
            wecom_bot_id: String::new(),
            wecom_bot_secret: String::new(),
            wecom_enabled: false,
            dingtalk_app_key: String::new(),
            dingtalk_app_secret: String::new(),
            dingtalk_robot_code: String::new(),
            dingtalk_corp_id: String::new(),
            dingtalk_agent_id: String::new(),
            dingtalk_mcp_url: String::new(),
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
            wechat_enabled: false,
            wechat_gateway_token: String::new(),
            wechat_gateway_port: default_wechat_gateway_port(),
            wechat_bot_token: String::new(),
            wechat_base_url: String::new(),
            wechat_bot_id: String::new(),
            smtp_host: String::new(),
            smtp_port: default_smtp_port(),
            smtp_username: String::new(),
            smtp_password: String::new(),
            imap_host: String::new(),
            imap_port: default_imap_port(),
            smtp_from_name: String::new(),
            email_enabled: false,
            max_iterations: default_max_iterations(),
            auto_compact_input_tokens_threshold: default_auto_compact_input_tokens_threshold(),
            compaction_micro_percent: default_compaction_micro_percent(),
            compaction_auto_percent: default_compaction_auto_percent(),
            compaction_full_percent: default_compaction_full_percent(),
            max_tool_result_tokens: default_max_tool_result_tokens(),
            summary_model: None,
            project_instruction_budget_chars: default_project_instruction_budget_chars(),
            enable_project_instructions: true,
            llm_read_timeout_secs: default_llm_read_timeout_secs(),
            koi_timeout_secs: default_koi_timeout_secs(),
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
            enable_streaming: false,
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
            Self::try_decrypt_field(&store, &mut settings.wecom_bot_secret);
            Self::try_decrypt_field(&store, &mut settings.dingtalk_app_secret);
            Self::try_decrypt_field(&store, &mut settings.dingtalk_robot_code);
            Self::try_decrypt_field(&store, &mut settings.telegram_bot_token);
            Self::try_decrypt_field(&store, &mut settings.matrix_access_token);
            Self::try_decrypt_field(&store, &mut settings.webhook_auth_token);
            Self::try_decrypt_field(&store, &mut settings.wechat_gateway_token);
            Self::try_decrypt_field(&store, &mut settings.wechat_bot_token);
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
            Self::encrypt_field(&store, &mut clone.wecom_bot_secret);
            Self::encrypt_field(&store, &mut clone.dingtalk_app_secret);
            Self::encrypt_field(&store, &mut clone.dingtalk_robot_code);
            Self::encrypt_field(&store, &mut clone.telegram_bot_token);
            Self::encrypt_field(&store, &mut clone.matrix_access_token);
            Self::encrypt_field(&store, &mut clone.webhook_auth_token);
            Self::encrypt_field(&store, &mut clone.wechat_gateway_token);
            Self::encrypt_field(&store, &mut clone.wechat_bot_token);
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
            || is_local_ollama_openai_provider(&self.provider, &self.custom_base_url)
    }

    /// Returns the active API key value to pass to the configured provider.
    pub fn active_api_key(&self) -> &str {
        match self.provider.as_str() {
            "custom" | "ollama"
                if self.openai_api_key.trim().is_empty()
                    && is_local_ollama_openai_provider(&self.provider, &self.custom_base_url) =>
            {
                "ollama"
            }
            "openai" | "custom" | "ollama" => &self.openai_api_key,
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
