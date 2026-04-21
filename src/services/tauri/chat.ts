/**
 * Tauri IPC — chat domain.
 *
 * Sessions, chat turn execution & streaming events, scheduler, IM gateway
 * channels (+ WeChat login handshake), Fish subagent listing, and the LLM
 * collaboration-trial harness.
 *
 * Mirrors Rust-side `src-tauri/src/commands/chat.rs` + `commands/chat/*`.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";

// ---------------------------------------------------------------------------
// Sessions
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
// Chat turns (chat_send, chat_cancel, agent_event_* stream)
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

export const chatApi = {
  send: (sessionId: string, content: string, attachment?: ChatAttachment, clearPlan?: boolean) =>
    invoke<void>("chat_send", { sessionId, content, attachment: attachment ?? null, clearPlan: clearPlan ?? true }),
  cancel: (sessionId: string) =>
    invoke<void>("chat_cancel", { sessionId }),
  onEvent: (sessionId: string, handler: (event: AgentEventType) => void): Promise<UnlistenFn> =>
    listen<AgentEventType>(`agent_event_${sessionId}`, (e) => handler(e.payload)),
};

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

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
// Gateway / IM channels
// ---------------------------------------------------------------------------

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

export const gatewayApi = {
  list: () => invoke<{ channels: ChannelInfo[] }>('list_gateway_channels'),
  connect: () => invoke<{ channels: ChannelInfo[] }>('connect_gateway_channels'),
  disconnect: () => invoke<void>('disconnect_gateway_channels'),
};

// ---------------------------------------------------------------------------
// WeChat login handshake
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
// Fish (小鱼) sub-Agents — read-only listing
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
// Collaboration Trial (LLM-driven debug harness)
// ---------------------------------------------------------------------------

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

export const testApi = {
  runCollaborationTrial: () => invoke<CollabTrialStatus>("run_collaboration_trial"),
};
