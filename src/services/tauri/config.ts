/**
 * Tauri IPC — config domain.
 *
 * User-facing configuration & registries: settings, memory, skills (+ ClawHub
 * bridge), MCP servers, builtin & user tool plugins, and audit log.
 *
 * Mirrors Rust-side `src-tauri/src/commands/config/*`.
 */
import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

export interface Settings {
  anthropic_api_key: string;
  openai_api_key: string;
  deepseek_api_key: string;
  qwen_api_key: string;
  minimax_api_key: string;
  zhipu_api_key: string;
  kimi_api_key: string;
  provider: string;
  model: string;
  custom_base_url: string;
  workspace_root: string;
  allow_outside_workspace: boolean;
  language: string;
  max_tokens: number;
  /** Context window size in tokens (input limit). 0 = auto. */
  context_window: number;
  confirm_shell_commands: boolean;
  confirm_file_writes: boolean;
  browser_headless: boolean;
  is_configured?: boolean;
  // IM Gateway
  feishu_app_id: string;
  feishu_app_secret: string;
  feishu_domain: string;
  feishu_enabled: boolean;
  wecom_bot_id: string;
  wecom_bot_secret: string;
  wecom_enabled: boolean;
  dingtalk_app_key: string;
  dingtalk_app_secret: string;
  dingtalk_robot_code: string;
  dingtalk_corp_id: string;
  dingtalk_agent_id: string;
  dingtalk_mcp_url: string;
  dingtalk_enabled: boolean;
  telegram_bot_token: string;
  telegram_enabled: boolean;
  // Slack
  slack_webhook_url: string;
  slack_enabled: boolean;
  // Discord
  discord_webhook_url: string;
  discord_enabled: boolean;
  // Microsoft Teams
  teams_webhook_url: string;
  teams_enabled: boolean;
  // Matrix
  matrix_homeserver: string;
  matrix_access_token: string;
  matrix_room_id: string;
  matrix_enabled: boolean;
  // Generic Webhook
  webhook_outbound_url: string;
  webhook_auth_token: string;
  webhook_enabled: boolean;
  // WeChat (iLink Bot HTTP server)
  wechat_enabled: boolean;
  wechat_gateway_token: string;
  wechat_gateway_port: number;
  wechat_bot_token: string;
  wechat_base_url: string;
  wechat_bot_id: string;
  /** When true, inbound IM messages switch the app into minimal overlay mode. */
  im_auto_minimal_mode: boolean;
  /** IM message handling mode: "queue" or "cancel". */
  im_message_mode: string;
  // Email (SMTP / IMAP)
  smtp_host: string;
  smtp_port: number;
  smtp_username: string;
  smtp_password: string;
  imap_host: string;
  imap_port: number;
  smtp_from_name: string;
  email_enabled: boolean;
  // User tool configs (tool_name → { field: value })
  user_tool_configs: Record<string, Record<string, unknown>>;
  // Builtin tool switches (tool_name -> enabled)
  builtin_tool_enabled: Record<string, boolean>;
  // Agent config
  max_iterations: number;
  auto_compact_input_tokens_threshold: number;
  compaction_micro_percent: number;
  compaction_auto_percent: number;
  compaction_full_percent: number;
  max_tool_result_tokens: number;
  summary_model?: string | null;
  project_instruction_budget_chars: number;
  enable_project_instructions: boolean;
  llm_read_timeout_secs: number;
  koi_timeout_secs: number;
  heartbeat_enabled: boolean;
  heartbeat_interval_mins: number;
  heartbeat_prompt: string;
  // Vision / multimodal
  vision_enabled: boolean;
  /** When true, main-chat LLM responses stream as they arrive. Default false. */
  enable_streaming: boolean;
  // SSH Servers
  ssh_servers?: SshServerConfig[];
  // Named LLM Providers
  llm_providers?: LlmProviderConfig[];
  /** When true, multiple app instances may run simultaneously. Default false. */
  allow_multiple_instances?: boolean;
}

export interface SshServerConfig {
  id: string;
  label: string;
  host: string;
  port: number;
  username: string;
  /** Password — empty string means "unchanged" when saving */
  password: string;
  /** PEM private key — empty string means "unchanged" when saving */
  private_key: string;
}

/** A named LLM provider configuration. Multiple can be stored in Settings.llm_providers. */
export interface LlmProviderConfig {
  id: string;
  label: string;
  /** "anthropic" | "openai" | "deepseek" | "qwen" | "minimax" | "zhipu" | "kimi" | "custom" */
  provider: string;
  model: string;
  /** API key — empty string means "unchanged" when saving */
  api_key: string;
  /** Custom base URL (only used when provider = "custom") */
  base_url: string;
  /** Max output tokens; 0 = inherit from global settings */
  max_tokens: number;
}

export const settingsApi = {
  get: () => invoke<Settings>("get_settings"),
  save: (updates: Partial<Settings>) => invoke<Settings>("save_settings", { updates }),
  isConfigured: () => invoke<boolean>("is_configured"),
  getDefaultWorkspace: () => invoke<string>("get_default_workspace"),
};

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

export interface Memory {
  id: string;
  content: string;
  category: string;
  confidence: number;
  source_session_id?: string;
  created_at: string;
  updated_at: string;
}

export const memoryApi = {
  list: () => invoke<{ memories: Memory[]; total: number }>("list_memories"),
  add: (content: string, category?: string, confidence?: number) =>
    invoke<Memory>("add_memory", { content, category, confidence }),
  delete: (memoryId: string) => invoke<void>("delete_memory", { memoryId }),
  clear: () => invoke<void>("clear_memories"),
};

// ---------------------------------------------------------------------------
// Skills (+ ClawHub registry bridge)
// ---------------------------------------------------------------------------

export interface Skill {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  icon: string;
  config: string;
}

export interface SkillCatalogItem {
  name: string;
  description: string;
  version: string;
  source: string;
  tools: string[];
  dependencies: string[];
  permissions: string[];
  platform: string[];
}

export interface SkillCompatibilityCheck {
  compatible: boolean;
  issues: string[];
  warnings: string[];
}

export interface SyncSkillsResult {
  synced: number;
  already_registered: number;
  errors: string[];
}

export const skillsApi = {
  list: () => invoke<{ skills: Skill[]; total: number }>("list_skills"),
  toggle: (skillId: string, enabled: boolean) =>
    invoke<void>("toggle_skill", { skillId, enabled }),
  catalog: () => invoke<SkillCatalogItem[]>("scan_skill_catalog"),
  install: (source: string) => invoke<SkillCatalogItem>("install_skill", { source }),
  uninstall: (skillName: string) => invoke<void>("uninstall_skill", { skillName }),
  checkCompat: (source: string) =>
    invoke<SkillCompatibilityCheck>("check_skill_compat", { source }),
  syncFromDisk: () => invoke<SyncSkillsResult>("sync_skills_from_disk"),
};

export interface ClawHubSkill {
  slug: string;
  name: string;
  description: string;
  version: string;
  author: string;
  downloads: number;
  stars: number;
  tags: string[];
  skill_url: string | null;
  zip_url: string | null;
  /** Platform requirements from SKILL.md (empty = all platforms) */
  platform: string[];
  /** Runtime dependencies from SKILL.md */
  dependencies: string[];
  /** null = not yet checked, true = compatible, false = incompatible */
  compatible: boolean | null;
  /** Populated when compatible === false */
  compat_issues: string[];
}

export interface ClawHubSearchResult {
  items: ClawHubSkill[];
  total: number;
  query: string;
}

export const clawHubApi = {
  search: (query: string, limit?: number) =>
    invoke<ClawHubSearchResult>("clawhub_search", { query, limit }),
  install: (slug: string, version?: string) =>
    invoke<SkillCatalogItem>("clawhub_install", { slug, version }),
};

// ---------------------------------------------------------------------------
// Audit log
// ---------------------------------------------------------------------------

export interface AuditEntry {
  id: string;
  session_id: string;
  timestamp: string;
  tool_name: string;
  action: string;
  input_summary?: string;
  result_summary?: string;
  is_error: boolean;
}

export const auditApi = {
  list: (params?: { session_id?: string; tool_name?: string; limit?: number; offset?: number }) =>
    invoke<AuditEntry[]>('get_audit_log', {
      sessionId: params?.session_id,
      toolName: params?.tool_name,
      limit: params?.limit ?? 50,
      offset: params?.offset ?? 0,
    }),
  clear: (sessionId?: string) => invoke<void>('clear_audit_log', { sessionId }),
};

// ---------------------------------------------------------------------------
// User tools (3rd-party tool plugins)
// ---------------------------------------------------------------------------

export interface ConfigFieldSchema {
  type: "string" | "number" | "boolean" | "password";
  label?: string;
  default?: unknown;
  description?: string;
  placeholder?: string;
}

export interface UserToolInfo {
  name: string;
  description: string;
  version: string;
  author: string;
  runtime: string;
  entrypoint: string;
  input_schema: unknown;
  config_schema: Record<string, ConfigFieldSchema>;
  has_config: boolean;
}

export const userToolsApi = {
  list: () => invoke<UserToolInfo[]>('list_user_tools'),
  install: (source: string) => invoke<UserToolInfo>('install_user_tool', { source }),
  uninstall: (toolName: string) => invoke<void>('uninstall_user_tool', { toolName }),
  saveConfig: (toolName: string, config: Record<string, unknown>) =>
    invoke<void>('save_user_tool_config', { toolName, config }),
  getConfig: (toolName: string) =>
    invoke<Record<string, unknown>>('get_user_tool_config', { toolName }),
};

// ---------------------------------------------------------------------------
// Built-in tools
// ---------------------------------------------------------------------------

export interface BuiltinToolInfo {
  name: string;
  description: string;
  icon: string;
  windows_only: boolean;
}

export const builtinToolsApi = {
  list: () => invoke<BuiltinToolInfo[]>('list_builtin_tools'),
  triggerHeartbeat: () => invoke<void>('trigger_heartbeat'),
};

// ---------------------------------------------------------------------------
// MCP servers
// ---------------------------------------------------------------------------

export interface McpServerConfig {
  name: string;
  transport: "stdio" | "sse";
  command: string;
  args: string[];
  url: string;
  env: Record<string, string>;
  enabled: boolean;
}

export interface McpToolInfo {
  name: string;
  description?: string;
  inputSchema?: unknown;
}

export interface McpTestResult {
  success: boolean;
  tools: McpToolInfo[];
  error?: string;
}

export const mcpApi = {
  list: () => invoke<McpServerConfig[]>("list_mcp_servers"),
  save: (servers: McpServerConfig[]) => invoke<void>("save_mcp_servers", { servers }),
  test: (config: McpServerConfig) => invoke<McpTestResult>("test_mcp_server", { config }),
};

// ---------------------------------------------------------------------------
// Enterprise capabilities
// ---------------------------------------------------------------------------

export interface EnterpriseCapabilityTemplate {
  platform: string;
  title: string;
  description: string;
  supported: boolean;
  mcp_server_name: string;
}

export interface EnterpriseCapabilityStatus {
  platform: string;
  supported: boolean;
  configured: boolean;
  enabled: boolean;
  mcp_configured: boolean;
  mcp_enabled: boolean;
  mcp_server_name: string;
  missing_credentials: string[];
  message: string;
}

export interface EnterpriseCapabilityTestResult {
  status: EnterpriseCapabilityStatus;
  success: boolean;
  tools: McpToolInfo[];
  error?: string;
  diagnostics: string[];
}

export const enterpriseCapabilityApi = {
  listTemplates: () =>
    invoke<EnterpriseCapabilityTemplate[]>("list_enterprise_capability_templates"),
  status: (platform: string) =>
    invoke<EnterpriseCapabilityStatus>("get_enterprise_capability_status", { platform }),
  enable: (platform: string) =>
    invoke<EnterpriseCapabilityStatus>("enable_enterprise_capability", { platform }),
  test: (platform: string) =>
    invoke<EnterpriseCapabilityTestResult>("test_enterprise_capability", { platform }),
};
