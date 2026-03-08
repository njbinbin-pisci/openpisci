import { useEffect, useRef, useState, useCallback } from "react";
import { useDispatch, useSelector } from "react-redux";
import { useTranslation } from "react-i18next";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { readFile } from "@tauri-apps/plugin-fs";
import { RootState, chatActions, sessionsActions, ToolStep, StreamingState } from "../../store";
import { chatApi, sessionsApi, gatewayApi, fishApi, AgentEventType, ChannelInfo, ChatAttachment } from "../../services/tauri";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import mermaid from "mermaid";
import "./Chat.css";

// ─── Mermaid diagram block ────────────────────────────────────────────────────
mermaid.initialize({ startOnLoad: false, theme: "dark", securityLevel: "loose" });

let mermaidIdCounter = 0;

function MermaidBlock({ code }: { code: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const idRef = useRef(`mermaid-${++mermaidIdCounter}`);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!ref.current) return;
    const id = idRef.current;
    setError(null);
    mermaid.render(id, code)
      .then(({ svg }) => {
        if (ref.current) ref.current.innerHTML = svg;
      })
      .catch((e) => {
        setError(String(e));
      });
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

/** Map a session.source value to a compact display emoji/label. */
function sourceIcon(source: string): string {
  if (source === "chat" || !source) return "";
  if (source.startsWith("fish_")) return "🐠";
  if (source.includes("telegram")) return "✈";
  if (source.includes("feishu") || source.includes("lark")) return "🪶";
  if (source.includes("wecom") || source.includes("wechat")) return "💬";
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
  const { messagesBySession, streaming, toolSteps, isRunning } = useSelector(
    (s: RootState) => s.chat
  );

  const [input, setInput] = useState("");
  const [sendError, setSendError] = useState<string | null>(null);
  // "all" | "chat" | "fish" | "im"
  const [sessionFilter, setSessionFilter] = useState<"all" | "chat" | "fish" | "im">("all");

  // Attachment state
  const [attachment, setAttachment] = useState<ChatAttachment | null>(null);
  // Preview URL for image attachments (object URL or base64 data URL)
  const [attachmentPreview, setAttachmentPreview] = useState<string | null>(null);
  // Set of fish IDs that are currently activated (have an active instance)
  const [activeFishIds, setActiveFishIds] = useState<Set<string>>(new Set());
  const [gatewayChannels, setGatewayChannels] = useState<ChannelInfo[]>([]);
  const [gatewayConnecting, setGatewayConnecting] = useState(false);
  const [gatewayDisconnecting, setGatewayDisconnecting] = useState(false);
  // History pagination for IM sessions: how many messages have been loaded
  const [historyLimit, setHistoryLimit] = useState(100);
  const [hasMoreHistory, setHasMoreHistory] = useState(false);
  const [permissionRequest, setPermissionRequest] = useState<{
    requestId: string;
    toolName: string;
    toolInput: any;
    description: string;
  } | null>(null);

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

  const activeMessages = (activeSessionId ? messagesBySession[activeSessionId] ?? [] : [])
    // Filter out tool-result carrier messages (role=user, no text content, only tool_results_json)
    .filter((m) => !(m.role === "user" && !m.content.trim() && m.tool_results_json))
    // Filter out pure tool-call assistant messages (no text content, only tool_calls_json).
    // Keep assistant messages that have actual text content even if they also have tool_calls_json.
    .filter((m) => !(m.role === "assistant" && !m.content.trim() && m.tool_calls_json))
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
  const activeSession = sessions.find((s) => s.id === activeSessionId);

  // Tool steps panel: open while running, auto-close when agent finishes
  const [stepsOpen, setStepsOpen] = useState(false);
  const prevRunningRef = useRef(false);
  useEffect(() => {
    if (running && !prevRunningRef.current) {
      // Agent just started — open the steps panel
      setStepsOpen(true);
    } else if (!running && prevRunningRef.current) {
      // Agent just finished — hide the steps panel
      setStepsOpen(false);
    }
    prevRunningRef.current = running;
  }, [running]);
  const isFishSession = !!(activeSession?.source && activeSession.source.startsWith("fish_"));
  const isImSession = !!(activeSession?.source && activeSession.source !== "chat" && !isFishSession);
  isImSessionRef.current = isImSession;
  // True only when the fish session's fish is currently activated
  const activeFishId = isFishSession ? activeSession!.source.replace(/^fish_/, "") : null;
  const isFishActivated = activeFishId ? activeFishIds.has(activeFishId) : false;

  // Load messages when the active session ID changes.
  // Also sync running state from DB to fix stale state if im_session_done was missed.
  useEffect(() => {
    if (!activeSessionId) return;
    setHistoryLimit(100);
    isNearBottomRef.current = true;

    const load = async () => {
      try {
        const [messages, { sessions: fresh }] = await Promise.all([
          sessionsApi.getMessages(activeSessionId, 100, 0),
          sessionsApi.list(),
        ]);
        dispatch(chatActions.setMessages({ sessionId: activeSessionId, messages }));
        setHasMoreHistory(messages.length >= 100);
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

  // Load more history (older messages) for IM sessions
  const loadMoreHistory = useCallback(() => {
    if (!activeSessionId) return;
    const el = messagesAreaRef.current;
    // Snapshot scroll position before loading so we can restore it after
    const scrollHeightBefore = el?.scrollHeight ?? 0;
    const scrollTopBefore = el?.scrollTop ?? 0;
    loadingMoreRef.current = true;
    const newLimit = historyLimit + 100;
    setHistoryLimit(newLimit);
    sessionsApi.getMessages(activeSessionId, newLimit, 0).then((messages) => {
      dispatch(chatActions.setMessages({ sessionId: activeSessionId, messages }));
      setHasMoreHistory(messages.length >= newLimit);
      // After React re-renders, restore scroll so the user stays at the same visual position
      requestAnimationFrame(() => {
        if (el) {
          const added = el.scrollHeight - scrollHeightBefore;
          el.scrollTop = scrollTopBefore + added;
        }
        loadingMoreRef.current = false;
      });
    }).catch(() => { loadingMoreRef.current = false; });
  }, [activeSessionId, historyLimit, dispatch]);

  // Auto-adjust filter when the active session changes (depends on sessions for source lookup)
  useEffect(() => {
    if (!activeSessionId) return;
    const activeSession = sessions.find((s) => s.id === activeSessionId);
    if (activeSession?.source && activeSession.source !== "chat") {
      if (activeSession.source.startsWith("fish_")) {
        setSessionFilter("fish");
      } else {
        setSessionFilter("im");
      }
    }
  }, [activeSessionId, sessions]);

  // Subscribe to agent events — use ref to avoid stale closure over activeSessionId
  useEffect(() => {
    if (!activeSessionId) return;

    // Cleanup previous listener synchronously before registering the new one
    if (unlistenRef.current) {
      unlistenRef.current();
      unlistenRef.current = null;
    }

    let cancelled = false;

    console.log('[Chat] registering event listener for session:', activeSessionId);
    chatApi.onEvent(activeSessionId, (event: AgentEventType) => {
      console.log('[Chat] received event:', event.type, 'for session:', activeSessionIdRef.current);
      // Always read the ref so we dispatch to the correct (current) session
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
        case "permission_request":
          setPermissionRequest({
            requestId: event.request_id,
            toolName: event.tool_name,
            toolInput: event.tool_input,
            description: event.description,
          });
          break;
        case "done":
          console.log('[Chat] agent done event, sid=', sid, 'isImSession=', isImSessionRef.current);
          dispatch(chatActions.setRunning({ sessionId: sid, running: false }));
          dispatch(chatActions.clearStreaming(sid));
          dispatch(chatActions.removeOptimisticMessages(sid));
          if (!isImSessionRef.current) {
            // Regular chat: reload from DB immediately (persist is synchronous before Done event)
            sessionsApi.getMessages(sid).then((messages) => {
              console.log('[Chat] done: reloaded', messages.length, 'messages for', sid);
              dispatch(chatActions.setMessages({ sessionId: sid, messages }));
            }).catch(() => {});
          }
          // IM sessions: App.tsx listens for im_session_done (emitted AFTER persist_agent_turn),
          // which will reload messages. No action needed here.
          break;
        case "error":
          dispatch(chatActions.setRunning({ sessionId: sid, running: false }));
          dispatch(chatActions.clearStreaming(sid));
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

  // Track whether user is near the bottom of the messages area
  useEffect(() => {
    const el = messagesAreaRef.current;
    if (!el) return;
    const onScroll = () => {
      const threshold = 120;
      isNearBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < threshold;
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, []);

  // Auto-scroll to bottom only when user is near the bottom and not loading history
  useEffect(() => {
    if (loadingMoreRef.current) return;
    if (!isNearBottomRef.current) return;
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [activeMessages, streamingCurrent]);

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

  // Load activated fish list on mount and when switching to fish filter
  useEffect(() => {
    fishApi.list().then((list) => {
      setActiveFishIds(new Set(list.filter((f) => !!f.instance).map((f) => f.id)));
    }).catch(() => {});
  }, [sessionFilter]);

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

  const handleDeleteSession = useCallback(async (e: React.MouseEvent, sessionId: string) => {
    e.stopPropagation();
    try {
      await sessionsApi.delete(sessionId);
      dispatch(sessionsActions.removeSession(sessionId));
      // If we deleted the active session, switch to the next session
      // within the CURRENT filter to avoid jumping to a different tab
      if (activeSessionId === sessionId) {
        const filterSession = (s: typeof sessions[0]) => {
          const isFish = !!(s.source && s.source.startsWith("fish_"));
          const fishActivated = isFish && activeFishIds.has(s.source!.replace(/^fish_/, ""));
          if (sessionFilter === "chat") return !s.source || s.source === "chat";
          if (sessionFilter === "fish") return isFish && fishActivated;
          if (sessionFilter === "im") return !!(s.source && s.source !== "chat" && !isFish);
          return !isFish || fishActivated;
        };
        const remaining = sessions.filter((s) => s.id !== sessionId && filterSession(s));
        dispatch(sessionsActions.setActiveSession(remaining.length > 0 ? remaining[0].id : null));
      }
    } catch (e) {
      setSendError(t("chat.failedDelete", { error: String(e) }));
    }
  }, [activeSessionId, sessions, sessionFilter, activeFishIds, dispatch, t]);

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

  const handleSend = useCallback(async () => {
    if ((!input.trim() && !attachment) || !activeSessionId || running) return;

    const content = input.trim();
    setInput("");
    setSendError(null);
    const pendingAttachment = attachment;
    clearAttachment();

    // Clear tool steps and any residual streaming text from the previous turn
    dispatch(chatActions.clearToolSteps(activeSessionId));
    dispatch(chatActions.clearStreaming(activeSessionId));

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

    // Optimistically add user message (id prefixed with "optimistic_" so it can be removed on reload)
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
      if (isFishSession && activeSession?.source) {
        // Fish session: extract fish_id from source "fish_{id}" and call fish_chat_send
        const fishId = activeSession.source.replace(/^fish_/, "");
        await fishApi.chatSend(fishId, content);
      } else {
        await chatApi.send(activeSessionId, content, pendingAttachment ?? undefined);
      }
    } catch (e) {
      console.error('[Chat] send error:', e);
      dispatch(chatActions.setRunning({ sessionId: activeSessionId, running: false }));
      dispatch(chatActions.clearStreaming(activeSessionId));
      setSendError(`${e}`);
    }
  }, [input, attachment, activeSessionId, running, isFishSession, activeSession, dispatch, clearAttachment, t]);

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
          {(["all", "chat", "fish", "im"] as const).map((f) => (
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
              {f === "all" ? t("chat.filterAll") : f === "chat" ? t("chat.filterChat") : f === "fish" ? t("chat.filterFish") : t("chat.filterIM")}
            </button>
          ))}
        </div>

        <div style={{ flex: 1, overflowY: "auto" }}>
          {sessions
            .filter((s) => {
              const isFish = !!(s.source && s.source.startsWith("fish_"));
              const fishActivated = isFish && activeFishIds.has(s.source!.replace(/^fish_/, ""));
              if (sessionFilter === "chat") return !s.source || s.source === "chat";
              if (sessionFilter === "fish") return isFish && fishActivated;
              if (sessionFilter === "im") return !!(s.source && s.source !== "chat" && !isFish);
              // "all": chat + activated fish + IM
              return !isFish || fishActivated;
            })
            .map((s) => {
              const icon = sourceIcon(s.source);
              return (
                <div
                  key={s.id}
                  className={`session-item ${s.id === activeSessionId ? "active" : ""}`}
                  onClick={() => dispatch(sessionsActions.setActiveSession(s.id))}
                >
                  <span className="session-title">
                    {icon && <span style={{ marginRight: 4, fontSize: 12 }}>{icon}</span>}
                    {(s.title ?? t("chat.defaultTitle")).replace(/^🐠\s*/, "")}
                  </span>
                  <span className="session-item-right">
                    <span className="session-count">{s.message_count}</span>
                    <button
                      className="session-delete-btn"
                      title={t("chat.deleteChat")}
                      onClick={(e) => handleDeleteSession(e, s.id)}
                    >✕</button>
                  </span>
                </div>
              );
            })}
          {sessions.filter((s) => {
            const isFish = !!(s.source && s.source.startsWith("fish_"));
            const fishActivated = isFish && activeFishIds.has(s.source!.replace(/^fish_/, ""));
            if (sessionFilter === "chat") return !s.source || s.source === "chat";
            if (sessionFilter === "fish") return isFish && fishActivated;
            if (sessionFilter === "im") return !!(s.source && s.source !== "chat" && !isFish);
            return !isFish || fishActivated;
          }).length === 0 && (
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

            {isFishSession && (
              <div style={{
                padding: "6px 16px",
                fontSize: 12,
                color: "var(--text-secondary)",
                background: "var(--bg-secondary)",
                borderBottom: "1px solid var(--border)",
                display: "flex",
                alignItems: "center",
                gap: 6,
              }}>
                <span style={{ fontSize: 16 }}>🐠</span>
                <span style={{ fontWeight: 500 }}>{(activeSession?.title ?? t("chat.fishSession")).replace(/^🐠\s*/, "")}</span>
                <span style={{ marginLeft: "auto", opacity: 0.6, fontSize: 11 }}>{t("chat.fishSessionHint")}</span>
              </div>
            )}

            <div className="messages-area" ref={messagesAreaRef}>
              {isImSession && hasMoreHistory && (
                <div style={{ textAlign: "center", padding: "8px 0" }}>
                  <button
                    className="load-more-btn"
                    onClick={loadMoreHistory}
                  >
                    {t("chat.loadMoreHistory")}
                  </button>
                </div>
              )}
              {activeMessages.map((msg) => (
                <div key={msg.id} className={`message message-${msg.role}`}>
                  <div className="message-role">
                    {msg.role === "user" ? t("chat.you") : isFishSession ? (activeSession?.title ?? t("chat.fishSession")).replace(/^🐠\s*/, "") : t("chat.pisci")}
                  </div>
                  <div className="message-content">
                    <MessageContent content={msg.content} />
                  </div>
                </div>
              ))}

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

              <div ref={messagesEndRef} />
            </div>

            {isImSession && (
              <div style={{ padding: "8px 16px", fontSize: 12, color: "var(--text-muted)", borderTop: "1px solid var(--border)", textAlign: "center" }}>
                {t("chat.imSessionHint")}
              </div>
            )}

            {isFishSession && !isFishActivated && (
              <div style={{ padding: "12px 16px", fontSize: 12, color: "var(--text-muted)", borderTop: "1px solid var(--border)", textAlign: "center", background: "var(--bg-secondary)" }}>
                {t("chat.fishNotActivated")}
              </div>
            )}

            {!isImSession && (!isFishSession || isFishActivated) && <div
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
    </div>
  );
}

// Matches bare local paths not already inside a Markdown link or code span.
// Windows:  C:\foo\bar.txt  or  C:/foo/bar.txt
// UNC:      \\server\share\file.txt
// Unix/Mac: /home/user/file.txt  /Users/foo/bar.md
// The character class excludes whitespace and Markdown-special chars but allows CJK and other Unicode.
const LOCAL_PATH_RE =
  /(?<!\]\()(?<![`\w/\\])(((?:[A-Za-z]:[\\/]|\\\\)[^\s`"'<>[\]()（）【】]+)|(?:\/(?:home|Users|tmp|var|etc|opt|srv|mnt|data)\/[^\s`"'<>[\]()（）【】]+))/g;

function linkifyPaths(text: string): string {
  return text.replace(LOCAL_PATH_RE, (match) => {
    // Skip if already inside a markdown link target or code fence line
    const encoded = match.replace(/\\/g, "/");
    const uri = encoded.startsWith("//")
      ? `file:${encoded}`           // UNC \\server\share → file://server/share
      : `file:///${encoded.replace(/^\//, "")}`; // Unix /home/... or Windows C:/...
    return `[${match}](${uri})`;
  });
}

function isLocalPath(href: string | undefined): boolean {
  if (!href) return false;
  return href.startsWith("file://") || /^[A-Za-z]:[\\/]/.test(href) || href.startsWith("\\\\");
}

// Renders message content with full Markdown support (GFM: tables, strikethrough, task lists, etc.)
function MessageContent({ content }: { content: string }) {
  const processed = linkifyPaths(content);
  return (
    <div className="markdown-body">
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      components={{
        // Local paths → shell.open(); web URLs → new tab
        a: ({ href, children }) => {
          if (isLocalPath(href)) {
            // Decode file:///C:/path → C:\path for shell.open
            const toNativePath = (uri: string) => {
              if (uri.startsWith("file:///")) {
                return decodeURIComponent(uri.slice(8)).replace(/\//g, "\\");
              }
              if (uri.startsWith("file://")) {
                return decodeURIComponent(uri.slice(7));
              }
              return uri; // already a plain path
            };
            return (
              <a
                href="#"
                title={href}
                style={{ cursor: "pointer" }}
                onClick={(e) => {
                  e.preventDefault();
                  shellOpen(toNativePath(href!)).catch(console.error);
                }}
              >
                {children}
              </a>
            );
          }
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
          return <code className="inline-code" {...props}>{children}</code>;
        },
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

      {step.expanded && (
        <div className="tool-step-body">
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
