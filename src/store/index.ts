import { configureStore, createSlice, PayloadAction } from "@reduxjs/toolkit";
import type {
  Session, ChatMessage, Memory, Skill, ScheduledTask, Settings,
  KoiWithStats, KoiTodo, PoolSession, PoolMessage,
} from "../services/tauri";

// ---------------------------------------------------------------------------
// Sessions slice
// ---------------------------------------------------------------------------

interface SessionsState {
  sessions: Session[];
  activeSessionId: string | null;
  loading: boolean;
  error: string | null;
}

const sessionsSlice = createSlice({
  name: "sessions",
  initialState: { sessions: [], activeSessionId: null, loading: false, error: null } as SessionsState,
  reducers: {
    setSessions: (state, action: PayloadAction<Session[]>) => {
      state.sessions = action.payload;
    },
    addSession: (state, action: PayloadAction<Session>) => {
      state.sessions.unshift(action.payload);
    },
    removeSession: (state, action: PayloadAction<string>) => {
      state.sessions = state.sessions.filter((s) => s.id !== action.payload);
      if (state.activeSessionId === action.payload) {
        state.activeSessionId = state.sessions[0]?.id ?? null;
      }
    },
    updateSessionTitle: (state, action: PayloadAction<{ id: string; title: string }>) => {
      const s = state.sessions.find((s) => s.id === action.payload.id);
      if (s) s.title = action.payload.title;
    },
    setActiveSession: (state, action: PayloadAction<string | null>) => {
      state.activeSessionId = action.payload;
    },
    setLoading: (state, action: PayloadAction<boolean>) => {
      state.loading = action.payload;
    },
    setError: (state, action: PayloadAction<string | null>) => {
      state.error = action.payload;
    },
  },
});

// ---------------------------------------------------------------------------
// Chat slice
// ---------------------------------------------------------------------------

export interface ToolStep {
  id: string;
  name: string;
  input: unknown;
  result?: string;
  isError?: boolean;
  /** false = still running, true = finished */
  completed: boolean;
  /** whether the detail panel is expanded */
  expanded: boolean;
  /** If this step is a Fish sub-agent delegation, track its progress */
  fishProgress?: {
    fishId: string;
    fishName: string;
    iteration: number;
    toolName: string | null;
    status: string;
    /** Accumulated streaming text from the Fish LLM (last ~200 chars shown in badge) */
    thinkingText?: string;
  };
}

export interface PlanTodoItem {
  id: string;
  content: string;
  status: "pending" | "in_progress" | "completed" | "cancelled";
}

/** Per-session streaming state for the current agent turn */
export interface StreamingState {
  /** The text currently being streamed in the visible bubble */
  current: string;
  /** Text from the previous segment that is animating out (slide-up exit) */
  exiting: string | null;
  /** Incremented each time a new segment starts — used as React key to re-trigger enter animation */
  segmentId: number;
  /** Character offset in `current` where the last segment started.
   *  Used by freezeStreaming to split intermediate steps from the final summary. */
  lastSegmentStart: number;
}

interface ChatState {
  messagesBySession: Record<string, ChatMessage[]>;
  /** Replaces the old flat `streamingText` — supports segment-based bubble replacement */
  streaming: Record<string, StreamingState>;
  /** Tool steps for the current (or most recent) agent turn, keyed by sessionId.
   *  Steps are KEPT after completion so the user can review them.
   *  Cleared automatically when the next agent turn begins. */
  toolSteps: Record<string, ToolStep[]>;
  /** Tracks whether the last agent turn has finished — used to auto-clear steps on next turn start */
  toolStepsTurnDone: Record<string, boolean>;
  planBySession: Record<string, PlanTodoItem[]>;
  isRunning: Record<string, boolean>;
  /** Frozen streaming text from the last completed agent turn, keyed by sessionId.
   *  Used to replace the multi-bubble DB reload with a single merged bubble.
   *  Cleared when the next user message is sent. */
  frozenBubble: Record<string, string>;
}

const chatSlice = createSlice({
  name: "chat",
  initialState: {
    messagesBySession: {},
    streaming: {},
    toolSteps: {},
    toolStepsTurnDone: {},
    planBySession: {},
    isRunning: {},
    frozenBubble: {},
  } as ChatState,
  reducers: {
    setMessages: (state, action: PayloadAction<{ sessionId: string; messages: ChatMessage[] }>) => {
      // Replace messages, discarding any optimistic placeholders
      state.messagesBySession[action.payload.sessionId] = action.payload.messages;
    },
    appendMessage: (state, action: PayloadAction<{ sessionId: string; message: ChatMessage }>) => {
      const { sessionId, message } = action.payload;
      if (!state.messagesBySession[sessionId]) {
        state.messagesBySession[sessionId] = [];
      }
      state.messagesBySession[sessionId].push(message);
    },
    /** Trim oldest messages beyond capacity for a session, marking hasMore in the component. */
    trimChatMessages: (state, action: PayloadAction<{ sessionId: string; capacity: number }>) => {
      const { sessionId, capacity } = action.payload;
      const msgs = state.messagesBySession[sessionId];
      if (msgs && msgs.length > capacity) {
        state.messagesBySession[sessionId] = msgs.slice(-capacity);
      }
    },
    /** Prepend older messages fetched from the server (for scroll-up pagination). */
    prependChatMessages: (state, action: PayloadAction<{ sessionId: string; messages: ChatMessage[] }>) => {
      const { sessionId, messages } = action.payload;
      const existing = state.messagesBySession[sessionId] ?? [];
      const existingIds = new Set(existing.map((m) => m.id));
      const newOnes = messages.filter((m) => !existingIds.has(m.id));
      state.messagesBySession[sessionId] = [...newOnes, ...existing];
    },
    /** Remove all optimistic placeholder messages (id starts with "optimistic_") for a session */
    removeOptimisticMessages: (state, action: PayloadAction<string>) => {
      const msgs = state.messagesBySession[action.payload];
      if (msgs) {
        state.messagesBySession[action.payload] = msgs.filter(
          (m) => !m.id.startsWith("optimistic_")
        );
      }
    },
    appendDelta: (state, action: PayloadAction<{ sessionId: string; delta: string }>) => {
      const { sessionId, delta } = action.payload;
      if (!state.streaming[sessionId]) {
        state.streaming[sessionId] = { current: "", exiting: null, segmentId: 0, lastSegmentStart: 0 };
      }
      state.streaming[sessionId].current += delta;
    },
    /** Called when a new LLM segment starts — keep current text visible, just mark segment boundary */
    startNewSegment: (state, action: PayloadAction<string>) => {
      const sid = action.payload;
      const s = state.streaming[sid];
      if (s) {
        // Keep current text; new deltas will append to it (single bubble, no exit animation).
        // Record where this segment starts so freezeStreaming can split off the final summary.
        s.lastSegmentStart = s.current.length;
        s.segmentId = (s.segmentId ?? 0) + 1;
      } else {
        state.streaming[sid] = { current: "", exiting: null, segmentId: 0, lastSegmentStart: 0 };
      }
    },
    /** No-op kept for API compatibility — exiting animation is removed */
    clearExiting: (state, action: PayloadAction<string>) => {
      const s = state.streaming[action.payload];
      if (s) s.exiting = null;
    },
    clearStreaming: (state, action: PayloadAction<string>) => {
      delete state.streaming[action.payload];
    },
    /** Called on `done`: snapshot intermediate streaming text into frozenBubble, then clear streaming.
     *  The frozen text (everything before the last segment) becomes the collapsed intermediate-steps
     *  bubble. The last segment is the final summary and will be shown separately from DB data. */
    freezeStreaming: (state, action: PayloadAction<string>) => {
      const sid = action.payload;
      const s = state.streaming[sid];
      if (s) {
        // Split: everything before lastSegmentStart = intermediate steps (frozen bubble).
        //        lastSegmentStart..end = final summary (shown separately from DB).
        const intermediateText = s.lastSegmentStart > 0
          ? s.current.slice(0, s.lastSegmentStart).trimEnd()
          : "";
        if (intermediateText.trim()) {
          state.frozenBubble[sid] = intermediateText;
        }
        // If there were no segment boundaries (single-segment run), store the full text.
        // setMessagesWithFrozen will deduplicate against the DB summary message.
        else if (s.current.trim()) {
          state.frozenBubble[sid] = s.current;
        }
      }
      delete state.streaming[sid];
    },
    /** Replace messages for a session, collapsing the last agent turn into a single bubble.
     *
     *  If a frozenBubble exists for this session (set by freezeStreaming during a recent run),
     *  the last agent turn is collapsed: intermediate steps → one bubble, final summary → separate bubble.
     *  If no frozenBubble exists (other sessions, old history, after app restart), messages are
     *  set as-is from DB — no collapsing, no reconstruction.
     *
     *  Result when frozenBubble present:
     *   [...history before last user msg]
     *   [single collapsed bubble — intermediate steps (frozenBubble content)]
     *   [final summary bubble — DB lastAssistant, only if different from frozenBubble]
     *   [...chat_ui interactive cards]
     */
    setMessagesWithFrozen: (state, action: PayloadAction<{ sessionId: string; messages: ChatMessage[] }>) => {
      const { sessionId, messages } = action.payload;

      // Find the index of the last real (non-optimistic) user message to determine the turn boundary.
      let turnStart = messages.length;
      for (let i = messages.length - 1; i >= 0; i--) {
        if (messages[i].role === "user" && !messages[i].id.startsWith("optimistic_")) {
          turnStart = i + 1;
          break;
        }
      }
      const before = messages.slice(0, turnStart);
      const agentMessages = messages.slice(turnStart);

      // Only use frozenBubble if it was explicitly set during a recent streaming run.
      // Never auto-reconstruct from DB — that would collapse all history into one bubble.
      const frozen = state.frozenBubble[sessionId];
      if (!frozen) {
        // No frozenBubble for this session — show raw DB messages as-is.
        state.messagesBySession[sessionId] = messages;
        return;
      }

      // Build the single collapsed bubble for intermediate steps.
      const synthetic: ChatMessage = {
        id: `frozen_${sessionId}`,
        session_id: sessionId,
        role: "assistant",
        content: frozen,
        created_at: agentMessages[0]?.created_at ?? new Date().toISOString(),
      };

      // Find the last persisted assistant message with text and no tool calls.
      // This is the final summary — show it as a separate bubble after the collapsed
      // intermediate-steps bubble. Skip it only if it is identical to the frozen text
      // (happens on single-segment runs where there were no intermediate tool steps).
      const lastAssistant = [...agentMessages]
        .reverse()
        .find((m) => m.role === "assistant" && m.content.trim() && !m.tool_calls_json);
      const summaryBubble: ChatMessage[] =
        lastAssistant && lastAssistant.content.trim() !== frozen.trim()
          ? [lastAssistant]
          : [];

      // Keep any chat_ui tool-call messages (interactive cards) from the agent turn as-is.
      const chatUiMessages = agentMessages.filter(
        (m) => m.role === "assistant" && m.tool_calls_json &&
          (() => {
            try {
              const calls = JSON.parse(m.tool_calls_json!);
              return Array.isArray(calls) && calls.some((c: { name: string }) => c.name === "chat_ui");
            } catch { return false; }
          })()
      );
      state.messagesBySession[sessionId] = [...before, synthetic, ...summaryBubble, ...chatUiMessages];
    },
    /** Clear the frozen bubble for a session (called when the next user message is sent). */
    clearFrozenBubble: (state, action: PayloadAction<string>) => {
      delete state.frozenBubble[action.payload];
    },
    /** Add a pending tool step when execution starts.
     *  If the previous turn is marked done, clear old steps first (new turn). */
    addToolStep: (state, action: PayloadAction<{ sessionId: string; id: string; name: string; input: unknown }>) => {
      const { sessionId, id, name, input } = action.payload;
      if (state.toolStepsTurnDone[sessionId]) {
        state.toolSteps[sessionId] = [];
        state.toolStepsTurnDone[sessionId] = false;
      }
      if (!state.toolSteps[sessionId]) state.toolSteps[sessionId] = [];
      state.toolSteps[sessionId].push({ id, name, input, completed: false, expanded: true });
    },
    /** Mark a tool step as completed (with result). Step stays visible. */
    completeToolStep: (state, action: PayloadAction<{ sessionId: string; id: string; result: string; isError: boolean }>) => {
      const { sessionId, id, result, isError } = action.payload;
      const step = state.toolSteps[sessionId]?.find((s) => s.id === id);
      if (step) {
        step.result = result;
        step.isError = isError;
        step.completed = true;
        // Collapse finished steps automatically to save space (user can expand)
        step.expanded = false;
      }
    },
    /** Toggle expand/collapse for a step */
    toggleToolStep: (state, action: PayloadAction<{ sessionId: string; id: string }>) => {
      const { sessionId, id } = action.payload;
      const step = state.toolSteps[sessionId]?.find((s) => s.id === id);
      if (step) step.expanded = !step.expanded;
    },
    /** Update the Fish progress on the call_fish tool step */
    updateFishProgress: (state, action: PayloadAction<{
      sessionId: string;
      fishId: string;
      fishName: string;
      iteration: number;
      toolName: string | null;
      status: string;
      textDelta?: string;
    }>) => {
      const { sessionId, fishId, fishName, iteration, toolName, status, textDelta } = action.payload;
      const steps = state.toolSteps[sessionId];
      if (!steps) return;
      // Find the call_fish step for this fish (most recent one)
      const step = [...steps].reverse().find((s) => s.name === "call_fish");
      if (step) {
        if (status === "thinking_text" && textDelta) {
          // Accumulate streaming text, keep last 200 chars to avoid unbounded growth
          const prev = step.fishProgress?.thinkingText ?? "";
          const next = prev + textDelta;
          step.fishProgress = {
            ...(step.fishProgress ?? { fishId, fishName, iteration, toolName: null, status: "thinking" }),
            thinkingText: next.length > 200 ? next.slice(-200) : next,
          };
        } else {
          const prevThinking = status === "thinking" ? "" : (step.fishProgress?.thinkingText ?? "");
          step.fishProgress = { fishId, fishName, iteration, toolName, status, thinkingText: prevThinking };
          if (status === "done") {
            step.completed = true;
            step.expanded = false;
          }
        }
      }
    },
    setPlan: (state, action: PayloadAction<{ sessionId: string; items: PlanTodoItem[] }>) => {
      state.planBySession[action.payload.sessionId] = action.payload.items;
    },
    clearPlan: (state, action: PayloadAction<string>) => {
      delete state.planBySession[action.payload];
    },
    /** Clear all tool steps when a new user message is sent */
    clearToolSteps: (state, action: PayloadAction<string>) => {
      delete state.toolSteps[action.payload];
      delete state.toolStepsTurnDone[action.payload];
    },
    setRunning: (state, action: PayloadAction<{ sessionId: string; running: boolean }>) => {
      state.isRunning[action.payload.sessionId] = action.payload.running;
      if (!action.payload.running) {
        // Mark turn as done so next tool_start will clear these steps
        state.toolStepsTurnDone[action.payload.sessionId] = true;
      }
    },
  },
});

// ---------------------------------------------------------------------------
// Memory slice
// ---------------------------------------------------------------------------

interface MemoryState {
  memories: Memory[];
  loading: boolean;
}

const memorySlice = createSlice({
  name: "memory",
  initialState: { memories: [], loading: false } as MemoryState,
  reducers: {
    setMemories: (state, action: PayloadAction<Memory[]>) => {
      state.memories = action.payload;
    },
    addMemory: (state, action: PayloadAction<Memory>) => {
      state.memories.unshift(action.payload);
    },
    removeMemory: (state, action: PayloadAction<string>) => {
      state.memories = state.memories.filter((m) => m.id !== action.payload);
    },
    setLoading: (state, action: PayloadAction<boolean>) => {
      state.loading = action.payload;
    },
  },
});

// ---------------------------------------------------------------------------
// Skills slice
// ---------------------------------------------------------------------------

interface SkillsState {
  skills: Skill[];
  loading: boolean;
}

const skillsSlice = createSlice({
  name: "skills",
  initialState: { skills: [], loading: false } as SkillsState,
  reducers: {
    setSkills: (state, action: PayloadAction<Skill[]>) => {
      state.skills = action.payload;
    },
    toggleSkill: (state, action: PayloadAction<{ id: string; enabled: boolean }>) => {
      const skill = state.skills.find((s) => s.id === action.payload.id);
      if (skill) skill.enabled = action.payload.enabled;
    },
  },
});

// ---------------------------------------------------------------------------
// Scheduler slice
// ---------------------------------------------------------------------------

interface SchedulerState {
  tasks: ScheduledTask[];
  loading: boolean;
}

const schedulerSlice = createSlice({
  name: "scheduler",
  initialState: { tasks: [], loading: false } as SchedulerState,
  reducers: {
    setTasks: (state, action: PayloadAction<ScheduledTask[]>) => {
      state.tasks = action.payload;
    },
    addTask: (state, action: PayloadAction<ScheduledTask>) => {
      state.tasks.unshift(action.payload);
    },
    removeTask: (state, action: PayloadAction<string>) => {
      state.tasks = state.tasks.filter((t) => t.id !== action.payload);
    },
  },
});

// ---------------------------------------------------------------------------
// Settings slice
// ---------------------------------------------------------------------------

interface SettingsState {
  settings: Settings | null;
  isConfigured: boolean;
  showOnboarding: boolean;
}

const settingsSlice = createSlice({
  name: "settings",
  initialState: { settings: null, isConfigured: false, showOnboarding: false } as SettingsState,
  reducers: {
    setSettings: (state, action: PayloadAction<Settings>) => {
      state.settings = action.payload;
    },
    setConfigured: (state, action: PayloadAction<boolean>) => {
      state.isConfigured = action.payload;
    },
    setShowOnboarding: (state, action: PayloadAction<boolean>) => {
      state.showOnboarding = action.payload;
    },
  },
});

// ---------------------------------------------------------------------------
// Koi slice
// ---------------------------------------------------------------------------

interface KoiState {
  kois: KoiWithStats[];
  loading: boolean;
}

const koiSlice = createSlice({
  name: "koi",
  initialState: { kois: [], loading: false } as KoiState,
  reducers: {
    setKois: (state, action: PayloadAction<KoiWithStats[]>) => {
      state.kois = action.payload;
    },
    addKoi: (state, action: PayloadAction<KoiWithStats>) => {
      state.kois.push(action.payload);
    },
    removeKoi: (state, action: PayloadAction<string>) => {
      state.kois = state.kois.filter((k) => k.id !== action.payload);
    },
    updateKoiInList: (state, action: PayloadAction<Partial<KoiWithStats> & { id: string }>) => {
      const idx = state.kois.findIndex((k) => k.id === action.payload.id);
      if (idx >= 0) state.kois[idx] = { ...state.kois[idx], ...action.payload };
    },
    setLoading: (state, action: PayloadAction<boolean>) => {
      state.loading = action.payload;
    },
  },
});

// ---------------------------------------------------------------------------
// Pool (Chat Pool) slice
// ---------------------------------------------------------------------------

/** Default capacity of pool messages kept in memory per session.
 *  The component manages the actual capacity (starts at this value, grows on lazy-load). */
export const POOL_DEFAULT_CAPACITY = 100;

export type PondSubTab = "kois" | "pool" | "board";

interface PoolState {
  sessions: PoolSession[];
  activeSessionId: string | null;
  messagesBySession: Record<string, PoolMessage[]>;
  /** Whether there are older messages on the server not yet loaded, keyed by sessionId */
  hasMoreBySession: Record<string, boolean>;
  loading: boolean;
}

const poolSlice = createSlice({
  name: "pool",
  initialState: { sessions: [], activeSessionId: null, messagesBySession: {}, hasMoreBySession: {}, loading: false } as PoolState,
  reducers: {
    setPoolSessions: (state, action: PayloadAction<PoolSession[]>) => {
      state.sessions = action.payload;
      if (state.activeSessionId && !action.payload.some(s => s.id === state.activeSessionId)) {
        state.activeSessionId = action.payload[0]?.id ?? null;
        // clean up stale message cache
        const validIds = new Set(action.payload.map(s => s.id));
        for (const key of Object.keys(state.messagesBySession)) {
          if (!validIds.has(key)) {
            delete state.messagesBySession[key];
            delete state.hasMoreBySession[key];
          }
        }
      }
    },
    addPoolSession: (state, action: PayloadAction<PoolSession>) => {
      state.sessions.unshift(action.payload);
    },
    removePoolSession: (state, action: PayloadAction<string>) => {
      state.sessions = state.sessions.filter((s) => s.id !== action.payload);
      delete state.messagesBySession[action.payload];
      delete state.hasMoreBySession[action.payload];
      if (state.activeSessionId === action.payload) {
        state.activeSessionId = state.sessions[0]?.id ?? null;
      }
    },
    updatePoolSessionStatus: (state, action: PayloadAction<{ id: string; status: string }>) => {
      const s = state.sessions.find((s) => s.id === action.payload.id);
      if (s) s.status = action.payload.status;
    },
    setActivePoolSession: (state, action: PayloadAction<string | null>) => {
      state.activeSessionId = action.payload;
    },
    setPoolMessages: (state, action: PayloadAction<{ sessionId: string; messages: PoolMessage[]; hasMore?: boolean }>) => {
      state.messagesBySession[action.payload.sessionId] = action.payload.messages;
      if (action.payload.hasMore !== undefined) {
        state.hasMoreBySession[action.payload.sessionId] = action.payload.hasMore;
      }
    },
    /** Prepend older messages fetched from the server (for scroll-up pagination) */
    prependPoolMessages: (state, action: PayloadAction<{ sessionId: string; messages: PoolMessage[]; hasMore: boolean }>) => {
      const { sessionId, messages, hasMore } = action.payload;
      const existing = state.messagesBySession[sessionId] ?? [];
      const existingIds = new Set(existing.map((m) => m.id));
      const newOnes = messages.filter((m) => !existingIds.has(m.id));
      state.messagesBySession[sessionId] = [...newOnes, ...existing];
      state.hasMoreBySession[sessionId] = hasMore;
    },
    appendPoolMessage: (state, action: PayloadAction<PoolMessage>) => {
      const sid = action.payload.pool_session_id;
      if (!state.messagesBySession[sid]) state.messagesBySession[sid] = [];
      const exists = state.messagesBySession[sid].some((m) => m.id === action.payload.id);
      if (!exists) {
        state.messagesBySession[sid].push(action.payload);
        // Trimming is handled by the component which knows the current capacity.
      }
    },
    /** Trim the oldest messages for a session to the given capacity, marking hasMore if trimmed. */
    trimPoolMessages: (state, action: PayloadAction<{ sessionId: string; capacity: number }>) => {
      const { sessionId, capacity } = action.payload;
      const msgs = state.messagesBySession[sessionId];
      if (msgs && msgs.length > capacity) {
        state.messagesBySession[sessionId] = msgs.slice(-capacity);
        state.hasMoreBySession[sessionId] = true;
      }
    },
    setLoading: (state, action: PayloadAction<boolean>) => {
      state.loading = action.payload;
    },
  },
});

// ---------------------------------------------------------------------------
// Board slice
// ---------------------------------------------------------------------------

interface BoardState {
  todos: KoiTodo[];
  filterOwnerId: string | null;
  filterPriority: string | null;
  filterSessionId: string | null;
  loading: boolean;
}

const boardSlice = createSlice({
  name: "board",
  initialState: { todos: [], filterOwnerId: null, filterPriority: null, filterSessionId: null, loading: false } as BoardState,
  reducers: {
    setTodos: (state, action: PayloadAction<KoiTodo[]>) => {
      state.todos = action.payload;
    },
    addTodo: (state, action: PayloadAction<KoiTodo>) => {
      state.todos.unshift(action.payload);
    },
    removeTodo: (state, action: PayloadAction<string>) => {
      state.todos = state.todos.filter((t) => t.id !== action.payload);
    },
    updateTodoInList: (state, action: PayloadAction<Partial<KoiTodo> & { id: string }>) => {
      const idx = state.todos.findIndex((t) => t.id === action.payload.id);
      if (idx >= 0) state.todos[idx] = { ...state.todos[idx], ...action.payload };
    },
    setFilterOwnerId: (state, action: PayloadAction<string | null>) => {
      state.filterOwnerId = action.payload;
    },
    setFilterPriority: (state, action: PayloadAction<string | null>) => {
      state.filterPriority = action.payload;
    },
    setFilterSessionId: (state, action: PayloadAction<string | null>) => {
      state.filterSessionId = action.payload;
    },
    setLoading: (state, action: PayloadAction<boolean>) => {
      state.loading = action.payload;
    },
  },
});

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

export const store = configureStore({
  reducer: {
    sessions: sessionsSlice.reducer,
    chat: chatSlice.reducer,
    memory: memorySlice.reducer,
    skills: skillsSlice.reducer,
    scheduler: schedulerSlice.reducer,
    settings: settingsSlice.reducer,
    koi: koiSlice.reducer,
    pool: poolSlice.reducer,
    board: boardSlice.reducer,
  },
});

export type RootState = ReturnType<typeof store.getState>;
export type AppDispatch = typeof store.dispatch;

export const sessionsActions = sessionsSlice.actions;
export const chatActions = chatSlice.actions;
export const memoryActions = memorySlice.actions;
export const skillsActions = skillsSlice.actions;
export const schedulerActions = schedulerSlice.actions;
export const settingsActions = settingsSlice.actions;
export const koiActions = koiSlice.actions;
export const poolActions = poolSlice.actions;
export const boardActions = boardSlice.actions;
