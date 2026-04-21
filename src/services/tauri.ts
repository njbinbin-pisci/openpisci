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
  rolling_summary?: string;
  rolling_summary_version?: number;
  total_input_tokens?: number;
  total_output_tokens?: number;
  last_compacted_at?: string | null;
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
  // WeChat (iLink Bot HTTP server)
  wechat_enabled: boolean;
  wechat_gateway_token: string;
  wechat_gateway_port: number;
  wechat_bot_token: string;
  wechat_base_url: string;
  wechat_bot_id: string;
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
  | { type: "interactive_ui"; request_id: string; ui_definition: unknown }
  | {
      type: "context_usage";
      estimated_input_tokens: number;
      total_input_budget: number;
      /** 60% of total_input_budget — proactive compaction fires above this line. */
      trigger_threshold: number;
      cumulative_input_tokens: number;
      cumulative_output_tokens: number;
      rolling_summary_version: number;
      /** Configured auto-compact threshold step (0 = cumulative trigger disabled). */
      auto_compact_threshold: number;
      /** p8 — optional per-layer token attribution (persona/scene/memory/project/
       *  platform_hint/tool_defs/history_text/history_tool_result_full/
       *  history_tool_result_receipt/rolling_summary/state_frame/vision/
       *  request_overhead). Absent when the emitter hasn't computed it. */
      layered_breakdown?: {
        persona: number;
        scene: number;
        memory: number;
        project: number;
        platform_hint: number;
        tool_defs: number;
        history_text: number;
        history_tool_result_full: number;
        history_tool_result_receipt: number;
        rolling_summary: number;
        state_frame: number;
        vision: number;
        request_overhead: number;
      };
    }
  | { type: "done"; total_input_tokens: number; total_output_tokens: number }
  | { type: "cancelled" }
  | { type: "error"; message: string }
  | {
      type: "plan_update";
      items: Array<{
        id: string;
        content: string;
        status: "pending" | "in_progress" | "completed" | "cancelled";
      }>;
    }
  | {
      type: "fish_progress";
      fish_id: string;
      fish_name: string;
      /** 1-based iteration index inside the Fish agent loop */
      iteration: number;
      /** Which tool the Fish is currently calling (null = LLM thinking) */
      tool_name: string | null;
      /** "thinking" | "thinking_text" | "tool_call" | "tool_done" | "done" */
      status: string;
      /** For status="thinking_text": streaming text delta from the Fish LLM */
      text_delta?: string;
    };

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

export const settingsApi = {
  get: () => invoke<Settings>("get_settings"),
  save: (updates: Partial<Settings>) => invoke<Settings>("save_settings", { updates }),
  isConfigured: () => invoke<boolean>("is_configured"),
  getDefaultWorkspace: () => invoke<string>("get_default_workspace"),
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
  send: (sessionId: string, content: string, attachment?: ChatAttachment, clearPlan?: boolean) =>
    invoke<void>("chat_send", { sessionId, content, attachment: attachment ?? null, clearPlan: clearPlan ?? true }),
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
// Interactive UI (chat_ui tool responses)
// ---------------------------------------------------------------------------

export const interactiveApi = {
  respond: (requestId: string, values: Record<string, unknown>) =>
    invoke<void>('respond_interactive_ui', { requestId, values }),
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
// Koi (锦鲤) persistent Agents
// ---------------------------------------------------------------------------

export interface KoiDefinition {
  id: string;
  name: string;
  role: string;
  icon: string;
  color: string;
  system_prompt: string;
  description: string;
  status: string;
  created_at: string;
  updated_at: string;
  /** Optional named LLM provider id. Empty/undefined = use global default. */
  llm_provider_id?: string;
  /** Maximum AgentLoop iterations. 0 = use system default (30). */
  max_iterations: number;
  /** Default single-task timeout in seconds. 0 = inherit from project/system. */
  task_timeout_secs: number;
}

export interface KoiWithStats {
  id: string;
  name: string;
  role: string;
  icon: string;
  color: string;
  system_prompt: string;
  description: string;
  status: string;
  created_at: string;
  updated_at: string;
  memory_count: number;
  todo_count: number;
  active_todo_count: number;
  llm_provider_id?: string;
  /** Maximum AgentLoop iterations. 0 = use system default (30). */
  max_iterations: number;
  /** Default single-task timeout in seconds. 0 = inherit from project/system. */
  task_timeout_secs: number;
}

export interface KoiTodo {
  id: string;
  owner_id: string;
  title: string;
  description: string;
  status: string;
  priority: string;
  assigned_by: string;
  pool_session_id?: string;
  claimed_by?: string;
  claimed_at?: string;
  depends_on?: string;
  blocked_reason?: string;
  result_message_id?: number;
  source_type: string;
  task_timeout_secs: number;
  created_at: string;
  updated_at: string;
}

export interface PoolSession {
  id: string;
  name: string;
  org_spec: string;
  status: string;
  project_dir?: string;
  task_timeout_secs: number;
  last_active_at?: string;
  created_at: string;
  updated_at: string;
}

export interface PoolMessage {
  id: number;
  pool_session_id: string;
  sender_id: string;
  content: string;
  msg_type: string;
  metadata: string;
  todo_id?: string;
  reply_to_message_id?: number;
  event_type?: string;
  created_at: string;
}

export interface KoiPalette {
  colors: [string, string][];
  icons: string[];
}

// ---------------------------------------------------------------------------
// Pool events (canonical `host://pool_event` channel, Phase 1.8)
//
// Kernel type: `pisci_core::host::PoolEvent`. The Rust side serialises each
// variant with `#[serde(tag = "kind", rename_all = "snake_case")]`, so a
// discriminated union keyed on `kind` maps one-to-one with zero custom
// adapter code. Keep these shapes in lock-step with `host.rs`.
// ---------------------------------------------------------------------------

export interface PoolSessionSnapshot {
  id: string;
  name: string;
  status: string;
  project_dir?: string;
  task_timeout_secs: number;
}

export interface PoolMessageSnapshot {
  id: number;
  pool_session_id: string;
  sender_id: string;
  content: string;
  msg_type: string;
  metadata?: unknown;
  todo_id?: string;
  reply_to_message_id?: number;
  event_type?: string;
  created_at: string;
}

export interface TodoSnapshot {
  id: string;
  owner_id: string;
  title: string;
  description: string;
  status: string;
  priority: string;
  assigned_by: string;
  pool_session_id?: string;
  claimed_by?: string;
  depends_on?: string;
  blocked_reason?: string;
  result_message_id?: number;
  source_type: string;
  task_timeout_secs: number;
}

export type TodoChangeAction =
  | "created"
  | "updated"
  | "claimed"
  | "completed"
  | "cancelled"
  | "blocked"
  | "resumed"
  | "replaced";

export interface PoolWaitSummary {
  completed: boolean;
  timed_out: boolean;
  active_todos: number;
  done_todos: number;
  cancelled_todos: number;
  blocked_todos: number;
  latest_messages: string[];
}

export type PoolEvent =
  | { kind: "pool_created"; pool: PoolSessionSnapshot }
  | { kind: "pool_updated"; pool: PoolSessionSnapshot }
  | { kind: "pool_paused"; pool: PoolSessionSnapshot }
  | { kind: "pool_resumed"; pool: PoolSessionSnapshot }
  | { kind: "pool_archived"; pool_id: string }
  | { kind: "message_appended"; pool_id: string; message: PoolMessageSnapshot }
  | {
      kind: "todo_changed";
      pool_id: string;
      action: TodoChangeAction;
      todo: TodoSnapshot;
    }
  | {
      kind: "koi_assigned";
      pool_id: string;
      koi_id: string;
      todo_id: string;
    }
  | {
      kind: "koi_status_changed";
      pool_id: string;
      koi_id: string;
      status: string;
    }
  | {
      kind: "koi_stale_recovered";
      pool_id: string;
      koi_id: string;
      recovered_todo_count: number;
    }
  | { kind: "coordinator_idle"; pool_id: string }
  | {
      kind: "coordinator_completed";
      pool_id: string;
      summary: PoolWaitSummary;
    }
  | {
      kind: "coordinator_timed_out";
      pool_id: string;
      summary: PoolWaitSummary;
    }
  | {
      kind: "fish_progress";
      parent_session_id: string;
      fish_id: string;
      stage: string;
      payload?: unknown;
    };

/** Canonical Tauri channel every `PoolEvent` is published on in addition
 *  to the legacy per-variant channels (`pool_session_updated`,
 *  `pool_message_{id}`, `koi_todo_updated`, ...). */
export const POOL_EVENT_CHANNEL = "host://pool_event";

/** Subscribe to the typed, forward-looking pool-event stream. Prefer this
 *  helper over ad-hoc `listen()` calls on the legacy per-variant channels
 *  when you need to reason about multiple variants at once. */
export function subscribePoolEvents(
  handler: (event: PoolEvent) => void,
): Promise<UnlistenFn> {
  return listen<PoolEvent>(POOL_EVENT_CHANNEL, (e) => handler(e.payload));
}

export const koiApi = {
  list: () => invoke<KoiWithStats[]>("list_kois"),
  get: (id: string) => invoke<KoiDefinition | null>("get_koi", { id }),
  create: (input: {
    name: string;
    role: string;
    icon: string;
    color: string;
    system_prompt: string;
    description: string;
    /** Optional named LLM provider id; empty/undefined = use global default */
    llm_provider_id?: string;
    /** Maximum AgentLoop iterations. 0 = use system default (30). */
    max_iterations?: number;
    /** Default single-task timeout in seconds. 0 = inherit from project/system. */
    task_timeout_secs?: number;
  }) => invoke<KoiDefinition>("create_koi", { input }),
  update: (input: {
    id: string;
    name?: string;
    role?: string;
    icon?: string;
    color?: string;
    system_prompt?: string;
    description?: string;
    /** Pass empty string to clear (use global default); undefined = don't change */
    llm_provider_id?: string;
    /** undefined = don't change; 0 = use system default; n = set to n */
    max_iterations?: number;
    /** undefined = don't change; 0 = inherit; n = set task timeout seconds */
    task_timeout_secs?: number;
  }) => invoke<void>("update_koi", { input }),
  delete: (id: string) => invoke<void>("delete_koi", { id }),
  getDeleteInfo: (id: string) =>
    invoke<{ name: string; icon: string; todo_count: number; memory_count: number; is_busy: boolean }>(
      "get_koi_delete_info",
      { id }
    ),
  setActive: (id: string, active: boolean, force?: boolean) =>
    invoke<void>("set_koi_active", { id, active, force }),
  palette: () => invoke<KoiPalette>("get_koi_palette"),
  listMemories: (koiId: string) =>
    invoke<{ memories: Memory[]; total: number }>("list_memories_for_koi", { koiId }),
  listTodos: (koiId: string) => invoke<KoiTodo[]>("list_koi_todos", { ownerId: koiId }),
};

export const poolApi = {
  listSessions: () => invoke<PoolSession[]>("list_pool_sessions"),
  createSession: (name: string, taskTimeoutSecs?: number) =>
    invoke<PoolSession>("create_pool_session", { name, taskTimeoutSecs }),
  deleteSession: (id: string) => invoke<void>("delete_pool_session", { id }),
  pauseSession: (id: string) => invoke<void>("pause_pool_session", { id }),
  resumeSession: (id: string) => invoke<void>("resume_pool_session", { id }),
  archiveSession: (id: string) => invoke<void>("archive_pool_session", { id }),
  getMessages: (input: { session_id: string; limit?: number; offset?: number }) =>
    invoke<PoolMessage[]>("get_pool_messages", { input }),
  sendMessage: (input: {
    session_id: string;
    sender_id: string;
    content: string;
    msg_type?: string;
    metadata?: string;
  }) => invoke<PoolMessage>("send_pool_message", { input }),
  getOrgSpec: (id: string) => invoke<string>("get_pool_org_spec", { id }),
  updateOrgSpec: (id: string, orgSpec: string) =>
    invoke<void>("update_pool_org_spec", { id, orgSpec }),
  updateConfig: (id: string, taskTimeoutSecs?: number) =>
    invoke<void>("update_pool_session_config", { id, taskTimeoutSecs }),
  dispatchTask: (koiId: string, task: string, poolSessionId?: string, priority?: string, timeoutSecs?: number) =>
    invoke<{ success: boolean; reply: string; result_message_id?: number }>(
      "dispatch_koi_task", { koiId, task, poolSessionId, priority, timeoutSecs }
    ),
  cancelKoiTask: (koiId: string, poolSessionId?: string) =>
    invoke<void>("cancel_koi_task", { koiId, poolSessionId: poolSessionId ?? null }),
  handleMention: (senderId: string, poolSessionId: string, content: string) =>
    invoke<{ success: boolean; reply: string; result_message_id?: number } | null>(
      "handle_pool_mention", { senderId, poolSessionId, content }
    ),
  onMessage: (sessionId: string, handler: (msg: PoolMessage) => void): Promise<UnlistenFn> =>
    listen<PoolMessage>(`pool_message_${sessionId}`, (e) => handler(e.payload)),
};

export const boardApi = {
  listTodos: (ownerId?: string) => invoke<KoiTodo[]>("list_koi_todos", { ownerId }),
  createTodo: (input: {
    owner_id: string;
    title: string;
    description?: string;
    priority?: string;
    assigned_by?: string;
    pool_session_id?: string;
    source_type?: string;
    depends_on?: string;
    task_timeout_secs?: number;
  }) => invoke<KoiTodo>("create_koi_todo", { input }),
  updateTodo: (input: {
    id: string;
    title?: string;
    description?: string;
    status?: string;
    priority?: string;
  }) => invoke<void>("update_koi_todo", { input }),
  claimTodo: (id: string, claimedBy: string) =>
    invoke<void>("claim_koi_todo", { id, claimedBy }),
  completeTodo: (id: string, resultMessageId?: number) =>
    invoke<void>("complete_koi_todo", { id, resultMessageId }),
  resumeTodo: (id: string) => invoke<void>("resume_koi_todo", { id }),
  deleteTodo: (id: string) => invoke<void>("delete_koi_todo", { id }),
  onTodoUpdated: (handler: (data: unknown) => void): Promise<UnlistenFn> =>
    listen("koi_todo_updated", (e) => handler(e.payload)),
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
// Test Runner (multi-agent integration tests)
// ---------------------------------------------------------------------------

export interface TestResult {
  name: string;
  passed: boolean;
  message: string;
  duration_ms: number;
}

export interface TestSuiteResult {
  total: number;
  passed: number;
  failed: number;
  results: TestResult[];
  summary: string;
}

export const testApi = {
  runMultiAgentTests: () => invoke<TestSuiteResult>("run_multi_agent_tests"),
  runCollaborationTrial: () => invoke<CollabTrialStatus>("run_collaboration_trial"),
};

export interface CollabTrialStep {
  name: string;
  koi_name: string;
  task: string;
  success: boolean;
  reply_preview: string;
  duration_ms: number;
}

export interface CollabTrialStatus {
  phase: string;
  pool_id: string;
  koi_ids: string[];
  steps: CollabTrialStep[];
  completed: boolean;
  error: string | null;
}

// ---------------------------------------------------------------------------
// File / Path utilities
// ---------------------------------------------------------------------------

/**
 * Open a local file or directory with the system default application.
 * On Windows, directories are opened with Explorer.exe directly,
 * which is more reliable than shell.open() for folder paths.
 */
export function openPath(path: string): Promise<void> {
  return invoke<void>("open_path", { path });
}

// ---------------------------------------------------------------------------
// WeChat login
// ---------------------------------------------------------------------------

export interface WechatLoginStatus {
  qr_data_url: string | null;
  qrcode_token: string | null;
  message: string;   // "scan_qr" | "wait" | "scaned" | "confirmed" | "connected" | "expired"
  connected: boolean;
  bot_id: string | null;
}

export const wechatApi = {
  startLogin: () => invoke<WechatLoginStatus>("start_wechat_login"),
  pollLogin: (qrcodeToken: string) =>
    invoke<WechatLoginStatus>("poll_wechat_login", { qrcodeToken }),
};

// ---------------------------------------------------------------------------
// System / Diagnostics
// ---------------------------------------------------------------------------

