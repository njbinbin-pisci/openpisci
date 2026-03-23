import { Component, useEffect, useRef, useState, useCallback, useMemo, type ErrorInfo, type ReactNode } from "react";
import { useDispatch, useSelector } from "react-redux";
import { useTranslation } from "react-i18next";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { readFile } from "@tauri-apps/plugin-fs";
import { RootState, chatActions, sessionsActions, ToolStep, StreamingState, PlanTodoItem } from "../../store";
import { chatApi, sessionsApi, gatewayApi, AgentEventType, ChannelInfo, ChatAttachment } from "../../services/tauri";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { openPath } from "../../services/tauri";
import mermaid from "mermaid";
import InteractiveCard from "./InteractiveCard";
import ConfirmDialog from "../ConfirmDialog";
import { isInternalSession } from "../../utils/session";
import "./Chat.css";

// ─── Mermaid diagram block ────────────────────────────────────────────────────
mermaid.initialize({ startOnLoad: false, theme: "dark", securityLevel: "loose" });

let mermaidIdCounter = 0;

class RenderErrorBoundary extends Component<
  { fallback: ReactNode; children: ReactNode },
  { hasError: boolean }
> {
  state = { hasError: false };

  static getDerivedStateFromError() {
    return { hasError: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("[Chat] render boundary caught error:", error, info);
  }

  componentDidUpdate(prevProps: { fallback: ReactNode; children: ReactNode }) {
    if (this.state.hasError && prevProps.children !== this.props.children) {
      this.setState({ hasError: false });
    }
  }

  render() {
    if (this.state.hasError) return this.props.fallback;
    return this.props.children;
  }
}

function MermaidBlock({ code }: { code: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const idRef = useRef(`mermaid-${++mermaidIdCounter}`);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!ref.current) return;
    let cancelled = false;
    const id = idRef.current;
    setError(null);
    ref.current.innerHTML = "";

    const render = async () => {
      try {
        await mermaid.parse(code, { suppressErrors: false });
        const { svg } = await mermaid.render(id, code);
        if (!cancelled && ref.current) {
          ref.current.innerHTML = svg;
        }
      } catch (e) {
        if (!cancelled) {
          console.warn("[Chat] Mermaid render failed, falling back to code block:", e);
          setError(String(e));
        }
      }
    };

    render();
    return () => {
      cancelled = true;
    };
  }, [code]);

  if (error) {
    return (
      <pre className="code-block">
        <span className="code-lang">mermaid (parse error)</span>
        <code>{code}</code>
      </pre>
    );
  }
  return <div ref={ref} className="mermaid-block" />;
}

// ── Session classification ────────────────────────────────────────────────────

type SessionKind = "chat" | "im";

type SessionLike = { source?: string | null; id?: string | null };


function classifySession(session: SessionLike | undefined | null): SessionKind {
  if (isInternalSession(session)) return "chat";
  if (!session?.source || session.source === "chat") return "chat";
  return "im";
}

/** Map a session.source value to a compact display emoji/label. */
function sourceIcon(source: string): string {
  if (source === "chat" || !source) return "👤";
  if (source.includes("telegram")) return "✈";
  if (source.includes("feishu") || source.includes("lark")) return "📘";
  if (source.includes("wechat")) return "🟢";
  if (source.includes("wecom")) return "💬";
  if (source.includes("dingtalk")) return "📎";
  if (source.includes("slack")) return "⚡";
  if (source.includes("discord")) return "🎮";
  if (source.includes("teams")) return "🟦";
  if (source.includes("matrix")) return "⬛";
  if (source.includes("webhook")) return "🔗";
  return "📩";
}

export default function Chat() {
  const { t } = useTranslation();
  const dispatch = useDispatch();
  const { sessions, activeSessionId } = useSelector((s: RootState) => s.sessions);
  const { messagesBySession, streaming, toolSteps, planBySession, isRunning } = useSelector(
    (s: RootState) => s.chat
  );

  const [input, setInput] = useState("");
  const [sendError, setSendError] = useState<string | null>(null);
  const [sessionFilter, setSessionFilter] = useState<"all" | SessionKind>("all");

  // Attachment state
  const [attachment, setAttachment] = useState<ChatAttachment | null>(null);
  // Preview URL for image attachments (object URL or base64 data URL)
  const [attachmentPreview, setAttachmentPreview] = useState<string | null>(null);
  const [gatewayChannels, setGatewayChannels] = useState<ChannelInfo[]>([]);
  const [gatewayConnecting, setGatewayConnecting] = useState(false);
  const [gatewayDisconnecting, setGatewayDisconnecting] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<{ id: string; title: string } | null>(null);
  const [deletingSession, setDeletingSession] = useState(false);
  // History pagination: capacity starts at CHAT_INITIAL_SIZE, grows by CHAT_LAZY_STEP on each lazy-load
  const CHAT_INITIAL_SIZE = 200;
  const CHAT_LAZY_STEP = 10;
  const [capacity, setCapacity] = useState(CHAT_INITIAL_SIZE);
  const [hasMoreHistory, setHasMoreHistory] = useState(false);
  const [unreadCount, setUnreadCount] = useState(0);
  const [permissionRequest, setPermissionRequest] = useState<{
    requestId: string;
    toolName: string;
    toolInput: any;
    description: string;
  } | null>(null);

  // Interactive UI cards from chat_ui tool
  const [interactiveCards, setInteractiveCards] = useState<
    Record<string, { requestId: string; uiDefinition: any; submitted?: boolean }>
  >({});

  // Context debug preview
  type ContextPreviewBlock =
    | { type: "text"; text: string }
    | { type: "tool_use"; id: string; name: string; input: string }
    | { type: "tool_result"; tool_use_id: string; content: string; is_error: boolean; truncated: boolean }
    | { type: "image"; note: string };

  const [contextPreview, setContextPreview] = useState<{
    messages: { role: string; blocks: ContextPreviewBlock[]; tokens: number }[];
    messages_tokens: number;
    total_tokens: number;
    model: string;
    context_budget: number;
  } | null>(null);
  const [contextPreviewLoading, setContextPreviewLoading] = useState(false);
  // Track which tool_use/tool_result blocks are expanded (by index key "msgIdx-blockIdx")
  const [expandedBlocks, setExpandedBlocks] = useState<Set<string>>(new Set());
  const toggleBlock = (key: string) => {
    setExpandedBlocks(prev => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key); else next.add(key);
      return next;
    });
  };

  const handleShowContextPreview = async () => {
    if (!activeSessionId) return;
    setContextPreviewLoading(true);
    try {
      const preview = await invoke<NonNullable<typeof contextPreview>>("get_context_preview", { sessionId: activeSessionId });
      setContextPreview(preview);
      setExpandedBlocks(new Set());
    } catch (e) {
      alert("Failed to load context preview: " + String(e));
    } finally {
      setContextPreviewLoading(false);
    }
  };
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const messagesAreaRef = useRef<HTMLDivElement>(null);
  const toolStepsScrollRef = useRef<HTMLDivElement>(null);
  const unlistenRef = useRef<UnlistenFn | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  // Whether the user is scrolled near the bottom (so we auto-scroll on new messages)
  const isNearBottomRef = useRef(true);
  // Flag set during loadMoreHistory to suppress auto-scroll
  const loadingMoreRef = useRef(false);
  // Keep a ref to the current sessionId so event callbacks always see the latest value
  const activeSessionIdRef = useRef<string | null>(activeSessionId);
  useEffect(() => {
    activeSessionIdRef.current = activeSessionId;
  }, [activeSessionId]);
  // Keep a ref to isImSession so the event callback closure always sees the latest value
  const isImSessionRef = useRef(false);

  // Throttle buffer for text_delta — accumulate deltas and flush every 80ms
  const deltaBufferRef = useRef<Record<string, string>>({});
  const flushTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    flushTimerRef.current = setInterval(() => {
      const buffer = deltaBufferRef.current;
      const entries = Object.entries(buffer);
      if (entries.length === 0) return;
      deltaBufferRef.current = {};
      for (const [sid, delta] of entries) {
        if (delta) {
          dispatch(chatActions.appendDelta({ sessionId: sid, delta }));
        }
      }
    }, 80);
    return () => {
      if (flushTimerRef.current) clearInterval(flushTimerRef.current);
    };
  }, [dispatch]);

  const rawMessages = activeSessionId ? messagesBySession[activeSessionId] ?? [] : [];

  // Extract historical interactive cards from chat_ui tool calls in persisted messages
  const historicalCards = useMemo(() => {
    const cards: Record<string, { requestId: string; uiDefinition: any; submittedValues: Record<string, unknown> | null; afterMessageId: string }> = {};
    for (let i = 0; i < rawMessages.length; i++) {
      const m = rawMessages[i];
      if (m.role !== "assistant" || !m.tool_calls_json) continue;
      try {
        const calls = JSON.parse(m.tool_calls_json);
        for (const call of Array.isArray(calls) ? calls : []) {
          if (call.name !== "chat_ui") continue;
          const uiDef = call.input?.ui_definition;
          if (!uiDef) continue;
          // Find matching tool result in subsequent messages
          let submittedValues: Record<string, unknown> | null = null;
          for (let j = i + 1; j < rawMessages.length && j <= i + 3; j++) {
            const rm = rawMessages[j];
            if (!rm.tool_results_json) continue;
            try {
              const results = JSON.parse(rm.tool_results_json);
              for (const r of Array.isArray(results) ? results : []) {
                if (r.tool_use_id === call.id && !r.is_error) {
                  try { submittedValues = JSON.parse(r.content.replace(/^User submitted.*?Selections:\n/, "")); } catch { /* text result */ }
                }
              }
            } catch { /* ignore parse errors */ }
          }
          cards[call.id] = { requestId: call.id, uiDefinition: uiDef, submittedValues, afterMessageId: m.id };
        }
      } catch { /* ignore parse errors */ }
    }
    return cards;
  }, [rawMessages]);

  // Check if a message is a chat_ui tool call or its result (should be rendered as a card, not filtered entirely)
  const chatUiToolCallIds = useMemo(() => {
    const ids = new Set<string>();
    for (const m of rawMessages) {
      if (m.role === "assistant" && m.tool_calls_json) {
        try {
          const calls = JSON.parse(m.tool_calls_json);
          for (const c of Array.isArray(calls) ? calls : []) {
            if (c.name === "chat_ui") ids.add(m.id);
          }
        } catch { /* ignore */ }
      }
    }
    return ids;
  }, [rawMessages]);

  const activeMessages = rawMessages
    // Filter out tool-result carrier messages (role=user, no text content, only tool_results_json)
    .filter((m) => !(m.role === "user" && !m.content.trim() && m.tool_results_json))
    // Filter out pure tool-call assistant messages (no text content, only tool_calls_json).
    // Keep assistant messages that have actual text content even if they also have tool_calls_json.
    // BUT keep chat_ui tool calls since they render as interactive cards.
    .filter((m) => !(m.role === "assistant" && !m.content.trim() && m.tool_calls_json && !chatUiToolCallIds.has(m.id)))
    // Filter out duplicate consecutive messages with same role and content
    .filter((m, i, arr) => {
      if (i === 0) return true;
      const prev = arr[i - 1];
      return !(prev.role === m.role && prev.content === m.content);
    });
  const streamingState: StreamingState | null = activeSessionId ? streaming[activeSessionId] ?? null : null;
  const streamingCurrent = streamingState?.current ?? "";
  const running = activeSessionId ? isRunning[activeSessionId] ?? false : false;
  const steps = activeSessionId ? toolSteps[activeSessionId] ?? [] : [];
  const activePlan = activeSessionId ? planBySession[activeSessionId] ?? [] : [];
  const activeSession = sessions.find((s) => s.id === activeSessionId);

  // Tool steps panel: open while running, auto-close when agent finishes
  const [stepsOpen, setStepsOpen] = useState(false);
  const [planOpen, setPlanOpen] = useState(true);

  // Plan resume dialog: shown when user sends a message while unfinished todos exist
  const [planResumeDialog, setPlanResumeDialog] = useState<{
    pendingContent: string;
    pendingAttachment: import("../../services/tauri").ChatAttachment | null;
  } | null>(null);
  const prevRunningRef = useRef(false);
  useEffect(() => {
    if (running && !prevRunningRef.current) {
      // Agent just started — open the steps panel
      setStepsOpen(true);
      setPlanOpen(true);
    } else if (!running && prevRunningRef.current) {
      // Agent just finished — hide the steps panel
      setStepsOpen(false);
    }
    prevRunningRef.current = running;
  }, [running]);
  const activeSessionKind = classifySession(activeSession);
  const isImSession = activeSessionKind === "im";
  isImSessionRef.current = isImSession;

  // Load messages when the active session ID changes.
  // Also sync running state from DB to fix stale state if im_session_done was missed.
  useEffect(() => {
    if (!activeSessionId) return;
    setCapacity(CHAT_INITIAL_SIZE);
    setUnreadCount(0);
    prevLastChatIdRef.current = null;
    isNearBottomRef.current = true;

    const load = async () => {
      try {
        const [messages, { sessions: fresh }] = await Promise.all([
          sessionsApi.getMessages(activeSessionId, CHAT_INITIAL_SIZE, 0),
          sessionsApi.list(),
        ]);
        // Use setMessagesWithFrozen: if a frozenBubble exists for this session (set during
        // a recent agent run), it is preserved as a single collapsed bubble. For sessions
        // with no frozenBubble (old history, other sessions), it falls back to plain setMessages.
        // Do NOT auto-reconstruct frozenBubble from DB here — that would collapse all history.
        dispatch(chatActions.setMessagesWithFrozen({ sessionId: activeSessionId, messages }));
        setHasMoreHistory(messages.length >= CHAT_INITIAL_SIZE);
        // Correct stale running state from DB
        const s = fresh.find((x) => x.id === activeSessionId);
        if (s && s.status !== "running") {
          dispatch(chatActions.setRunning({ sessionId: activeSessionId, running: false }));
          dispatch(chatActions.clearStreaming(activeSessionId));
        }
      } catch (e) {
        console.error('[Chat] failed to load messages on session switch:', e);
      }
    };
    load();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeSessionId, dispatch]);

  // Load CHAT_LAZY_STEP older messages (incremental prepend), triggered by scrolling to top
  const loadMoreHistory = useCallback(() => {
    if (!activeSessionId || loadingMoreRef.current) return;
    const el = messagesAreaRef.current;
    const prevScrollHeight = el ? el.scrollHeight : 0;
    const currentCount = rawMessages.length;
    loadingMoreRef.current = true;
    sessionsApi.getMessages(activeSessionId, CHAT_LAZY_STEP, currentCount).then((older) => {
      if (older.length > 0) {
        dispatch(chatActions.prependChatMessages({ sessionId: activeSessionId, messages: older }));
        setHasMoreHistory(older.length === CHAT_LAZY_STEP);
        setCapacity((c) => c + CHAT_LAZY_STEP);
      } else {
        setHasMoreHistory(false);
      }
      // Restore scroll position after prepend so the view stays at the same message
      requestAnimationFrame(() => {
        if (el) {
          el.scrollTop = el.scrollHeight - prevScrollHeight;
        }
        loadingMoreRef.current = false;
      });
    }).catch(() => { loadingMoreRef.current = false; });
  }, [activeSessionId, rawMessages.length, dispatch]);

  // When the filter changes, switch to the first visible session if the current
  // active session is not visible under the new filter.
  // We use refs for sessions/activeSessionId to avoid re-running on every session
  // list update (which would kick the user out of IM sessions not yet in the list).
  const sessionsRef = useRef(sessions);
  sessionsRef.current = sessions;
  const activeSessionIdForFilterRef = useRef(activeSessionId);
  activeSessionIdForFilterRef.current = activeSessionId;
  useEffect(() => {
    const currentSessions = sessionsRef.current;
    const currentActiveId = activeSessionIdForFilterRef.current;
    const visibleSessions = currentSessions.filter((x) => !isInternalSession(x) && (
      sessionFilter === "all" || classifySession(x) === sessionFilter
    ));
    if (visibleSessions.length === 0) return;
    const s = currentActiveId ? currentSessions.find((x) => x.id === currentActiveId) : null;
    if (s && visibleSessions.some((x) => x.id === s.id)) return;
    const first = visibleSessions[0];
    dispatch(sessionsActions.setActiveSession(first ? first.id : null));
  }, [sessionFilter, dispatch]);

  // Subscribe to agent events — use ref to avoid stale closure over activeSessionId
  useEffect(() => {
    if (!activeSessionId) return;

    // Cleanup previous listener synchronously before registering the new one
    if (unlistenRef.current) {
      unlistenRef.current();
      unlistenRef.current = null;
    }

    let cancelled = false;

    // The session id this listener is bound to — used for session-scoped operations
    // like freezeStreaming and getMessages on done, which must target THIS session,
    // not whatever session happens to be active when the event fires.
    const boundSessionId = activeSessionId;
    console.log('[Chat] registering event listener for session:', boundSessionId);
    chatApi.onEvent(activeSessionId, (event: AgentEventType) => {
      console.log('[Chat] received event:', event.type, 'for session:', boundSessionId);
      // For streaming deltas: write to the currently visible session (ref) so the user
      // sees live output even if they switched sessions mid-stream.
      // For session-scoped finalization (done, error): always use boundSessionId so we
      // don't corrupt another session's frozenBubble or message state.
      const sid = activeSessionIdRef.current;
      if (!sid) return;
      switch (event.type) {
        case "text_segment_start":
          // Flush any buffered delta before starting a new segment
          {
            const buffered = deltaBufferRef.current[sid];
            if (buffered) {
              delete deltaBufferRef.current[sid];
              dispatch(chatActions.appendDelta({ sessionId: sid, delta: buffered }));
            }
          }
          // Mark segment boundary — current text stays visible, new deltas will append
          dispatch(chatActions.startNewSegment(sid));
          break;
        case "text_delta":
          // Buffer delta for throttled flush (80ms interval)
          deltaBufferRef.current[sid] = (deltaBufferRef.current[sid] ?? "") + event.delta;
          break;
        case "tool_start":
          dispatch(chatActions.addToolStep({ sessionId: sid, id: event.id, name: event.name, input: event.input }));
          break;
        case "tool_end":
          // Mark the step as completed — it stays visible for the user to review
          dispatch(chatActions.completeToolStep({
            sessionId: sid,
            id: event.id,
            result: event.result,
            isError: event.is_error ?? false,
          }));
          break;
        case "plan_update":
          dispatch(chatActions.setPlan({ sessionId: sid, items: event.items }));
          break;
        case "permission_request":
          setPermissionRequest({
            requestId: event.request_id,
            toolName: event.tool_name,
            toolInput: event.tool_input,
            description: event.description,
          });
          break;
        case "interactive_ui":
          setInteractiveCards((prev) => ({
            ...prev,
            [event.request_id]: {
              requestId: event.request_id,
              uiDefinition: event.ui_definition,
            },
          }));
          // Scroll to bottom so the card is immediately visible — it renders after the
          // streaming bubble, so without this the user might not notice it appeared.
          setTimeout(() => scrollToBottom(true), 50);
          break;
        case "done":
          // Use boundSessionId (the session this listener was registered for) so that
          // freezeStreaming and getMessages always target the correct session, even if
          // the user switched to a different session while the agent was running.
          console.log('[Chat] agent done event, boundSid=', boundSessionId);
          dispatch(chatActions.setRunning({ sessionId: boundSessionId, running: false }));
          dispatch(chatActions.freezeStreaming(boundSessionId));
          dispatch(chatActions.removeOptimisticMessages(boundSessionId));
          sessionsApi.getMessages(boundSessionId, CHAT_INITIAL_SIZE).then((messages) => {
            console.log('[Chat] done: reloaded', messages.length, 'messages for', boundSessionId);
            dispatch(chatActions.setMessagesWithFrozen({ sessionId: boundSessionId, messages }));
          }).catch(() => {});
          break;
        case "fish_progress":
          dispatch(chatActions.updateFishProgress({
            sessionId: sid,
            fishId: event.fish_id,
            fishName: event.fish_name,
            iteration: event.iteration,
            toolName: event.tool_name,
            status: event.status,
            textDelta: (event as { type: "fish_progress"; fish_id: string; fish_name: string; iteration: number; tool_name: string | null; status: string; text_delta?: string }).text_delta,
          }));
          break;
        case "error":
          // Also use boundSessionId for error — clears running state for the correct session.
          dispatch(chatActions.setRunning({ sessionId: boundSessionId, running: false }));
          dispatch(chatActions.clearStreaming(boundSessionId));
          setSendError((event as { type: "error"; message: string }).message ?? "Unknown error");
          break;
      }
    }).then((unlisten) => {
      if (cancelled) {
        // Effect already cleaned up before the promise resolved — unlisten immediately
        unlisten();
      } else {
        unlistenRef.current = unlisten;
      }
    });

    return () => {
      cancelled = true;
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
    };
  }, [activeSessionId, dispatch]);

  // Track whether user is near the bottom (bottom 10%) and trigger lazy-load on scroll to top
  useEffect(() => {
    const el = messagesAreaRef.current;
    if (!el) return;
    const onScroll = () => {
      const scrollable = el.scrollHeight - el.clientHeight;
      isNearBottomRef.current = scrollable <= 0 || el.scrollTop >= scrollable * 0.9;
      if (isNearBottomRef.current) setUnreadCount(0);
      // Trigger lazy-load when scrolled near the top
      if (el.scrollTop < 60 && hasMoreHistory && !loadingMoreRef.current) {
        const prevScrollHeight = el.scrollHeight;
        loadMoreHistory();
        requestAnimationFrame(() => {
          el.scrollTop = el.scrollHeight - prevScrollHeight;
        });
      }
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, [hasMoreHistory, loadMoreHistory]);

  // Scroll the messages area to the bottom without affecting parent containers.
  // scrollIntoView() bubbles up and can cause the whole window to jump in Tauri WebView;
  // directly setting scrollTop on the container avoids that.
  const scrollToBottom = useCallback((smooth = true) => {
    const el = messagesAreaRef.current;
    if (!el) return;
    if (smooth) {
      el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    } else {
      el.scrollTop = el.scrollHeight;
    }
  }, []);

  // Detect real-time appends (tail id changed), apply FIFO trim, auto-scroll or show unread badge
  const prevLastChatIdRef = useRef<string | null>(null);
  useEffect(() => {
    if (loadingMoreRef.current || rawMessages.length === 0) return;
    const lastId = rawMessages[rawMessages.length - 1].id;
    const isAppend = lastId !== prevLastChatIdRef.current && prevLastChatIdRef.current !== null;
    prevLastChatIdRef.current = lastId;
    if (!isAppend) {
      // Still auto-scroll for streaming updates (streamingCurrent changes)
      if (isNearBottomRef.current) {
        scrollToBottom();
      }
      return;
    }
    // FIFO trim: evict oldest messages beyond current capacity
    if (activeSessionId && rawMessages.length > capacity) {
      dispatch(chatActions.trimChatMessages({ sessionId: activeSessionId, capacity }));
      setHasMoreHistory(true);
    }
    if (isNearBottomRef.current) {
      scrollToBottom();
      setUnreadCount(0);
    } else {
      setUnreadCount((n) => n + 1);
    }
  }, [rawMessages, streamingCurrent, capacity, activeSessionId, dispatch, scrollToBottom]);

  // Scroll the tool-steps area to the bottom when a new step is added or toggled open
  useEffect(() => {
    const el = toolStepsScrollRef.current;
    if (!el) return;
    // Scroll to bottom of the steps scroll container so the latest step is always visible
    el.scrollTop = el.scrollHeight;
  }, [steps]);

  const handleNewSession = useCallback(async () => {
    try {
      const session = await sessionsApi.create(t("chat.newChat"));
      dispatch(sessionsActions.addSession(session));
      dispatch(sessionsActions.setActiveSession(session.id));
    } catch (e) {
      setSendError(t("chat.failedCreate", { error: String(e) }));
    }
  }, [dispatch, t]);

  // Load gateway status on mount and when switching to IM filter
  useEffect(() => {
    gatewayApi.list().then((r) => setGatewayChannels(r.channels)).catch(() => setGatewayChannels([]));
  }, [sessionFilter]);

  // (activeFishIds removed — session filtering no longer depends on Fish activation state)

  const handleGatewayConnect = useCallback(async () => {
    setGatewayConnecting(true);
    try {
      const r = await Promise.race([
        gatewayApi.connect(),
        new Promise<never>((_, reject) => setTimeout(() => reject(new Error(t("settings.channelTimeout"))), 20000)),
      ]);
      setGatewayChannels(r.channels);
    } catch {
      // ignore, user can retry
    } finally {
      setGatewayConnecting(false);
    }
  }, [t]);

  const handleGatewayDisconnect = useCallback(async () => {
    setGatewayDisconnecting(true);
    try {
      await gatewayApi.disconnect();
      setGatewayChannels([]);
    } catch {
      // ignore
    } finally {
      setGatewayDisconnecting(false);
    }
  }, []);

  const handleDeleteSession = useCallback(async (sessionId: string) => {
    try {
      await sessionsApi.delete(sessionId);
      dispatch(sessionsActions.removeSession(sessionId));
      if (activeSessionId === sessionId) {
        const remaining = sessions.filter((s) => {
          if (s.id === sessionId) return false;
          if (isInternalSession(s)) return false;
          if (sessionFilter === "all") return true;
          return classifySession(s) === sessionFilter;
        });
        dispatch(sessionsActions.setActiveSession(remaining.length > 0 ? remaining[0].id : null));
      }
    } catch (e) {
      setSendError(t("chat.failedDelete", { error: String(e) }));
    }
  }, [activeSessionId, sessions, sessionFilter, dispatch, t]);

  const requestDeleteSession = useCallback((e: React.MouseEvent, sessionId: string, title: string) => {
    e.stopPropagation();
    setDeleteTarget({ id: sessionId, title });
  }, []);

  const confirmDeleteSession = useCallback(async () => {
    if (!deleteTarget) return;
    try {
      setDeletingSession(true);
      await handleDeleteSession(deleteTarget.id);
      setDeleteTarget(null);
    } finally {
      setDeletingSession(false);
    }
  }, [deleteTarget, handleDeleteSession]);

  const handleAttach = useCallback(async () => {
    try {
      const selected = await openFileDialog({
        multiple: false,
        filters: [
          { name: t("chat.attachImages"), extensions: ["png", "jpg", "jpeg", "gif", "webp"] },
          { name: t("chat.attachFiles"), extensions: ["pdf", "txt", "md", "csv", "json", "ts", "tsx", "js", "jsx", "py", "rs", "go", "java", "c", "cpp", "h", "yaml", "toml", "xml", "html", "css"] },
          { name: t("chat.attachAll"), extensions: ["*"] },
        ],
      });
      if (!selected) return;

      const filePath = selected as string;
      const filename = filePath.split(/[\\/]/).pop() ?? filePath;
      const ext = filename.split(".").pop()?.toLowerCase() ?? "";
      const imageExts = ["png", "jpg", "jpeg", "gif", "webp"];
      const isImage = imageExts.includes(ext);

      if (isImage) {
        // Read file bytes and convert to base64 for vision model support
        const bytes = await readFile(filePath);
        const mimeMap: Record<string, string> = {
          png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg",
          gif: "image/gif", webp: "image/webp",
        };
        const mediaType = mimeMap[ext] ?? "image/jpeg";
        // Build base64 string
        let binary = "";
        const chunk = 8192;
        for (let i = 0; i < bytes.length; i += chunk) {
          binary += String.fromCharCode(...bytes.slice(i, i + chunk));
        }
        const b64 = btoa(binary);
        setAttachment({ media_type: mediaType, path: filePath, data: b64, filename });
        setAttachmentPreview(`data:${mediaType};base64,${b64}`);
      } else {
        // Non-image: just pass path
        setAttachment({ media_type: "application/octet-stream", path: filePath, filename });
        setAttachmentPreview(null);
      }
    } catch (e) {
      console.error("attach error:", e);
    }
  }, [t]);

  const clearAttachment = useCallback(() => {
    setAttachment(null);
    setAttachmentPreview(null);
  }, []);

  // ── File drag-and-drop ────────────────────────────────────────────────────
  const [isDragging, setIsDragging] = useState(false);

  const processDroppedFile = useCallback(async (filePath: string) => {
    const filename = filePath.split(/[\\/]/).pop() ?? filePath;
    const ext = filename.split(".").pop()?.toLowerCase() ?? "";
    const imageExts = ["png", "jpg", "jpeg", "gif", "webp"];
    const isImage = imageExts.includes(ext);

    if (isImage) {
      try {
        const bytes = await readFile(filePath);
        const mimeMap: Record<string, string> = {
          png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg",
          gif: "image/gif", webp: "image/webp",
        };
        const mediaType = mimeMap[ext] ?? "image/jpeg";
        let binary = "";
        const chunk = 8192;
        for (let i = 0; i < bytes.length; i += chunk) {
          binary += String.fromCharCode(...bytes.slice(i, i + chunk));
        }
        const b64 = btoa(binary);
        setAttachment({ media_type: mediaType, path: filePath, data: b64, filename });
        setAttachmentPreview(`data:${mediaType};base64,${b64}`);
      } catch (e) {
        console.error("drop image read error:", e);
        // Fallback: just use path
        setAttachment({ media_type: "application/octet-stream", path: filePath, filename });
      }
    } else {
      // Non-image: append path to input text
      setInput((prev) => {
        const sep = prev.trim() ? "\n" : "";
        return prev + sep + filePath;
      });
    }
  }, []);

  const handleDrop = useCallback(async (e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setIsDragging(false);
    if (running) return;

    const files = Array.from(e.dataTransfer.files);
    if (files.length === 0) return;

    // Check if any file is an image (for single-image attachment)
    const imageExts = ["png", "jpg", "jpeg", "gif", "webp"];
    const imageFiles = files.filter(f => {
      const ext = f.name.split(".").pop()?.toLowerCase() ?? "";
      return imageExts.includes(ext);
    });
    const nonImageFiles = files.filter(f => {
      const ext = f.name.split(".").pop()?.toLowerCase() ?? "";
      return !imageExts.includes(ext);
    });

    // Single image and no non-image files: use attachment mechanism
    if (imageFiles.length === 1 && nonImageFiles.length === 0) {
      // Get path via webkitRelativePath or name; Tauri provides full path via dataTransfer
      const filePath = (files[0] as any).path as string | undefined;
      if (filePath) {
        await processDroppedFile(filePath);
        return;
      }
    }

    // Multiple files or non-images: append all paths to input
    for (const file of files) {
      const filePath = (file as any).path as string | undefined;
      if (filePath) {
        await processDroppedFile(filePath);
      }
    }
  }, [running, processDroppedFile]);

  const handleDragOver = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (!running) setIsDragging(true);
  }, [running]);

  const handleDragLeave = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    setIsDragging(false);
  }, []);

  // Core send logic, called after plan-resume decision is made.
  // clearPlan=true: clear existing plan before this turn (default / new task)
  // clearPlan=false: keep existing plan (user chose to continue previous tasks)
  const doSend = useCallback(async (
    content: string,
    pendingAttachment: import("../../services/tauri").ChatAttachment | null,
    clearPlan: boolean,
  ) => {
    if (!activeSessionId) return;

    dispatch(chatActions.clearToolSteps(activeSessionId));
    if (clearPlan) dispatch(chatActions.clearPlan(activeSessionId));
    dispatch(chatActions.clearStreaming(activeSessionId));
    // Clear frozen bubble so the next turn starts fresh from DB messages
    dispatch(chatActions.clearFrozenBubble(activeSessionId));

    // Auto-title: if this is the first message in the session, derive a title from it
    const currentMessages = messagesBySession[activeSessionId] ?? [];
    if (currentMessages.length === 0) {
      const raw = (content || pendingAttachment?.filename || "").replace(/\s+/g, " ").trim();
      const title = raw.length > 30 ? raw.slice(0, 30) + "…" : raw;
      if (title) {
        sessionsApi.rename(activeSessionId, title).catch(() => {});
        dispatch(sessionsActions.updateSessionTitle({ id: activeSessionId, title }));
      }
    }

    // Build display content for optimistic message (include attachment hint)
    const displayContent = pendingAttachment
      ? content
        ? `${content}\n📎 ${pendingAttachment.filename ?? pendingAttachment.path ?? t("chat.attachment")}`
        : `📎 ${pendingAttachment.filename ?? pendingAttachment.path ?? t("chat.attachment")}`
      : content;

    dispatch(chatActions.appendMessage({
      sessionId: activeSessionId,
      message: {
        id: `optimistic_${Date.now()}`,
        session_id: activeSessionId,
        role: "user",
        content: displayContent,
        created_at: new Date().toISOString(),
      },
    }));

    dispatch(chatActions.setRunning({ sessionId: activeSessionId, running: true }));

    try {
      await chatApi.send(activeSessionId, content, pendingAttachment ?? undefined, clearPlan);
    } catch (e) {
      console.error('[Chat] send error:', e);
      dispatch(chatActions.setRunning({ sessionId: activeSessionId, running: false }));
      dispatch(chatActions.clearStreaming(activeSessionId));
      setSendError(`${e}`);
    }
  }, [activeSessionId, messagesBySession, dispatch, t]);

  const handleSend = useCallback(async () => {
    if ((!input.trim() && !attachment) || !activeSessionId || running) return;

    const content = input.trim();
    setInput("");
    setSendError(null);
    const pendingAttachment = attachment;
    clearAttachment();

    // Check if there are unfinished todos — if so, ask the user what to do
    const unfinished = activePlan.filter(
      (item) => item.status === "pending" || item.status === "in_progress"
    );
    if (unfinished.length > 0) {
      setPlanResumeDialog({ pendingContent: content, pendingAttachment });
      return;
    }

    await doSend(content, pendingAttachment, true);
  }, [input, attachment, activeSessionId, running, activePlan, doSend, clearAttachment]);

  const handleCancel = useCallback(() => {
    if (activeSessionId) {
      chatApi.cancel(activeSessionId);
    }
  }, [activeSessionId]);

  const handlePermissionResponse = useCallback(async (approved: boolean) => {
    if (!permissionRequest) return;
    try {
      await invoke("respond_permission", {
        requestId: permissionRequest.requestId,
        approved,
      });
    } catch (e) {
      setSendError(`Permission response failed: ${e}`);
    }
    setPermissionRequest(null);
  }, [permissionRequest]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  // ── Filtered session list (single source of truth) ───────────────────────
  const filteredSessions = sessions.filter((s) => {
    if (isInternalSession(s)) return false;
    if (sessionFilter === "all") return true;
    return classifySession(s) === sessionFilter;
  });

  return (
    <div className="chat-layout">
      {/* Session sidebar */}
      <div className="session-list">
        <div className="session-list-header">
          <span>{t("chat.chats")}</span>
          <button className="btn-icon" onClick={handleNewSession} title={t("chat.newChat")}>+</button>
        </div>

        {/* Filter tabs */}
        <div style={{ display: "flex", gap: 4, padding: "4px 8px 0", fontSize: 12 }}>
          {(["all", "chat", "im"] as const).map((f) => (
            <button
              key={f}
              onClick={() => setSessionFilter(f)}
              style={{
                flex: 1,
                padding: "3px 0",
                borderRadius: "var(--radius-sm)",
                border: "1px solid var(--border)",
                background: sessionFilter === f ? "var(--accent)" : "transparent",
                color: sessionFilter === f ? "#fff" : "var(--text-secondary)",
                cursor: "pointer",
                fontSize: 11,
              }}
            >
              {f === "all" ? t("chat.filterAll") : f === "chat" ? t("chat.filterChat") : t("chat.filterIM")}
            </button>
          ))}
        </div>

        <div style={{ flex: 1, overflowY: "auto" }}>
          {filteredSessions.map((s) => {
              const icon = sourceIcon(s.source);
              const sessionTitle = (s.title ?? t("chat.defaultTitle")).replace(/^🐠\s*/, "");
              return (
                <div
                  key={s.id}
                  className={`session-item ${s.id === activeSessionId ? "active" : ""}`}
                  onClick={() => dispatch(sessionsActions.setActiveSession(s.id))}
                >
                  <span className="session-title">
                    {icon && <span style={{ marginRight: 4, fontSize: 12 }}>{icon}</span>}
                    {sessionTitle}
                  </span>
                  <span className="session-item-right">
                    <span className="session-count">{s.message_count}</span>
                    <button
                      className="session-delete-btn"
                      title={t("chat.deleteChat")}
                      onClick={(e) => requestDeleteSession(e, s.id, sessionTitle)}
                    >✕</button>
                  </span>
                </div>
              );
            })}
          {filteredSessions.length === 0 && (
            <div className="session-empty">{t("chat.noChats")}</div>
          )}
        </div>

        {/* IM channel quick-connect panel — shown when IM filter is active */}
        {sessionFilter === "im" && (
          <div style={{
            marginTop: "auto",
            borderTop: "1px solid var(--border)",
            padding: "10px 8px",
            fontSize: 12,
          }}>
            {/* Connected channels list */}
            {gatewayChannels.length > 0 && (
              <div style={{ marginBottom: 8 }}>
                {gatewayChannels.map((ch) => (
                  <div key={ch.name} style={{ display: "flex", justifyContent: "space-between", alignItems: "center", padding: "2px 0", color: "var(--text-secondary)" }}>
                    <span style={{ fontSize: 11 }}>{ch.name}</span>
                    <span style={{
                      fontSize: 10,
                      color: ch.status === "Connected" ? "#28a745" : ch.status === "Connecting" ? "#ffc107" : "var(--text-muted)",
                      fontWeight: 600,
                    }}>
                      {ch.status === "Connected" ? "●" : ch.status === "Connecting" ? "◌" : "○"}
                    </span>
                  </div>
                ))}
              </div>
            )}
            <div style={{ display: "flex", gap: 4 }}>
              {(() => {
                const hasConnected = gatewayChannels.some((ch) => ch.status === "Connected" || ch.status === "Connecting");
                return (
                  <>
                    <button
                      className="btn btn-primary"
                      style={{ flex: 1, fontSize: 11, padding: "4px 0", justifyContent: "center" }}
                      onClick={handleGatewayConnect}
                      disabled={gatewayConnecting || gatewayDisconnecting}
                    >
                      {gatewayConnecting
                        ? t("common.connecting")
                        : hasConnected
                          ? t("settings.reconnectChannels")
                          : t("settings.connectChannels")}
                    </button>
                    <button
                      className="btn"
                      style={{ flex: 1, fontSize: 11, padding: "4px 0", justifyContent: "center", border: "1px solid var(--border)" }}
                      onClick={handleGatewayDisconnect}
                      disabled={gatewayDisconnecting || gatewayConnecting || !hasConnected}
                    >
                      {gatewayDisconnecting ? t("common.disconnecting") : t("settings.disconnectAll")}
                    </button>
                  </>
                );
              })()}
            </div>
          </div>
        )}
      </div>

      {/* Main chat area */}
      <div className="chat-main">
        {activeSessionId ? (
          <>
            {sendError && (
              <div className="error-banner" role="alert">
                <span>{sendError}</span>
                <button className="error-dismiss" onClick={() => setSendError(null)}>✕</button>
              </div>
            )}

            <div className="messages-area" ref={messagesAreaRef}>
              {hasMoreHistory && (
                <div style={{ textAlign: "center", padding: "8px 0", fontSize: 11, color: "var(--text-muted)" }}>
                  {loadingMoreRef.current ? t("common.loading") : t("chat.loadMoreHistory")}
                </div>
              )}
              {activeMessages.map((msg) => {
                // Render historical chat_ui tool calls as interactive cards
                if (chatUiToolCallIds.has(msg.id)) {
                  const cards = Object.values(historicalCards).filter((c) => c.afterMessageId === msg.id);
                  if (cards.length > 0) {
                    return cards.map((card) => (
                      <div key={card.requestId} className="message message-assistant">
                        <div className="message-role">{t("chat.pisci")}</div>
                        <div className="message-content">
                          {msg.content.trim() && <MessageContent content={msg.content} />}
                          <InteractiveCard
                            requestId={card.requestId}
                            uiDefinition={card.uiDefinition}
                            submittedValues={card.submittedValues}
                          />
                        </div>
                      </div>
                    ));
                  }
                }
                return (
                  <div key={msg.id} className={`message message-${msg.role}`}>
                    <div className="message-role">
                      {msg.role === "user" ? t("chat.you") : t("chat.pisci")}
                    </div>
                    <div className="message-content">
                      <MessageContent content={msg.content} />
                    </div>
                  </div>
                );
              })}

              {activePlan.length > 0 && (
                <div className="tool-steps-container plan-steps-container">
                  <div
                    className={`tool-steps-header${!running ? " tool-steps-header-clickable" : ""}`}
                    onClick={!running ? () => setPlanOpen((o) => !o) : undefined}
                  >
                    <span className="tool-steps-label">
                      {running
                        ? t("chat.planWorking", { count: activePlan.length })
                        : t("chat.planSummary", { count: activePlan.length })}
                    </span>
                    {!running && (
                      <span className="tool-steps-chevron">{planOpen ? "▲" : "▼"}</span>
                    )}
                  </div>
                  {(running || planOpen) && (
                    <div className="tool-steps-scroll">
                      <PlanPanel items={activePlan} />
                    </div>
                  )}
                </div>
              )}

              {/* Tool steps — visible while running; collapses to a single summary line when done */}
              {steps.length > 0 && (
                <div className="tool-steps-container">
                  <div
                    className={`tool-steps-header${!running ? " tool-steps-header-clickable" : ""}`}
                    onClick={!running ? () => setStepsOpen((o) => !o) : undefined}
                  >
                    <span className="tool-steps-label">
                      {running
                        ? t("chat.agentWorking")
                        : t("chat.agentSteps", { count: steps.length })}
                    </span>
                    {!running && (
                      <span className="tool-steps-chevron">{stepsOpen ? "▲" : "▼"}</span>
                    )}
                  </div>
                  {/* Steps body: always visible while running, hidden when done unless user opens */}
                  {(running || stepsOpen) && (
                    <div className="tool-steps-scroll" ref={toolStepsScrollRef}>
                      {steps.map((step) => (
                        <ToolStepCard
                          key={step.id}
                          step={step}
                          onToggle={() => {
                            dispatch(chatActions.toggleToolStep({ sessionId: activeSessionId!, id: step.id }));
                            if (!step.expanded) {
                              requestAnimationFrame(() => {
                                const el = toolStepsScrollRef.current;
                                if (el) {
                                  const cards = el.querySelectorAll<HTMLElement>(".tool-step-card");
                                  const idx = steps.findIndex((s) => s.id === step.id);
                                  if (idx >= 0 && cards[idx]) {
                                    cards[idx].scrollIntoView({ block: "nearest", behavior: "smooth" });
                                  }
                                }
                              });
                            }
                          }}
                        />
                      ))}
                    </div>
                  )}
                </div>
              )}

              {/* Single streaming bubble — shows thinking dots until first text arrives,
                  then displays the latest streamed text. Disappears when running stops.
                  Hidden for IM sessions (headless agent, no real-time text stream). */}
              {running && !isImSession && (
                <div className="message message-assistant streaming-bubble">
                  <div className="message-role">{t("chat.pisci")}</div>
                  <div className="message-content">
                    {streamingCurrent ? (
                      <>
                        <MessageContent content={streamingCurrent} />
                        <span className="cursor-blink">▋</span>
                      </>
                    ) : (
                      <span className="thinking-dots">
                        <span /><span /><span />
                      </span>
                    )}
                  </div>
                </div>
              )}

              {/* Interactive UI cards from chat_ui tool — rendered AFTER the streaming bubble
                  so they appear at the bottom of the conversation, always visible to the user.
                  The agent pauses streaming while waiting for user input, so the streaming
                  bubble is empty/hidden at this point anyway. */}
              {Object.values(interactiveCards).map((card) => (
                <div key={card.requestId} className="message message-assistant">
                  <div className="message-role">{t("chat.pisci")}</div>
                  <div className="message-content">
                    <InteractiveCard
                      requestId={card.requestId}
                      uiDefinition={card.uiDefinition}
                      submittedValues={card.submitted ? undefined : null}
                    />
                  </div>
                </div>
              ))}

              <div ref={messagesEndRef} />
            </div>

            {unreadCount > 0 && (
              <button
                className="chat-unread-badge"
                onClick={() => {
                  messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
                  setUnreadCount(0);
                }}
              >
                ↓ {unreadCount} 条新消息
              </button>
            )}

            {isImSession && (
              <div style={{ padding: "8px 16px", fontSize: 12, color: "var(--text-muted)", borderTop: "1px solid var(--border)", textAlign: "center" }}>
                {t("chat.imSessionHint")}
              </div>
            )}

            {!isImSession && <div
              className={`input-area${isDragging ? " drag-over" : ""}`}
              onDrop={handleDrop}
              onDragOver={handleDragOver}
              onDragLeave={handleDragLeave}
            >
              {isDragging && (
                <div className="drag-overlay">
                  <div className="drag-overlay-text">📎 {t("chat.dropFiles")}</div>
                </div>
              )}
              {/* Attachment preview strip */}
              {attachment && (
                <div className="attachment-preview">
                  {attachmentPreview ? (
                    <img src={attachmentPreview} className="attachment-thumb" alt={attachment.filename} />
                  ) : (
                    <span className="attachment-file-icon">📎</span>
                  )}
                  <span className="attachment-name" title={attachment.path}>
                    {attachment.filename ?? attachment.path}
                  </span>
                  <button className="attachment-remove" onClick={clearAttachment} title={t("chat.removeAttachment")}>✕</button>
                </div>
              )}
              <textarea
                ref={textareaRef}
                className="chat-input"
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={handleKeyDown}
                placeholder={t("chat.inputPlaceholder")}
                rows={3}
                disabled={running}
              />
              <div className="input-actions">
                <button
                  className="btn btn-attach"
                  onClick={handleAttach}
                  disabled={running}
                  title={t("chat.attachFile")}
                >
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"/>
                  </svg>
                </button>
                <button
                  className="btn btn-attach"
                  onClick={handleShowContextPreview}
                  disabled={contextPreviewLoading || !activeSessionId}
                  title={t("chat.debugContextTitle")}
                  style={{ opacity: 0.6, fontSize: 14 }}
                >
                  {contextPreviewLoading ? "…" : "🔍"}
                </button>
                {running ? (
                  <button className="btn btn-danger" onClick={handleCancel}>
                    ⏹ {t("common.stop")}
                  </button>
                ) : (
                  <button
                    className="btn btn-primary"
                    onClick={handleSend}
                    disabled={!input.trim() && !attachment}
                  >
                    {t("common.send")} ↵
                  </button>
                )}
              </div>
            </div>}
          </>
        ) : (
          <div className="empty-state">
            <div className="empty-state-icon">
              <img src="/pisci.png" alt="Pisci" style={{ width: 64, height: 64, objectFit: "contain", borderRadius: 14, opacity: 0.7 }} />
            </div>
            <div className="empty-state-title">{t("chat.welcome")}</div>
            <div className="empty-state-desc">{t("chat.welcomeDesc")}</div>
            <button className="btn btn-primary" onClick={handleNewSession}>
              {t("chat.newChatBtn")}
            </button>
          </div>
        )}
      </div>

      {permissionRequest && (
        <div className="permission-overlay">
          <div className="permission-dialog">
          <h3>{t("chat.permissionTitle")}</h3>
          <p>{permissionRequest.description}</p>
            <div className="tool-info">
              <strong>{permissionRequest.toolName}</strong>
              <pre>{JSON.stringify(permissionRequest.toolInput, null, 2)}</pre>
            </div>
            <div className="actions">
              <button
                className="btn-deny"
                onClick={() => handlePermissionResponse(false)}
              >
                {t("chat.permissionDeny")}
              </button>
              <button
                className="btn-allow"
                onClick={() => handlePermissionResponse(true)}
              >
                {t("chat.permissionAllow")}
              </button>
            </div>
          </div>
        </div>
      )}

      {contextPreview && (
        <div className="permission-overlay" onClick={() => setContextPreview(null)}>
          <div
            className="permission-dialog"
            style={{ maxWidth: 860, width: "92vw", maxHeight: "88vh", display: "flex", flexDirection: "column", padding: 0, overflow: "hidden" }}
            onClick={(e) => e.stopPropagation()}
          >
            {/* Header */}
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", padding: "10px 14px", borderBottom: "1px solid var(--border)", flexShrink: 0 }}>
              <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                <span style={{ fontWeight: 600, fontSize: 13 }}>{t("chat.debugContextTitle")}</span>
                <span style={{ fontSize: 11, color: "var(--text-muted)", background: "var(--bg-secondary)", padding: "2px 7px", borderRadius: 8, border: "1px solid var(--border)" }}>
                  {contextPreview.model}
                </span>
                <span style={{ fontSize: 11, color: "var(--text-muted)" }}>
                  {contextPreview.messages.length} 条消息 · ~{contextPreview.messages_tokens.toLocaleString()} / {contextPreview.context_budget.toLocaleString()} tok
                </span>
                <div style={{ width: 60, height: 4, borderRadius: 2, background: "var(--bg-secondary)", overflow: "hidden" }}>
                  <div style={{
                    height: "100%",
                    width: `${Math.min(100, Math.round(contextPreview.messages_tokens / contextPreview.context_budget * 100))}%`,
                    background: contextPreview.messages_tokens / contextPreview.context_budget > 0.85 ? "#e05c5c" : "var(--accent)",
                    borderRadius: 2,
                  }} />
                </div>
              </div>
              <button
                onClick={() => setContextPreview(null)}
                style={{ background: "none", border: "none", cursor: "pointer", fontSize: 18, color: "var(--text-muted)", lineHeight: 1, padding: "0 4px" }}
              >✕</button>
            </div>

            {/* Message list — no tabs, just the raw LLM context */}
            <div style={{ flex: 1, overflowY: "auto", padding: "10px 14px" }}>
              {contextPreview.messages.length === 0 ? (
                <div style={{ color: "var(--text-muted)", fontSize: 13, padding: "30px 0", textAlign: "center" }}>{t("chat.debugNoMessages")}</div>
              ) : (
                contextPreview.messages.map((msg, msgIdx) => (
                  <div key={msgIdx} style={{ marginBottom: 8, borderRadius: 6, border: "1px solid var(--border)", overflow: "hidden" }}>
                    {/* Role header */}
                    <div style={{
                      display: "flex", justifyContent: "space-between", alignItems: "center",
                      padding: "4px 10px",
                      background: msg.role === "user" ? "rgba(var(--accent-rgb),0.10)" : "var(--bg-secondary)",
                      fontSize: 11, fontWeight: 700, letterSpacing: "0.06em",
                    }}>
                      <span style={{ color: msg.role === "user" ? "var(--accent)" : "var(--text-secondary)", textTransform: "uppercase" }}>
                        {msg.role}
                      </span>
                      <span style={{ color: "var(--text-muted)", fontWeight: 400, fontSize: 11 }}>~{msg.tokens} tok</span>
                    </div>
                    {/* Blocks */}
                    <div style={{ background: "var(--bg-primary)" }}>
                      {msg.blocks.map((block, blockIdx) => {
                        const key = `${msgIdx}-${blockIdx}`;
                        const expanded = expandedBlocks.has(key);
                        const sep = blockIdx > 0 ? { borderTop: "1px solid var(--border)" } : {};
                        if (block.type === "text") {
                          return (
                            <pre key={blockIdx} style={{
                              margin: 0, padding: "8px 10px",
                              fontSize: 12, lineHeight: 1.55,
                              whiteSpace: "pre-wrap", wordBreak: "break-word",
                              color: "var(--text-primary)",
                              ...sep,
                            }}>
                              {block.text || <span style={{ color: "var(--text-muted)", fontStyle: "italic" }}>(empty)</span>}
                            </pre>
                          );
                        }
                        if (block.type === "tool_use") {
                          let inputParsed: Record<string, unknown> | null = null;
                          try { inputParsed = JSON.parse(block.input); } catch { /* raw */ }
                          return (
                            <div key={blockIdx} style={sep}>
                              <button onClick={() => toggleBlock(key)} style={{
                                display: "flex", alignItems: "center", gap: 6, width: "100%",
                                padding: "5px 10px", background: "rgba(120,180,255,0.06)",
                                border: "none", cursor: "pointer", textAlign: "left",
                              }}>
                                <span style={{ fontSize: 11, color: "#7ab4ff", fontFamily: "monospace", fontWeight: 700 }}>⚙ {block.name}</span>
                                <span style={{ fontSize: 10, color: "var(--text-muted)", fontFamily: "monospace" }}>{block.id}</span>
                                <span style={{ marginLeft: "auto", fontSize: 10, color: "var(--text-muted)" }}>{expanded ? "▲" : "▼"}</span>
                              </button>
                              {expanded && (
                                <pre style={{
                                  margin: 0, padding: "6px 10px 8px",
                                  fontSize: 11, lineHeight: 1.5,
                                  whiteSpace: "pre-wrap", wordBreak: "break-word",
                                  color: "var(--text-primary)",
                                  background: "rgba(120,180,255,0.04)",
                                  borderTop: "1px solid var(--border)",
                                }}>
                                  {inputParsed !== null ? JSON.stringify(inputParsed, null, 2) : block.input}
                                </pre>
                              )}
                            </div>
                          );
                        }
                        if (block.type === "tool_result") {
                          const isErr = block.is_error;
                          return (
                            <div key={blockIdx} style={sep}>
                              <button onClick={() => toggleBlock(key)} style={{
                                display: "flex", alignItems: "center", gap: 6, width: "100%",
                                padding: "5px 10px",
                                background: isErr ? "rgba(224,92,92,0.06)" : "rgba(80,200,120,0.06)",
                                border: "none", cursor: "pointer", textAlign: "left",
                              }}>
                                <span style={{ fontSize: 11, fontFamily: "monospace", fontWeight: 700, color: isErr ? "#e05c5c" : "#50c878" }}>
                                  {isErr ? "✗" : "✓"} result
                                </span>
                                <span style={{ fontSize: 10, color: "var(--text-muted)", fontFamily: "monospace" }}>{block.tool_use_id}</span>
                                {block.truncated && <span style={{ fontSize: 10, color: "#e0a050" }}>truncated</span>}
                                <span style={{ marginLeft: "auto", fontSize: 10, color: "var(--text-muted)" }}>{expanded ? "▲" : "▼"}</span>
                              </button>
                              {expanded && (
                                <pre style={{
                                  margin: 0, padding: "6px 10px 8px",
                                  fontSize: 11, lineHeight: 1.5,
                                  whiteSpace: "pre-wrap", wordBreak: "break-word",
                                  color: isErr ? "#e05c5c" : "var(--text-primary)",
                                  background: isErr ? "rgba(224,92,92,0.04)" : "rgba(80,200,120,0.04)",
                                  borderTop: "1px solid var(--border)",
                                  maxHeight: 400, overflowY: "auto",
                                }}>
                                  {block.content}
                                </pre>
                              )}
                            </div>
                          );
                        }
                        if (block.type === "image") {
                          return (
                            <div key={blockIdx} style={{ padding: "5px 10px", fontSize: 11, color: "var(--text-muted)", fontStyle: "italic", ...sep }}>
                              {block.note}
                            </div>
                          );
                        }
                        return null;
                      })}
                    </div>
                  </div>
                ))
              )}
            </div>
          </div>
        </div>
      )}
      <ConfirmDialog
        open={!!deleteTarget}
        title={t("chat.confirmDeleteTitle")}
        message={t("chat.confirmDeleteMessage", { name: deleteTarget?.title ?? "" })}
        confirmLabel={t("common.delete")}
        cancelLabel={t("common.cancel")}
        variant="danger"
        loading={deletingSession}
        onConfirm={confirmDeleteSession}
        onCancel={() => !deletingSession && setDeleteTarget(null)}
      />

      {/* Plan resume dialog — shown when user sends a message while unfinished todos exist */}
      {planResumeDialog && (
        <div
          style={{
            position: "fixed", inset: 0, zIndex: 9999,
            background: "rgba(0,0,0,0.45)",
            display: "flex", alignItems: "center", justifyContent: "center",
          }}
          onClick={() => setPlanResumeDialog(null)}
        >
          <div
            style={{
              background: "var(--bg-primary)", borderRadius: 12,
              padding: "24px 28px", maxWidth: 420, width: "90%",
              boxShadow: "0 8px 32px rgba(0,0,0,0.3)",
              border: "1px solid var(--border)",
            }}
            onClick={(e) => e.stopPropagation()}
          >
            <div style={{ fontSize: 15, fontWeight: 600, color: "var(--text-primary)", marginBottom: 10 }}>
              {t("chat.planResumeTitle")}
            </div>
            <div style={{ fontSize: 13, color: "var(--text-secondary)", marginBottom: 20, lineHeight: 1.5 }}>
              {t("chat.planResumeMessage", {
                count: activePlan.filter(i => i.status === "pending" || i.status === "in_progress").length
              })}
            </div>
            <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              <button
                onClick={async () => {
                  const { pendingContent, pendingAttachment } = planResumeDialog;
                  setPlanResumeDialog(null);
                  await doSend(pendingContent, pendingAttachment, false);
                }}
                style={{
                  padding: "8px 16px", fontSize: 13, fontWeight: 600,
                  background: "var(--accent)", color: "#fff",
                  border: "none", borderRadius: 6, cursor: "pointer", textAlign: "left",
                }}
              >
                {t("chat.planResumeContinue")}
              </button>
              <button
                onClick={async () => {
                  const { pendingContent, pendingAttachment } = planResumeDialog;
                  setPlanResumeDialog(null);
                  await doSend(pendingContent, pendingAttachment, true);
                }}
                style={{
                  padding: "8px 16px", fontSize: 13, fontWeight: 600,
                  background: "#dc3545", color: "#fff",
                  border: "none", borderRadius: 6, cursor: "pointer", textAlign: "left",
                }}
              >
                {t("chat.planResumeClear")}
              </button>
              <button
                onClick={() => setPlanResumeDialog(null)}
                style={{
                  padding: "8px 16px", fontSize: 13,
                  background: "var(--bg-secondary)", color: "var(--text-secondary)",
                  border: "1px solid var(--border)", borderRadius: 6, cursor: "pointer", textAlign: "left",
                }}
              >
                {t("chat.planResumeCancelSend")}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

import { linkifyPaths, stripSendMarkers, isLocalPath, uriToNativePath } from "../../utils/linkify";

// Renders message content with full Markdown support (GFM: tables, strikethrough, task lists, etc.)
function MessageContent({ content }: { content: string }) {
  const processed = linkifyPaths(stripSendMarkers(content));
  const fallback = (
    <pre className="code-block">
      <span className="code-lang">text</span>
      <code>{content}</code>
    </pre>
  );
  return (
    <div className="markdown-body">
      <RenderErrorBoundary fallback={fallback}>
        <ReactMarkdown
          remarkPlugins={[remarkGfm]}
          urlTransform={(url) => url.startsWith("file://") ? url : (url.startsWith("http://") || url.startsWith("https://") || url.startsWith("mailto:") || url.startsWith("#") || url.startsWith("/") || !url.includes(":")) ? url : ""}
          components={{
            // Local paths → shell.open(); web URLs → new tab
            a: ({ href, children }) => {
              if (isLocalPath(href)) {
                return (
                  <a
                    href="#"
                    title={href}
                    style={{ cursor: "pointer" }}
                    onClick={(e) => {
                      e.preventDefault();
                      openPath(uriToNativePath(href!)).catch(console.error);
                    }}
                  >
                    {children}
                  </a>
                );
              }
              if (!href) return <span>{children}</span>;
              return <a href={href} target="_blank" rel="noopener noreferrer">{children}</a>;
            },
            // Code blocks with language label; mermaid gets special rendering
            code: ({ className, children, ...props }) => {
              const isBlock = !!className;
              const lang = className?.replace("language-", "") ?? "";
              if (isBlock) {
                if (lang === "mermaid") {
                  return <MermaidBlock code={String(children).trimEnd()} />;
                }
                return (
                  <pre className="code-block">
                    {lang && <span className="code-lang">{lang}</span>}
                    <code>{children}</code>
                  </pre>
                );
              }
              // Inline code: if it looks like a local file path, render as clickable link
              const text = String(children);
              if (isLocalPath(text)) {
                const uri = `file:///${text.replace(/\\/g, "/").replace(/^\//, "")}`;
                return (
                  <a
                    href="#"
                    title={text}
                    style={{ cursor: "pointer" }}
                    onClick={(e) => {
                      e.preventDefault();
                      openPath(uriToNativePath(uri)).catch(console.error);
                    }}
                  >
                    {text}
                  </a>
                );
              }
              return <code className="inline-code" {...props}>{children}</code>;
            },
            // Tables: wrap in a scrollable container so wide tables don't stretch the bubble
            table: ({ children }) => (
              <div className="table-scroll-wrapper">
                <table>{children}</table>
              </div>
            ),
            // Inline images — clickable for full-size view
            img: ({ src, alt }) => (
              <img
                src={src}
                alt={alt || "image"}
                className="message-image"
                onClick={(e) => {
                  const w = window.open();
                  if (w) { w.document.write(`<img src="${src}" style="max-width:100%">`); }
                  e.stopPropagation();
                }}
              />
            ),
          }}
        >
          {processed}
        </ReactMarkdown>
      </RenderErrorBoundary>
    </div>
  );
}

// ─── Tool step card ───────────────────────────────────────────────────────────

const TOOL_ICONS: Record<string, string> = {
  shell: "💻", powershell: "💻", powershell_query: "💻",
  file_read: "📄", file_write: "📝",
  web_search: "🔍",
  browser: "🌐",
  screen_capture: "📸",
  uia: "🖱️",
  wmi: "🔧",
  com: "📋",
  office: "📊",
  plan_todo: "🗂️",
};

function toolIcon(name: string): string {
  return TOOL_ICONS[name] ?? "⚙️";
}

/** Summarise tool input into a one-line description */
function toolSummary(name: string, input: unknown): string {
  const i = input as Record<string, unknown>;
  if (!i) return name;
  if (name === "browser") {
    const parts = [i["action"]];
    if (i["url"]) parts.push(String(i["url"]).slice(0, 60));
    else if (i["selector"]) parts.push(String(i["selector"]).slice(0, 40));
    return parts.filter(Boolean).join(" → ");
  }
  if (name === "shell" || name === "powershell") return String(i["command"] ?? "").slice(0, 80);
  if (name === "file_read" || name === "file_write") return String(i["path"] ?? "").slice(0, 80);
  if (name === "web_search") return String(i["query"] ?? "").slice(0, 80);
  if (name === "screen_capture") return String(i["mode"] ?? "fullscreen");
  return Object.entries(i).slice(0, 2).map(([k, v]) => `${k}=${String(v).slice(0, 30)}`).join(" ");
}

function planStatusLabel(t: ReturnType<typeof useTranslation>["t"], status: PlanTodoItem["status"]): string {
  switch (status) {
    case "pending":
      return t("chat.planPending");
    case "in_progress":
      return t("chat.planInProgress");
    case "completed":
      return t("chat.planCompleted");
    case "cancelled":
      return t("chat.planCancelled");
    default:
      return status;
  }
}

function PlanPanel({ items }: { items: PlanTodoItem[] }) {
  const { t } = useTranslation();
  return (
    <div className="plan-panel">
      {items.map((item, index) => (
        <div key={item.id} className={`plan-item plan-${item.status}`}>
          <div className="plan-item-left">
            <span className="plan-item-index">{index + 1}</span>
            <span className="plan-item-content">{item.content}</span>
          </div>
          <div className="plan-item-right">
            <span className="plan-item-id">{item.id}</span>
            <span className={`plan-item-status plan-status-${item.status}`}>
              {item.status === "in_progress" && <span className="step-spinner" style={{ width: 10, height: 10, marginRight: 4 }} />}
              {planStatusLabel(t, item.status)}
            </span>
          </div>
        </div>
      ))}
    </div>
  );
}

function FishProgressBadge({ progress }: { progress: NonNullable<ToolStep["fishProgress"]> }) {
  const statusLabel: Record<string, string> = {
    thinking: "思考中",
    thinking_text: "思考中",
    tool_call: "调用工具",
    tool_done: "工具完成",
    done: "已完成",
  };
  const label = statusLabel[progress.status] ?? progress.status;
  const isRunning = progress.status !== "done";
  const showThinking = isRunning && progress.thinkingText;

  return (
    <div className="fish-progress-badge">
      <span className="fish-progress-icon">🐠</span>
      <span className="fish-progress-name">{progress.fishName}</span>
      {progress.iteration > 0 && (
        <span className="fish-progress-iter">第 {progress.iteration} 步</span>
      )}
      {progress.toolName && (
        <span className="fish-progress-tool">{progress.toolName}</span>
      )}
      <span className={`fish-progress-status ${isRunning ? "fish-status-running" : "fish-status-done"}`}>
        {isRunning && <span className="step-spinner" style={{ width: 10, height: 10, marginRight: 4 }} />}
        {label}
      </span>
      {showThinking && (
        <span className="fish-progress-thinking">{progress.thinkingText}</span>
      )}
    </div>
  );
}

function ToolStepCard({ step, onToggle }: { step: ToolStep; onToggle: () => void }) {
  const { t } = useTranslation();
  const maxResultLen = 400;
  const result = step.result ?? "";
  const truncated = result.length > maxResultLen;
  const [showFull, setShowFull] = useState(false);

  const statusClass = !step.completed
    ? "step-running"
    : step.isError
    ? "step-error"
    : "step-ok";

  const statusIcon = !step.completed ? (
    <span className="step-spinner" aria-label="running" />
  ) : step.isError ? (
    <span className="step-status-icon">✕</span>
  ) : (
    <span className="step-status-icon">✓</span>
  );

  return (
    <div className={`tool-step-card ${statusClass}`}>
      <button className="tool-step-header" onClick={onToggle} aria-expanded={step.expanded}>
        <span className="tool-step-icon">{toolIcon(step.name)}</span>
        <span className="tool-step-name">{step.name}</span>
        <span className="tool-step-summary">{toolSummary(step.name, step.input)}</span>
        <span className={`tool-step-status ${statusClass}`}>{statusIcon}</span>
        <span className="tool-step-chevron">{step.expanded ? "▲" : "▼"}</span>
      </button>

      {/* Fish progress inline — shown even when step is collapsed */}
      {step.fishProgress && step.fishProgress.status !== "done" && (
        <FishProgressBadge progress={step.fishProgress} />
      )}

      {step.expanded && (
        <div className="tool-step-body">
          {/* Fish progress detail when expanded */}
          {step.fishProgress && (
            <div className="tool-step-section">
              <span className="tool-step-section-label">🐠 小鱼进度</span>
              <FishProgressBadge progress={step.fishProgress} />
            </div>
          )}
          <div className="tool-step-section">
            <span className="tool-step-section-label">{t("chat.toolStepInput")}</span>
            <pre className="tool-step-pre">
              {typeof step.input === "string"
                ? step.input
                : JSON.stringify(step.input, null, 2)}
            </pre>
          </div>
          {step.completed && (
            <div className="tool-step-section">
              <span className={`tool-step-section-label ${step.isError ? "label-error" : ""}`}>
                {step.isError ? t("chat.toolStepError") : t("chat.toolStepOutput")}
              </span>
              <pre className={`tool-step-pre ${step.isError ? "pre-error" : ""}`}>
                {showFull || !truncated ? result : result.slice(0, maxResultLen) + "…"}
              </pre>
              {truncated && (
                <button
                  className="tool-step-show-more"
                  onClick={(e) => { e.stopPropagation(); setShowFull(!showFull); }}
                >
                  {showFull
                    ? t("chat.toolStepCollapse")
                    : t("chat.toolStepExpand", { count: result.length })}
                </button>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
