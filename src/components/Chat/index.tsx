import { useEffect, useRef, useState, useCallback } from "react";
import { useDispatch, useSelector } from "react-redux";
import { useTranslation } from "react-i18next";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { RootState, chatActions, sessionsActions, ToolStep } from "../../store";
import { chatApi, sessionsApi, AgentEventType } from "../../services/tauri";
import "./Chat.css";

/** Map a session.source value to a compact display emoji/label. */
function sourceIcon(source: string): string {
  if (source === "chat" || !source) return "";
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
  const { messagesBySession, streamingText, toolSteps, isRunning } = useSelector(
    (s: RootState) => s.chat
  );

  const [input, setInput] = useState("");
  const [sendError, setSendError] = useState<string | null>(null);
  // "all" | "chat" | "im"
  const [sessionFilter, setSessionFilter] = useState<"all" | "chat" | "im">("all");
  const [permissionRequest, setPermissionRequest] = useState<{
    requestId: string;
    toolName: string;
    toolInput: any;
    description: string;
  } | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const toolStepsScrollRef = useRef<HTMLDivElement>(null);
  const unlistenRef = useRef<UnlistenFn | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  // Keep a ref to the current sessionId so event callbacks always see the latest value
  const activeSessionIdRef = useRef<string | null>(activeSessionId);
  useEffect(() => {
    activeSessionIdRef.current = activeSessionId;
  }, [activeSessionId]);

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

  const activeMessages = activeSessionId ? messagesBySession[activeSessionId] ?? [] : [];
  const streamingContent = activeSessionId ? streamingText[activeSessionId] ?? "" : "";
  const running = activeSessionId ? isRunning[activeSessionId] ?? false : false;
  const steps = activeSessionId ? toolSteps[activeSessionId] ?? [] : [];

  // Load messages when session changes
  useEffect(() => {
    if (!activeSessionId) return;
    sessionsApi.getMessages(activeSessionId).then((messages) => {
      dispatch(chatActions.setMessages({ sessionId: activeSessionId, messages }));
    });
  }, [activeSessionId, dispatch]);

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
          dispatch(chatActions.setRunning({ sessionId: sid, running: false }));
          // Backend persists the assistant message BEFORE emitting Done (race condition fix).
          // Reload from DB then clear streaming text. Tool steps are kept for review.
          sessionsApi.getMessages(sid).then((messages) => {
            dispatch(chatActions.setMessages({ sessionId: sid, messages }));
            dispatch(chatActions.clearStreaming(sid));
          }).catch(() => {
            dispatch(chatActions.clearStreaming(sid));
          });
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

  // Auto-scroll messages to bottom when new messages or streaming text arrive
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [activeMessages, streamingContent]);

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

  const handleDeleteSession = useCallback(async (e: React.MouseEvent, sessionId: string) => {
    e.stopPropagation();
    try {
      await sessionsApi.delete(sessionId);
      dispatch(sessionsActions.removeSession(sessionId));
      // If we deleted the active session, switch to another or clear
      if (activeSessionId === sessionId) {
        const remaining = sessions.filter((s) => s.id !== sessionId);
        dispatch(sessionsActions.setActiveSession(remaining.length > 0 ? remaining[0].id : null));
      }
    } catch (e) {
      setSendError(t("chat.failedDelete", { error: String(e) }));
    }
  }, [activeSessionId, sessions, dispatch, t]);

  const handleSend = useCallback(async () => {
    if (!input.trim() || !activeSessionId || running) return;

    const content = input.trim();
    setInput("");
    setSendError(null);

    // Clear tool steps from the previous turn before starting a new one
    dispatch(chatActions.clearToolSteps(activeSessionId));

    // Auto-title: if this is the first message in the session, derive a title from it
    const currentMessages = messagesBySession[activeSessionId] ?? [];
    if (currentMessages.length === 0) {
      const raw = content.replace(/\s+/g, " ").trim();
      const title = raw.length > 30 ? raw.slice(0, 30) + "…" : raw;
      sessionsApi.rename(activeSessionId, title).catch(() => {});
      dispatch(sessionsActions.updateSessionTitle({ id: activeSessionId, title }));
    }

    // Optimistically add user message
    dispatch(chatActions.appendMessage({
      sessionId: activeSessionId,
      message: {
        id: Date.now().toString(),
        session_id: activeSessionId,
        role: "user",
        content,
        created_at: new Date().toISOString(),
      },
    }));

    dispatch(chatActions.setRunning({ sessionId: activeSessionId, running: true }));

    try {
      console.log('[Chat] sending message to session:', activeSessionId);
      // chat_send now returns immediately (agent runs in background).
      // The event listener is already registered via the useEffect above,
      // so no race condition — events will arrive after this call returns.
      await chatApi.send(activeSessionId, content);
      console.log('[Chat] chat_send returned (agent running in background)');
    } catch (e) {
      console.error('[Chat] chat_send error:', e);
      dispatch(chatActions.setRunning({ sessionId: activeSessionId, running: false }));
      dispatch(chatActions.clearStreaming(activeSessionId));
      setSendError(`${e}`);
    }
  }, [input, activeSessionId, running, dispatch]);

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

        {sessions
          .filter((s) => {
            if (sessionFilter === "chat") return !s.source || s.source === "chat";
            if (sessionFilter === "im") return s.source && s.source !== "chat";
            return true;
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
                  {s.title ?? t("chat.defaultTitle")}
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
        {sessions.length === 0 && (
          <div className="session-empty">{t("chat.noChats")}</div>
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

            <div className="messages-area">
              {activeMessages.map((msg) => (
                <div key={msg.id} className={`message message-${msg.role}`}>
                  <div className="message-role">
                    {msg.role === "user" ? t("chat.you") : t("chat.pisci")}
                  </div>
                  <div className="message-content">
                    <MessageContent content={msg.content} />
                  </div>
                </div>
              ))}

              {/* Tool steps — persist after completion, user can expand/collapse each */}
              {steps.length > 0 && (
                <div className="tool-steps-container">
                  <div className="tool-steps-header">
                    <span className="tool-steps-label">
                      {running
                        ? t("chat.agentWorking")
                        : t("chat.agentSteps", { count: steps.length })}
                    </span>
                    {!running && (
                      <button
                        className="tool-steps-toggle-all"
                        onClick={() => {
                          const allExpanded = steps.every((s) => s.expanded);
                          steps.forEach((s) =>
                            dispatch(chatActions.toggleToolStep({ sessionId: activeSessionId!, id: s.id }))
                          );
                          // After expanding all, scroll to the bottom of the steps area
                          if (!allExpanded) {
                            requestAnimationFrame(() => {
                              const el = toolStepsScrollRef.current;
                              if (el) el.scrollTop = el.scrollHeight;
                            });
                          }
                        }}
                      >
                        {steps.every((s) => s.expanded) ? t("chat.collapseAll") : t("chat.expandAll")}
                      </button>
                    )}
                  </div>
                  {/* Scrollable body — isolated from flex container height constraints */}
                  <div className="tool-steps-scroll" ref={toolStepsScrollRef}>
                    {steps.map((step) => (
                      <ToolStepCard
                        key={step.id}
                        step={step}
                        onToggle={() => {
                          dispatch(chatActions.toggleToolStep({ sessionId: activeSessionId!, id: step.id }));
                          // If expanding, scroll this step into view after render
                          if (!step.expanded) {
                            requestAnimationFrame(() => {
                              const el = toolStepsScrollRef.current;
                              if (el) {
                                // Find the toggled step's DOM element and scroll it visible
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
                </div>
              )}

              {/* Streaming text bubble */}
              {streamingContent && (
                <div className="message message-assistant">
                  <div className="message-role">{t("chat.pisci")}</div>
                  <div className="message-content">
                    <MessageContent content={streamingContent} />
                    <span className="cursor-blink">▋</span>
                  </div>
                </div>
              )}

              <div ref={messagesEndRef} />
            </div>

            <div className="input-area">
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
                {running ? (
                  <button className="btn btn-danger" onClick={handleCancel}>
                    ⏹ {t("common.stop")}
                  </button>
                ) : (
                  <button
                    className="btn btn-primary"
                    onClick={handleSend}
                    disabled={!input.trim()}
                  >
                    {t("common.send")} ↵
                  </button>
                )}
              </div>
            </div>
          </>
        ) : (
          <div className="empty-state">
            <div className="empty-state-icon">🐟</div>
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
    </div>
  );
}

// Renders message content with basic markdown: code blocks and inline text
function MessageContent({ content }: { content: string }) {
  const segments: React.ReactNode[] = [];
  const lines = content.split("\n");
  let i = 0;
  let key = 0;

  while (i < lines.length) {
    const line = lines[i];

    if (line.startsWith("```")) {
      // Collect code block lines
      const lang = line.slice(3).trim();
      const codeLines: string[] = [];
      i++;
      while (i < lines.length && !lines[i].startsWith("```")) {
        codeLines.push(lines[i]);
        i++;
      }
      i++; // skip closing ```
      segments.push(
        <pre key={key++} className="code-block">
          {lang && <span className="code-lang">{lang}</span>}
          <code>{codeLines.join("\n")}</code>
        </pre>
      );
    } else {
      // Regular text line
      segments.push(
        <span key={key++}>
          {line}
          {i < lines.length - 1 && <br />}
        </span>
      );
      i++;
    }
  }

  return <>{segments}</>;
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
