/**
 * Redux slices — chat domain.
 *
 * Owns per-session chat state:
 *
 *   - `sessions`   — the session list & active-session pointer (header bar)
 *   - `chat`       — messages + streaming state + tool steps + plan todos +
 *                    context-usage snapshots for the active turn
 *   - `scheduler`  — scheduled-task list (cron jobs)
 *
 * These are distinct slices (separate `createSlice` calls) because they own
 * logically different state shapes, but they're grouped together because
 * they all map to `commands/chat/*` on the Rust side.
 */
import { createSlice, PayloadAction } from "@reduxjs/toolkit";
import type { Session, ChatMessage, ScheduledTask } from "../../services/tauri";

// ---------------------------------------------------------------------------
// Sessions slice
// ---------------------------------------------------------------------------

interface SessionsState {
  sessions: Session[];
  activeSessionId: string | null;
  loading: boolean;
  error: string | null;
}

export const sessionsSlice = createSlice({
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
// Chat slice (messages + streaming + tool steps + plan + context usage)
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

export interface ContextUsageSnapshot {
  estimatedInputTokens: number;
  totalInputBudget: number;
  /** 60% of totalInputBudget — the compaction trigger line */
  triggerThreshold: number;
  cumulativeInputTokens: number;
  cumulativeOutputTokens: number;
  rollingSummaryVersion: number;
  /** Configured cumulative auto-compact threshold (0 = disabled) */
  autoCompactThreshold: number;
  /** p8 — optional per-layer token attribution so the ring can display
   *  a stacked visualisation. Absent for snapshots emitted before p8
   *  or from legacy codepaths. */
  layeredBreakdown?: LayeredTokenBreakdownSnapshot;
}

/** p8 — layered token attribution mirroring the Rust
 *  `LayeredTokenBreakdownSnapshot`. All values are raw token estimates;
 *  sum approximately equals `estimatedInputTokens`. */
export interface LayeredTokenBreakdownSnapshot {
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
  /** The final summary segment extracted by freezeStreaming, keyed by sessionId.
   *  Injected immediately into messagesBySession as a synthetic message so the user
   *  sees the final report without waiting for the async DB reload.
   *  Cleared by setMessagesWithFrozen once DB data arrives. */
  pendingSummary: Record<string, string>;
  /** Latest context-window utilisation snapshot per session, used to drive the ring
   *  progress indicator next to the send button. Populated from AgentEvent.context_usage
   *  during runs and seeded from get_context_preview on session switch / run completion. */
  contextUsage: Record<string, ContextUsageSnapshot>;
}

export const chatSlice = createSlice({
  name: "chat",
  initialState: {
    messagesBySession: {},
    streaming: {},
    toolSteps: {},
    toolStepsTurnDone: {},
    planBySession: {},
    isRunning: {},
    frozenBubble: {},
    pendingSummary: {},
    contextUsage: {},
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
    setContextUsage: (
      state,
      action: PayloadAction<{ sessionId: string; usage: ContextUsageSnapshot }>
    ) => {
      state.contextUsage[action.payload.sessionId] = action.payload.usage;
    },
    clearContextUsage: (state, action: PayloadAction<string>) => {
      delete state.contextUsage[action.payload];
    },
    /** Called on `done`: snapshot streaming text into frozenBubble + pendingSummary, clear streaming.
     *
     *  frozenBubble  = intermediate steps (everything before the last segment boundary).
     *                  Shown as the single collapsed bubble after DB reload.
     *  pendingSummary = the final summary segment (lastSegmentStart..end).
     *                  Immediately injected into messagesBySession so the user sees the
     *                  final report right away, before the async getMessages DB reload.
     *                  Cleared by setMessagesWithFrozen once DB data arrives.
     */
    freezeStreaming: (state, action: PayloadAction<string>) => {
      const sid = action.payload;
      const s = state.streaming[sid];
      if (s) {
        const intermediateText = s.lastSegmentStart > 0
          ? s.current.slice(0, s.lastSegmentStart).trimEnd()
          : "";
        const summaryText = s.lastSegmentStart > 0
          ? s.current.slice(s.lastSegmentStart).trimStart()
          : s.current;

        if (intermediateText.trim()) {
          state.frozenBubble[sid] = intermediateText;
        } else if (s.current.trim()) {
          // Single-segment run — full text is both the intermediate record and the summary.
          state.frozenBubble[sid] = s.current;
        }

        // Immediately append the final summary as a synthetic message so the user sees
        // it right away without waiting for the async DB reload.
        if (summaryText.trim()) {
          state.pendingSummary[sid] = summaryText;
          const existing = state.messagesBySession[sid] ?? [];
          // Remove any previous pending-summary placeholder, then append the new one.
          const withoutPrev = existing.filter((m) => !m.id.startsWith("pending_summary_"));
          const summaryMsg: ChatMessage = {
            id: `pending_summary_${sid}`,
            session_id: sid,
            role: "assistant",
            content: summaryText,
            created_at: new Date().toISOString(),
          };
          state.messagesBySession[sid] = [...withoutPrev, summaryMsg];
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

      // Find the final summary: prefer the last persisted assistant text message from DB.
      // Fall back to pendingSummary (the streamed text captured at done time) if DB has
      // nothing new yet — this handles the race where setMessagesWithFrozen is called
      // before the backend has fully persisted the final message.
      const lastAssistant = [...agentMessages]
        .reverse()
        .find((m) => m.role === "assistant" && m.content.trim() && !m.tool_calls_json);

      const pending = state.pendingSummary[sessionId];
      // Clear pendingSummary now that DB data has arrived.
      delete state.pendingSummary[sessionId];

      let summaryBubble: ChatMessage[] = [];
      if (lastAssistant && lastAssistant.content.trim() !== frozen.trim()) {
        // DB has a distinct final summary — use it (authoritative).
        summaryBubble = [lastAssistant];
      } else if (!lastAssistant && pending && pending.trim() !== frozen.trim()) {
        // DB doesn't have the final summary yet (or it was deduplicated) but we have
        // the streamed text — show it as a synthetic bubble so the user isn't left blank.
        summaryBubble = [{
          id: `pending_summary_${sessionId}`,
          session_id: sessionId,
          role: "assistant",
          content: pending,
          created_at: new Date().toISOString(),
        }];
      }

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
      delete state.pendingSummary[action.payload];
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
    /** Restore the most recent persisted task UI state for a session. */
    restoreTaskPanels: (
      state,
      action: PayloadAction<{
        sessionId: string;
        toolSteps: ToolStep[];
        planItems: PlanTodoItem[];
        turnDone: boolean;
      }>
    ) => {
      const { sessionId, toolSteps, planItems, turnDone } = action.payload;
      state.toolSteps[sessionId] = toolSteps;
      state.toolStepsTurnDone[sessionId] = turnDone;
      if (planItems.length > 0) {
        state.planBySession[sessionId] = planItems;
      } else {
        delete state.planBySession[sessionId];
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
// Scheduler slice
// ---------------------------------------------------------------------------

interface SchedulerState {
  tasks: ScheduledTask[];
  loading: boolean;
}

export const schedulerSlice = createSlice({
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
