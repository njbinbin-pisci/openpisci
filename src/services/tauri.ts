/**
 * Tauri IPC service layer — replaces HTTP/WebSocket api.ts.
 * All communication goes through Tauri invoke() and listen().
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface Session {
  source: string;         // "chat" | "im_telegram" | "im_feishu" | ...
  id: string;
  title?: string;
  status: string;
  created_at: string;
  updated_at: string;
  message_count: number;
}

export interface ChatMessage {
  id: string;
  session_id: string;
  role: "user" | "assistant" | "system" | "tool";
  content: string;
  created_at: string;
  /** JSON array of ToolUse ContentBlocks (assistant messages with tool calls) */
  tool_calls_json?: string | null;
  /** JSON array of ToolResult ContentBlocks (user messages carrying tool results) */
  tool_results_json?: string | null;
  /** 1-based conversation turn index */
  turn_index?: number | null;
}

export interface Memory {
  id: string;
  content: string;
  category: string;
  confidence: number;
  source_session_id?: string;
  created_at: string;
  updated_at: string;
}

export interface Skill {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  icon: string;
  config: string;
}

export interface ScheduledTask {
  id: string;
  name: string;
  description?: string;
  cron_expression: string;
  task_prompt: string;
  status: string;
  last_run_status?: string;
  run_count: number;
  last_run_at?: string;
  next_run_at?: string;
  created_at: string;
}

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
  wecom_corp_id: string;
  wecom_agent_secret: string;
  wecom_agent_id: string;
  wecom_enabled: boolean;
  dingtalk_app_key: string;
  dingtalk_app_secret: string;
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
  // WeCom relay inbox file
  wecom_inbox_file: string;
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
  heartbeat_enabled: boolean;
  heartbeat_interval_mins: number;
  heartbeat_prompt: string;
  // Vision / multimodal
  vision_enabled: boolean;
  // SSH Servers
  ssh_servers?: SshServerConfig[];
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

export type ChannelStatus =
  | "Disconnected"
  | "Connecting"
  | "Connected"
  | { Error: string };

export interface ChannelInfo {
  name: string;
  status: ChannelStatus;
  connected_at?: number;
}

export type AgentEventType =
  | { type: "text_segment_start"; iteration: number }
  | { type: "text_delta"; delta: string }
  | { type: "tool_start"; id: string; name: string; input: unknown }
  | { type: "tool_end"; id: string; name: string; result: string; is_error: boolean }
  | { type: "message_commit"; message: unknown }
  | { type: "permission_request"; request_id: string; tool_name: string; tool_input: unknown; description: string }
  | { type: "done"; total_input_tokens: number; total_output_tokens: number }
  | { type: "error"; message: string }
  | {
      type: "fish_progress";
      fish_id: string;
      fish_name: string;
      /** 1-based iteration index inside the Fish agent loop */
      iteration: number;
      /** Which tool the Fish is currently calling (null = LLM thinking) */
      tool_name: string | null;
      /** "thinking" | "tool_call" | "tool_done" | "done" */
      status: string;
    };

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

export const settingsApi = {
  get: () => invoke<Settings>("get_settings"),
  save: (updates: Partial<Settings>) => invoke<Settings>("save_settings", { updates }),
  isConfigured: () => invoke<boolean>("is_configured"),
};

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

export const sessionsApi = {
  create: (title?: string) => invoke<Session>("create_session", { title }),
  list: (limit = 20, offset = 0) =>
    invoke<{ sessions: Session[]; total: number }>("list_sessions", { limit, offset }),
  delete: (sessionId: string) => invoke<void>("delete_session", { sessionId }),
  rename: (sessionId: string, title: string) => invoke<void>("rename_session", { sessionId, title }),
  getMessages: (sessionId: string, limit = 100, offset = 0) =>
    invoke<ChatMessage[]>("get_messages", { sessionId, limit, offset }),
};

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

export interface ChatAttachment {
  /** MIME type, e.g. "image/png", "application/pdf" */
  media_type: string;
  /** Local absolute file path (for non-image files or non-vision models) */
  path?: string;
  /** Base64-encoded file data (for images with vision models) */
  data?: string;
  /** Original filename */
  filename?: string;
}

export const chatApi = {
  send: (sessionId: string, content: string, attachment?: ChatAttachment) =>
    invoke<void>("chat_send", { sessionId, content, attachment: attachment ?? null }),
  cancel: (sessionId: string) =>
    invoke<void>("chat_cancel", { sessionId }),
  onEvent: (sessionId: string, handler: (event: AgentEventType) => void): Promise<UnlistenFn> =>
    listen<AgentEventType>(`agent_event_${sessionId}`, (e) => handler(e.payload)),
};

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

export const memoryApi = {
  list: () => invoke<{ memories: Memory[]; total: number }>("list_memories"),
  add: (content: string, category?: string, confidence?: number) =>
    invoke<Memory>("add_memory", { content, category, confidence }),
  delete: (memoryId: string) => invoke<void>("delete_memory", { memoryId }),
  clear: () => invoke<void>("clear_memories"),
};

// ---------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------

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

export const skillsApi = {
  list: () => invoke<{ skills: Skill[]; total: number }>("list_skills"),
  toggle: (skillId: string, enabled: boolean) =>
    invoke<void>("toggle_skill", { skillId, enabled }),
  catalog: () => invoke<SkillCatalogItem[]>("scan_skill_catalog"),
  install: (source: string) => invoke<SkillCatalogItem>("install_skill", { source }),
  uninstall: (skillName: string) => invoke<void>("uninstall_skill", { skillName }),
  checkCompat: (source: string) =>
    invoke<SkillCompatibilityCheck>("check_skill_compat", { source }),
};

// ---------------------------------------------------------------------------
// ClawHub
// ---------------------------------------------------------------------------

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
// Scheduler
// ---------------------------------------------------------------------------

export const schedulerApi = {
  list: () => invoke<{ tasks: ScheduledTask[]; total: number }>("list_tasks"),
  create: (params: {
    name: string;
    description?: string;
    cron_expression: string;
    task_prompt: string;
  }) => invoke<ScheduledTask>("create_task", params),
  update: (params: {
    task_id: string;
    name?: string;
    cron_expression?: string;
    task_prompt?: string;
    status?: string;
  }) => invoke<void>("update_task", params),
  delete: (taskId: string) => invoke<void>("delete_task", { taskId }),
  runNow: (taskId: string) => invoke<string>("run_task_now", { taskId }),
};

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

export interface RuntimeCheckItem {
  name: string;
  available: boolean;
  version: string | null;
  download_url: string;
  hint: string;
}

export const systemApi = {
  getVmStatus: () =>
    invoke<{ backend: string; available: boolean; description: string }>("get_vm_status"),
  checkRuntimes: () => invoke<RuntimeCheckItem[]>("check_runtimes"),
  setRuntimePath: (runtimeKey: string, exePath: string) =>
    invoke<RuntimeCheckItem[]>("set_runtime_path", { runtimeKey, exePath }),
};

// ---------------------------------------------------------------------------
// Gateway / IM
// ---------------------------------------------------------------------------

export const gatewayApi = {
  list: () => invoke<{ channels: ChannelInfo[] }>('list_gateway_channels'),
  connect: () => invoke<{ channels: ChannelInfo[] }>('connect_gateway_channels'),
  disconnect: () => invoke<void>('disconnect_gateway_channels'),
};

// ---------------------------------------------------------------------------
// Audit Log
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
// Permission
// ---------------------------------------------------------------------------

export const permissionApi = {
  respond: (requestId: string, approved: boolean) =>
    invoke<void>('respond_permission', { requestId, approved }),
};

// ---------------------------------------------------------------------------
// User Tools
// ---------------------------------------------------------------------------

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
// Built-in Tools
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
// Fish (小鱼) sub-Agents
// ---------------------------------------------------------------------------

export interface FishSettingOption {
  value: string;
  label: string;
}

export interface FishSettingDef {
  key: string;
  label: string;
  setting_type: string;
  default: string;
  placeholder: string;
  options: FishSettingOption[];
}

export interface FishAgentConfig {
  system_prompt: string;
  max_iterations: number;
  model: string;
}

/** Where a Fish definition comes from */
export type FishSource = "builtin" | "skill" | "user";

export interface FishDefinition {
  id: string;
  name: string;
  description: string;
  icon: string;
  tools: string[];
  agent: FishAgentConfig;
  settings: FishSettingDef[];
  builtin: boolean;
  /** "builtin" | "skill" | "user" */
  source: FishSource;
}

export const fishApi = {
  list: () => invoke<FishDefinition[]>('list_fish'),
};

// ---------------------------------------------------------------------------
// MCP Servers
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
// Window (minimal mode)
// ---------------------------------------------------------------------------

export const windowApi = {
  enterMinimalMode: () => invoke<void>("enter_minimal_mode"),
  exitMinimalMode: () => invoke<void>("exit_minimal_mode"),
  setOverlayPosition: (x: number, y: number) =>
    invoke<void>("set_overlay_position", { x, y }),
  saveOverlayPosition: (x: number, y: number) =>
    invoke<void>("save_overlay_position", { x, y }),
  setThemeBorder: (theme: "violet" | "gold") =>
    invoke<void>("set_window_theme_border", { theme }),
};

// ---------------------------------------------------------------------------
// System / Diagnostics
// ---------------------------------------------------------------------------

