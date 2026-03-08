import { configureStore, createSlice, PayloadAction } from "@reduxjs/toolkit";
import type { Session, ChatMessage, Memory, Skill, ScheduledTask, Settings } from "../services/tauri";

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
  };
}

/** Per-session streaming state for the current agent turn */
export interface StreamingState {
  /** The text currently being streamed in the visible bubble */
  current: string;
  /** Text from the previous segment that is animating out (slide-up exit) */
  exiting: string | null;
  /** Incremented each time a new segment starts — used as React key to re-trigger enter animation */
  segmentId: number;
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
  isRunning: Record<string, boolean>;
}

const chatSlice = createSlice({
  name: "chat",
  initialState: {
    messagesBySession: {},
    streaming: {},
    toolSteps: {},
    toolStepsTurnDone: {},
    isRunning: {},
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
        state.streaming[sessionId] = { current: "", exiting: null, segmentId: 0 };
      }
      state.streaming[sessionId].current += delta;
    },
    /** Called when a new LLM segment starts — keep current text visible, just mark segment boundary */
    startNewSegment: (state, action: PayloadAction<string>) => {
      const sid = action.payload;
      const s = state.streaming[sid];
      if (s) {
        // Keep current text; new deltas will append to it (single bubble, no exit animation)
        s.segmentId = (s.segmentId ?? 0) + 1;
      } else {
        state.streaming[sid] = { current: "", exiting: null, segmentId: 0 };
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
    }>) => {
      const { sessionId, fishId, fishName, iteration, toolName, status } = action.payload;
      const steps = state.toolSteps[sessionId];
      if (!steps) return;
      // Find the call_fish step for this fish (most recent one)
      const step = [...steps].reverse().find((s) => s.name === "call_fish");
      if (step) {
        step.fishProgress = { fishId, fishName, iteration, toolName, status };
        if (status === "done") {
          step.completed = true;
          step.expanded = false;
        }
      }
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
