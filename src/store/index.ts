/**
 * Redux store barrel.
 *
 * The monolithic ~780-line store module was split into three domain files
 * under `./slices/`, each mirroring the Rust-side `commands/{chat,pool,
 * config}` grouping:
 *
 *   - `./slices/chat`   — sessions, chat, scheduler
 *   - `./slices/pool`   — koi, pool, board
 *   - `./slices/config` — memory, skills, settings
 *
 * Components keep importing `store`, `RootState`, `AppDispatch`, the
 * `<domain>Actions` objects, and the public types (`ToolStep`,
 * `StreamingState`, `ContextUsageSnapshot`, `PlanTodoItem`,
 * `LayeredTokenBreakdownSnapshot`, `POOL_DEFAULT_CAPACITY`, `PondSubTab`)
 * from `"../store"` — this file re-exports all of that so no call site
 * had to change when the module was expanded into a directory.
 */
import { configureStore } from "@reduxjs/toolkit";

import { sessionsSlice, chatSlice, schedulerSlice } from "./slices/chat";
import { koiSlice, poolSlice, boardSlice } from "./slices/pool";
import { memorySlice, skillsSlice, settingsSlice } from "./slices/config";

// Public type re-exports so `import { ToolStep } from "../store"` keeps
// working. These are intentionally not wildcard-re-exported because we
// only want to expose the deliberate public shape, not every slice's
// internal `*State` helper types.
export type {
  ToolStep,
  PlanTodoItem,
  StreamingState,
  ContextUsageSnapshot,
  LayeredTokenBreakdownSnapshot,
} from "./slices/chat";
export { POOL_DEFAULT_CAPACITY } from "./slices/pool";
export type { PondSubTab } from "./slices/pool";

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

// ---------------------------------------------------------------------------
// Action creators, grouped by slice
// ---------------------------------------------------------------------------

export const sessionsActions = sessionsSlice.actions;
export const chatActions = chatSlice.actions;
export const memoryActions = memorySlice.actions;
export const skillsActions = skillsSlice.actions;
export const schedulerActions = schedulerSlice.actions;
export const settingsActions = settingsSlice.actions;
export const koiActions = koiSlice.actions;
export const poolActions = poolSlice.actions;
export const boardActions = boardSlice.actions;
