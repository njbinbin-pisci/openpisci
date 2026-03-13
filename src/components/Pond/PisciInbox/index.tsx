import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { ChatMessage, Session, sessionsApi, poolApi } from "../../../services/tauri";
import { linkifyPaths, isLocalPath, uriToNativePath } from "../../../utils/linkify";
import ConfirmDialog from "../../ConfirmDialog";
import "./PisciInbox.css";

function InboxMessageContent({ content }: { content: string }) {
  const processed = linkifyPaths(content);
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      urlTransform={(url) => url.startsWith("file://") ? url : (url.startsWith("http://") || url.startsWith("https://") || url.startsWith("mailto:") || url.startsWith("#") || url.startsWith("/") || !url.includes(":")) ? url : ""}
      components={{
        a: ({ href, children }) => {
          if (isLocalPath(href)) {
            return (
              <a href="#" title={href} style={{ cursor: "pointer" }}
                onClick={(e) => { e.preventDefault(); shellOpen(uriToNativePath(href!)).catch(console.error); }}>
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

function isInternalSession(session: Session): boolean {
  return session.source === "heartbeat"
    || session.source === "heartbeat_pool"
    || session.source === "pisci_inbox_global"
    || session.source === "pisci_inbox_pool"
    || session.source === "pisci_internal"
    || session.id === "heartbeat"
    || session.id === "pisci_inbox_global"
    || session.id.startsWith("pisci_pool_");
}

function formatTime(value: string): string {
  try {
    return new Date(value).toLocaleString();
  } catch {
    return value;
  }
}

function sessionKindLabel(t: (key: string) => string, session: Session): string {
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

export default function PisciInbox() {
  const { t } = useTranslation();
  const [sessions, setSessions] = useState<Session[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [loadingSessions, setLoadingSessions] = useState(false);
  const [loadingMessages, setLoadingMessages] = useState(false);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [confirmTarget, setConfirmTarget] = useState<{ id: string; title: string; blocked: boolean } | null>(null);

  const internalSessions = useMemo(
    () => sessions.filter(isInternalSession),
    [sessions],
  );

  const loadSessions = useCallback(async () => {
    setLoadingSessions(true);
    try {
      const result = await sessionsApi.list(200, 0);
      const internal = result.sessions.filter(isInternalSession);
      setSessions(result.sessions);
      setActiveSessionId((prev) => {
        if (prev && internal.some((session) => session.id === prev)) return prev;
        return internal[0]?.id ?? null;
      });
    } finally {
      setLoadingSessions(false);
    }
  }, []);

  const loadMessages = useCallback(async (sessionId: string) => {
    setLoadingMessages(true);
    try {
      const result = await sessionsApi.getMessages(sessionId, 200, 0);
      setMessages(result);
    } finally {
      setLoadingMessages(false);
    }
  }, []);

  useEffect(() => {
    loadSessions().catch(console.error);
  }, [loadSessions]);

  useEffect(() => {
    if (!activeSessionId) {
      setMessages([]);
      return;
    }
    loadMessages(activeSessionId).catch(console.error);
  }, [activeSessionId, loadMessages]);

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
        const remaining = sessions.filter((s) => s.id !== confirmTarget.id && isInternalSession(s));
        setActiveSessionId(remaining.length > 0 ? remaining[0].id : null);
        setMessages([]);
      }
      setConfirmTarget(null);
    } catch (err) {
      console.error("Failed to delete session:", err);
    } finally {
      setDeletingId(null);
    }
  }, [confirmTarget, activeSessionId, sessions]);

  const activeSession = internalSessions.find((session) => session.id === activeSessionId) ?? null;

  return (
    <div className="pisci-inbox">
      <div className="pisci-inbox-sidebar">
        <div className="pisci-inbox-sidebar-header">
          <div>
            <div className="pisci-inbox-title">{t("pond.inboxTitle")}</div>
            <div className="pisci-inbox-subtitle">{t("pond.inboxDesc")}</div>
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
            <div className="pisci-inbox-empty">{t("pond.inboxEmpty")}</div>
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
                  <span className="pisci-inbox-session-kind">{sessionKindLabel(t, session)}</span>
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
            <div>{t("pond.inboxSelectHint")}</div>
          </div>
        )}

        {activeSession && (
          <>
            <div className="pisci-inbox-main-header">
              <div>
                <div className="pisci-inbox-main-title">{activeSession.title || activeSession.id}</div>
                <div className="pisci-inbox-main-meta">
                  {sessionKindLabel(t, activeSession)} · {t("pond.inboxReadonly")}
                </div>
              </div>
              <button
                className="pisci-inbox-refresh"
                onClick={() => loadMessages(activeSession.id).catch(console.error)}
              >
                {t("pond.inboxRefresh")}
              </button>
            </div>

            <div className="pisci-inbox-messages">
              {loadingMessages && messages.length === 0 && (
                <div className="pisci-inbox-empty">{t("common.loading")}</div>
              )}
              {!loadingMessages && messages.length === 0 && (
                <div className="pisci-inbox-empty">{t("pond.inboxNoMessages")}</div>
              )}
              {messages.map((message) => (
                <div key={message.id} className={`pisci-inbox-message pisci-inbox-message--${message.role}`}>
                  <div className="pisci-inbox-message-header">
                    <span className="pisci-inbox-message-role">
                      {message.role === "assistant" ? "Pisci" : message.role}
                    </span>
                    <span className="pisci-inbox-message-time">{formatTime(message.created_at)}</span>
                  </div>
                  <div className="pisci-inbox-message-content"><InboxMessageContent content={message.content} /></div>
                </div>
              ))}
            </div>
          </>
        )}
      </div>

      <ConfirmDialog
        open={!!confirmTarget}
        title={confirmTarget?.blocked ? t("pond.inboxDeleteActiveTitle") : t("pond.inboxDeleteTitle")}
        message={
          confirmTarget?.blocked
            ? t("pond.inboxDeleteActiveMessage", { name: confirmTarget.title })
            : t("pond.inboxDeleteMessage", { name: confirmTarget?.title ?? "" })
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
