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
  run_count: number;
  last_run_at?: string;
  next_run_at?: string;
  created_at: string;
}

export interface Settings {
  anthropic_api_key: string;
  openai_api_key: string;
  provider: string;
  model: string;
  custom_base_url: string;
  workspace_root: string;
  language: string;
  max_tokens: number;
  confirm_shell_commands: boolean;
  confirm_file_writes: boolean;
}

export type AgentEventType =
  | { type: "text_delta"; delta: string }
  | { type: "tool_start"; id: string; name: string; input: unknown }
  | { type: "tool_end"; id: string; name: string; result: string; is_error: boolean }
  | { type: "message_commit"; message: unknown }
  | { type: "permission_request"; request_id: string; tool_name: string; tool_input: unknown; description: string }
  | { type: "done"; total_input_tokens: number; total_output_tokens: number }
  | { type: "error"; message: string };

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
  getMessages: (sessionId: string, limit = 100, offset = 0) =>
    invoke<ChatMessage[]>("get_messages", { sessionId, limit, offset }),
};

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

export const chatApi = {
  send: (sessionId: string, content: string) =>
    invoke<void>("chat_send", { sessionId, content }),
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

export const skillsApi = {
  list: () => invoke<{ skills: Skill[]; total: number }>("list_skills"),
  toggle: (skillId: string, enabled: boolean) =>
    invoke<void>("toggle_skill", { skillId, enabled }),
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

export const systemApi = {
  getVmStatus: () =>
    invoke<{ backend: string; available: boolean; description: string }>("get_vm_status"),
};
