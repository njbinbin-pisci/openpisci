import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { ChatMessage, Session, sessionsApi } from "../../../services/tauri";
import "./PisciInbox.css";

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
            <button
              key={session.id}
              className={`pisci-inbox-session ${session.id === activeSessionId ? "active" : ""}`}
              onClick={() => setActiveSessionId(session.id)}
            >
              <div className="pisci-inbox-session-top">
                <span className="pisci-inbox-session-name">{session.title || session.id}</span>
                <span className="pisci-inbox-session-kind">
                  {sessionKindLabel(t, session)}
                </span>
              </div>
              <div className="pisci-inbox-session-meta">
                <span>{formatTime(session.updated_at)}</span>
                <span>{t("pond.inboxMessageCount", { count: session.message_count })}</span>
              </div>
            </button>
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
                  <pre className="pisci-inbox-message-content">{message.content}</pre>
                </div>
              ))}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
