/**
 * Tauri IPC — pool domain.
 *
 * Koi (persistent independent agents), Chat Pool (multi-agent chat room),
 * Board (Koi todo list), and the canonical typed `PoolEvent` stream
 * published on `host://pool_event`.
 *
 * Mirrors Rust-side `src-tauri/src/commands/pool.rs` +
 * `commands/pool/{koi,board}.rs`, and `pisci_core::host::PoolEvent`.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";

import type { Memory } from "./config";

// ---------------------------------------------------------------------------
// Koi / Pool / Board entity types
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
// Pool events — canonical `host://pool_event` channel (Phase 1.8)
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

// ---------------------------------------------------------------------------
// Koi (锦鲤) persistent Agents
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Chat Pool (multi-agent chat room)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Board (Kanban view over KoiTodos)
// ---------------------------------------------------------------------------

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
