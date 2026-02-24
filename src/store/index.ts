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

interface ChatState {
  messagesBySession: Record<string, ChatMessage[]>;
  streamingText: Record<string, string>;
  activeTools: Record<string, { id: string; name: string; input: unknown }[]>;
  isRunning: Record<string, boolean>;
}

const chatSlice = createSlice({
  name: "chat",
  initialState: {
    messagesBySession: {},
    streamingText: {},
    activeTools: {},
    isRunning: {},
  } as ChatState,
  reducers: {
    setMessages: (state, action: PayloadAction<{ sessionId: string; messages: ChatMessage[] }>) => {
      state.messagesBySession[action.payload.sessionId] = action.payload.messages;
    },
    appendMessage: (state, action: PayloadAction<{ sessionId: string; message: ChatMessage }>) => {
      const { sessionId, message } = action.payload;
      if (!state.messagesBySession[sessionId]) {
        state.messagesBySession[sessionId] = [];
      }
      state.messagesBySession[sessionId].push(message);
    },
    appendDelta: (state, action: PayloadAction<{ sessionId: string; delta: string }>) => {
      const { sessionId, delta } = action.payload;
      state.streamingText[sessionId] = (state.streamingText[sessionId] ?? "") + delta;
    },
    clearStreaming: (state, action: PayloadAction<string>) => {
      delete state.streamingText[action.payload];
    },
    setToolStart: (state, action: PayloadAction<{ sessionId: string; id: string; name: string; input: unknown }>) => {
      const { sessionId, id, name, input } = action.payload;
      if (!state.activeTools[sessionId]) state.activeTools[sessionId] = [];
      state.activeTools[sessionId].push({ id, name, input });
    },
    removeActiveTool: (state, action: PayloadAction<{ sessionId: string; id: string }>) => {
      const { sessionId, id } = action.payload;
      if (state.activeTools[sessionId]) {
        state.activeTools[sessionId] = state.activeTools[sessionId].filter((t) => t.id !== id);
      }
    },
    setRunning: (state, action: PayloadAction<{ sessionId: string; running: boolean }>) => {
      state.isRunning[action.payload.sessionId] = action.payload.running;
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
