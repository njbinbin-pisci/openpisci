/**
 * Redux slices — pool domain.
 *
 * Multi-agent coordination state:
 *
 *   - `koi`   — list of persistent Koi agents with their stats
 *   - `pool`  — Chat Pool sessions, active pool pointer, message cache
 *   - `board` — Koi todos (with filters: owner / priority / session)
 *
 * All three talk to `commands/pool/*` on the Rust side and subscribe to
 * `PoolEvent` updates via the canonical `host://pool_event` channel.
 */
import { createSlice, PayloadAction } from "@reduxjs/toolkit";
import type {
  KoiWithStats, KoiTodo, PoolSession, PoolMessage,
} from "../../services/tauri";

// ---------------------------------------------------------------------------
// Koi slice
// ---------------------------------------------------------------------------

interface KoiState {
  kois: KoiWithStats[];
  loading: boolean;
}

export const koiSlice = createSlice({
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

export const poolSlice = createSlice({
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

export const boardSlice = createSlice({
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
