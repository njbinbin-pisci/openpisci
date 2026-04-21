/**
 * Redux slices — config domain.
 *
 * User-facing configuration state:
 *
 *   - `memory`   — long-term memory list (populates the Memory tab)
 *   - `skills`   — installed skill list (populates the Skills tab)
 *   - `settings` — global Settings payload + onboarding flags
 *
 * These slices mirror `commands/config/*` on the Rust side.
 */
import { createSlice, PayloadAction } from "@reduxjs/toolkit";
import type { Memory, Skill, Settings } from "../../services/tauri";

// ---------------------------------------------------------------------------
// Memory slice
// ---------------------------------------------------------------------------

interface MemoryState {
  memories: Memory[];
  loading: boolean;
}

export const memorySlice = createSlice({
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

export const skillsSlice = createSlice({
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
// Settings slice
// ---------------------------------------------------------------------------

interface SettingsState {
  settings: Settings | null;
  isConfigured: boolean;
  showOnboarding: boolean;
}

export const settingsSlice = createSlice({
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
