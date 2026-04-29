import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { ChatMessage, Session, sessionsApi, poolApi, openPath } from "../../../services/tauri";
import { linkifyPaths, isLocalPath, uriToNativePath } from "../../../utils/linkify";
import { isInternalSession } from "../../../utils/session";
import ConfirmDialog from "../../ConfirmDialog";
import "./PisciInbox.css";

const INBOX_INITIAL_SIZE = 200;
const INBOX_LAZY_STEP = 10;
type InboxMode = "coordination" | "koiObserver";

type PisciInboxProps = {
  mode?: InboxMode;
};

function isKoiObserverSession(session: Session): boolean {
  const id = session.id ?? "";
  return id.startsWith("koi_runtime_") || id.startsWith("koi_notify_");
}

function isCoordinationSession(session: Session): boolean {
  return isInternalSession(session) && !isKoiObserverSession(session);
}

function InboxMessageContent({ content }: { content: string }) {
  const processed = linkifyPaths(content);
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      urlTransform={(url) => url.startsWith("file://") ? url : (url.startsWith("http://") || url.startsWith("https://") || url.startsWith("mailto:") || url.startsWith("#") || url.startsWith("/") || !url.includes(":")) ? url : ""}
      components={{
        a: ({ href, children }) => {
          if (isLocalPath(href)) {
            const nativePath = uriToNativePath(href!);
            return (
              <a href="#" title={nativePath}
                onClick={(e) => {
                  e.preventDefault();
                  openPath(nativePath).catch((err) => {
                    console.error("[inbox] openPath failed:", nativePath, err);
                  });
                }}
              >
                {children}
              </a>
            );
          }
          if (!href) return <span>{children}</span>;
          return <a href={href} target="_blank" rel="noopener noreferrer">{children}</a>;
        },
      }}
    >
      {processed}
    </ReactMarkdown>
  );
}


function formatTime(value: string): string {
  try {
    return new Date(value).toLocaleString();
  } catch {
    return value;
  }
}

function inboxMessageRoleLabel(
  t: (key: string) => string,
  mode: InboxMode,
  role: ChatMessage["role"],
): string {
  if (mode === "koiObserver") {
    switch (role) {
      case "assistant":
        return t("pond.observerRoleAssistant");
      case "user":
        return t("pond.observerRoleUser");
      case "system":
        return t("pond.observerRoleSystem");
      case "tool":
        return t("pond.observerRoleTool");
      default:
        return role;
    }
  }
  return role === "assistant" ? t("chat.pisci") : role;
}

function sessionKindLabel(t: (key: string) => string, mode: InboxMode, session: Session): string {
  if (mode === "koiObserver") {
    if (session.id.startsWith("koi_runtime_")) return t("pond.observerRuntime");
    if (session.id.startsWith("koi_notify_")) return t("pond.observerNotify");
    return t("pond.observerInternal");
  }
  if (
    session.id === "heartbeat"
    || session.id === "pisci_inbox_global"
    || session.source === "heartbeat"
    || session.source === "pisci_inbox_global"
  ) {
    return t("pond.inboxGlobal");
  }
  return t("pond.inboxProject");
}

export default function PisciInbox({ mode = "coordination" }: PisciInboxProps) {
  const { t } = useTranslation();
  const [sessions, setSessions] = useState<Session[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [loadingSessions, setLoadingSessions] = useState(false);
  const [loadingMessages, setLoadingMessages] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [confirmTarget, setConfirmTarget] = useState<{ id: string; title: string; blocked: boolean } | null>(null);

  const messagesContainerRef = useRef<HTMLDivElement>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const initialLoadDoneRef = useRef<string | null>(null);

  const sessionFilter = useCallback(
    (session: Session) => (mode === "koiObserver" ? isKoiObserverSession(session) : isCoordinationSession(session)),
    [mode],
  );

  const internalSessions = useMemo(
    () => sessions.filter(sessionFilter),
    [sessions, sessionFilter],
  );

  const loadSessions = useCallback(async () => {
    setLoadingSessions(true);
    try {
      const result = await sessionsApi.list(200, 0);
      const internal = result.sessions.filter(sessionFilter);
      setSessions(result.sessions);
      setActiveSessionId((prev) => {
        if (prev && internal.some((session) => session.id === prev)) return prev;
        return internal[0]?.id ?? null;
      });
    } finally {
      setLoadingSessions(false);
    }
  }, [sessionFilter]);

  const loadMessages = useCallback(async (sessionId: string) => {
    setLoadingMessages(true);
    try {
      const result = await sessionsApi.getMessages(sessionId, INBOX_INITIAL_SIZE, 0);
      setMessages(result);
      setHasMore(result.length === INBOX_INITIAL_SIZE);
      initialLoadDoneRef.current = sessionId;
    } finally {
      setLoadingMessages(false);
    }
  }, []);

  const loadOlderMessages = useCallback(async (sessionId: string, currentCount: number) => {
    if (loadingMore) return;
    setLoadingMore(true);
    try {
      const result = await sessionsApi.getMessages(sessionId, INBOX_LAZY_STEP, currentCount);
      if (result.length > 0) {
        setMessages((prev) => {
          const existingIds = new Set(prev.map((m) => m.id));
          const newOnes = result.filter((m) => !existingIds.has(m.id));
          return [...newOnes, ...prev];
        });
        setHasMore(result.length === INBOX_LAZY_STEP);
      } else {
        setHasMore(false);
      }
    } finally {
      setLoadingMore(false);
    }
  }, [loadingMore]);

  useEffect(() => {
    loadSessions().catch(console.error);
  }, [loadSessions]);

  useEffect(() => {
    if (!activeSessionId) {
      setMessages([]);
      setHasMore(false);
      return;
    }
    loadMessages(activeSessionId).catch(console.error);
  }, [activeSessionId, loadMessages]);

  // After initial load: immediately jump to bottom
  useLayoutEffect(() => {
    if (initialLoadDoneRef.current === activeSessionId && messagesContainerRef.current) {
      messagesContainerRef.current.scrollTop = messagesContainerRef.current.scrollHeight;
      initialLoadDoneRef.current = null;
    }
  });

  const handleScroll = useCallback((e: React.UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    if (el.scrollTop < 60 && hasMore && activeSessionId && !loadingMore) {
      const prevScrollHeight = el.scrollHeight;
      loadOlderMessages(activeSessionId, messages.length).then(() => {
        requestAnimationFrame(() => {
          if (messagesContainerRef.current) {
            messagesContainerRef.current.scrollTop =
              messagesContainerRef.current.scrollHeight - prevScrollHeight;
          }
        });
      });
    }
  }, [hasMore, activeSessionId, loadingMore, loadOlderMessages, messages.length]);

  const requestDeleteSession = useCallback(async (e: React.MouseEvent, session: Session) => {
    e.stopPropagation();
    // Check if this inbox session is linked to an active pool
    let blocked = false;
    if (session.id.startsWith("pisci_pool_")) {
      const poolId = session.id.replace("pisci_pool_", "");
      try {
        const pools = await poolApi.listSessions();
        const pool = pools.find((p) => p.id === poolId);
        if (pool && pool.status === "active") blocked = true;
      } catch { /* ignore */ }
    }
    setConfirmTarget({ id: session.id, title: session.title || session.id, blocked });
  }, []);

  const confirmDeleteSession = useCallback(async () => {
    if (!confirmTarget) return;
    setDeletingId(confirmTarget.id);
    try {
      await sessionsApi.delete(confirmTarget.id);
      setSessions((prev) => prev.filter((s) => s.id !== confirmTarget.id));
      if (activeSessionId === confirmTarget.id) {
        const remaining = sessions.filter((s) => s.id !== confirmTarget.id && sessionFilter(s));
        setActiveSessionId(remaining.length > 0 ? remaining[0].id : null);
        setMessages([]);
      }
      setConfirmTarget(null);
    } catch (err) {
      console.error("Failed to delete session:", err);
    } finally {
      setDeletingId(null);
    }
  }, [confirmTarget, activeSessionId, sessionFilter, sessions]);

  const copy = mode === "koiObserver"
    ? {
        title: t("pond.observerTitle"),
        subtitle: t("pond.observerDesc"),
        empty: t("pond.observerEmpty"),
        selectHint: t("pond.observerSelectHint"),
        readonly: t("pond.observerReadonly"),
        noMessages: t("pond.observerNoMessages"),
        deleteTitle: t("pond.observerDeleteTitle"),
        deleteMessage: t("pond.observerDeleteMessage", { name: confirmTarget?.title ?? "" }),
      }
    : {
        title: t("pond.inboxTitle"),
        subtitle: t("pond.inboxDesc"),
        empty: t("pond.inboxEmpty"),
        selectHint: t("pond.inboxSelectHint"),
        readonly: t("pond.inboxReadonly"),
        noMessages: t("pond.inboxNoMessages"),
        deleteTitle: t("pond.inboxDeleteTitle"),
        deleteMessage: t("pond.inboxDeleteMessage", { name: confirmTarget?.title ?? "" }),
      };

  const activeSession = internalSessions.find((session) => session.id === activeSessionId) ?? null;

  return (
    <div className="pisci-inbox">
      <div className="pisci-inbox-sidebar">
        <div className="pisci-inbox-sidebar-header">
          <div>
            <div className="pisci-inbox-title">{copy.title}</div>
            <div className="pisci-inbox-subtitle">{copy.subtitle}</div>
          </div>
          <button className="pisci-inbox-refresh" onClick={() => loadSessions().catch(console.error)}>
            {t("pond.inboxRefresh")}
          </button>
        </div>

        <div className="pisci-inbox-session-list">
          {loadingSessions && internalSessions.length === 0 && (
            <div className="pisci-inbox-empty">{t("common.loading")}</div>
          )}
          {!loadingSessions && internalSessions.length === 0 && (
            <div className="pisci-inbox-empty">{copy.empty}</div>
          )}
          {internalSessions.map((session) => (
            <div
              key={session.id}
              className={`pisci-inbox-session ${session.id === activeSessionId ? "active" : ""}`}
              onClick={() => setActiveSessionId(session.id)}
              style={{ cursor: "pointer" }}
            >
              <div className="pisci-inbox-session-top">
                <span className="pisci-inbox-session-name">{session.title || session.id}</span>
                <span style={{ display: "flex", alignItems: "center", gap: 4 }}>
                  <span className="pisci-inbox-session-kind">{sessionKindLabel(t, mode, session)}</span>
                  <button
                    title={t("common.delete")}
                    disabled={deletingId === session.id}
                    onClick={(e) => requestDeleteSession(e, session)}
                    style={{ background: "none", border: "none", cursor: "pointer", color: "var(--text-muted)", fontSize: 12, padding: "0 2px", lineHeight: 1, opacity: 0.6 }}
                    onMouseEnter={(e) => (e.currentTarget.style.opacity = "1")}
                    onMouseLeave={(e) => (e.currentTarget.style.opacity = "0.6")}
                  >✕</button>
                </span>
              </div>
              <div className="pisci-inbox-session-meta">
                <span>{formatTime(session.updated_at)}</span>
                <span>{t("pond.inboxMessageCount", { count: session.message_count })}</span>
              </div>
            </div>
          ))}
        </div>
      </div>

      <div className="pisci-inbox-main">
        {!activeSession && (
          <div className="pisci-inbox-main-empty">
            <div className="pisci-inbox-main-empty-icon">📬</div>
            <div>{copy.selectHint}</div>
          </div>
        )}

        {activeSession && (
          <>
            <div className="pisci-inbox-main-header">
              <div>
                <div className="pisci-inbox-main-title">{activeSession.title || activeSession.id}</div>
                <div className="pisci-inbox-main-meta">
                  {sessionKindLabel(t, mode, activeSession)} · {copy.readonly}
                </div>
              </div>
              <button
                className="pisci-inbox-refresh"
                onClick={() => loadMessages(activeSession.id).catch(console.error)}
              >
                {t("pond.inboxRefresh")}
              </button>
            </div>

            <div
              className="pisci-inbox-messages"
              ref={messagesContainerRef}
              onScroll={handleScroll}
            >
              {loadingMessages && messages.length === 0 && (
                <div className="pisci-inbox-empty">{t("common.loading")}</div>
              )}
              {!loadingMessages && messages.length === 0 && (
                <div className="pisci-inbox-empty">{copy.noMessages}</div>
              )}
              {messages.length > 0 && (
                <div className="pisci-inbox-load-more">
                  {loadingMore
                    ? t("common.loading")
                    : hasMore
                      ? t("common.loadMore")
                      : null}
                </div>
              )}
              {messages.filter((m) => m.content.trim()).map((message) => (
                <div key={message.id} className={`pisci-inbox-message pisci-inbox-message--${message.role}`}>
                  <div className="pisci-inbox-message-header">
                    <span className="pisci-inbox-message-role">
                      {inboxMessageRoleLabel(t, mode, message.role)}
                    </span>
                    <span className="pisci-inbox-message-time">{formatTime(message.created_at)}</span>
                  </div>
                  <div className="pisci-inbox-message-content"><InboxMessageContent content={message.content} /></div>
                </div>
              ))}
              <div ref={messagesEndRef} />
            </div>
          </>
        )}
      </div>

      <ConfirmDialog
        open={!!confirmTarget}
        title={confirmTarget?.blocked ? t("pond.inboxDeleteActiveTitle") : copy.deleteTitle}
        message={
          confirmTarget?.blocked
            ? t("pond.inboxDeleteActiveMessage", { name: confirmTarget.title })
            : copy.deleteMessage
        }
        confirmLabel={t("common.delete")}
        variant="danger"
        loading={deletingId !== null}
        onConfirm={confirmDeleteSession}
        onCancel={() => setConfirmTarget(null)}
      />
    </div>
  );
}
