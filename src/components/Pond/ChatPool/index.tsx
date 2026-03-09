import { useState, useEffect, useRef, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { useSelector, useDispatch } from "react-redux";
import { poolApi, koiApi, PoolMessage, KoiWithStats } from "../../../services/tauri";
import { RootState, poolActions, koiActions } from "../../../store";
import "./ChatPool.css";

const STATUS_COLORS: Record<string, string> = {
  idle: "#6b7280",
  busy: "#22c55e",
  offline: "#6b7280",
};

function formatTime(iso: string): string {
  const d = new Date(iso);
  const now = new Date();
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  if (sameDay) return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  return d.toLocaleDateString([], { month: "short", day: "numeric" }) +
    " " + d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function parseMeta(metadata: string): Record<string, unknown> {
  try { return JSON.parse(metadata || "{}"); }
  catch { return {}; }
}

function MessageBubble({
  msg,
  kois,
}: {
  msg: PoolMessage;
  kois: KoiWithStats[];
}) {
  const sender = kois.find((k) => k.id === msg.sender_id);
  const isPisci = msg.sender_id === "pisci";
  const icon = isPisci ? "🐋" : sender?.icon ?? "🐟";
  const color = isPisci ? "#7c3aed" : sender?.color ?? "#6b7280";
  const name = isPisci ? "Pisci" : sender?.name ?? msg.sender_id;
  const meta = parseMeta(msg.metadata);

  return (
    <div className={`pool-msg pool-msg--${msg.msg_type}`}>
      <div className="pool-msg-bar" style={{ background: color }} />
      <div className="pool-msg-body">
        <div className="pool-msg-header">
          <span className="pool-msg-icon">{icon}</span>
          <span className="pool-msg-name" style={{ color }}>{name}</span>
          <span className="pool-msg-time">{formatTime(msg.created_at)}</span>
        </div>

        {msg.msg_type === "task_assign" ? (
          <div className="pool-msg-task-card">
            <div className="pool-msg-task-title">{(meta.title as string) || msg.content}</div>
            {typeof meta.priority === "string" && (
              <span className={`pool-msg-priority pool-msg-priority--${meta.priority}`}>
                {meta.priority}
              </span>
            )}
            {!meta.title && <div className="pool-msg-text">{msg.content}</div>}
          </div>
        ) : msg.msg_type === "status_update" ? (
          <div className="pool-msg-status-line">{msg.content}</div>
        ) : msg.msg_type === "result" ? (
          <div className="pool-msg-result-card">{msg.content}</div>
        ) : (
          <div className="pool-msg-text">{msg.content}</div>
        )}
      </div>
    </div>
  );
}

export default function ChatPool() {
  const { t } = useTranslation();
  const dispatch = useDispatch();

  const sessions = useSelector((s: RootState) => s.pool.sessions);
  const activeSessionId = useSelector((s: RootState) => s.pool.activeSessionId);
  const messagesBySession = useSelector((s: RootState) => s.pool.messagesBySession);
  const loading = useSelector((s: RootState) => s.pool.loading);
  const kois = useSelector((s: RootState) => s.koi.kois);

  const messages = activeSessionId ? messagesBySession[activeSessionId] ?? [] : [];
  const messagesEndRef = useRef<HTMLDivElement>(null);

  const [showNewDialog, setShowNewDialog] = useState(false);
  const [newName, setNewName] = useState("");
  const [creating, setCreating] = useState(false);

  const loadSessions = useCallback(async () => {
    try {
      dispatch(poolActions.setLoading(true));
      const list = await poolApi.listSessions();
      dispatch(poolActions.setPoolSessions(list));
      if (!activeSessionId && list.length > 0) {
        dispatch(poolActions.setActivePoolSession(list[0].id));
      }
    } catch {
      // silently ignore
    } finally {
      dispatch(poolActions.setLoading(false));
    }
  }, [dispatch, activeSessionId]);

  const loadMessages = useCallback(async (sessionId: string) => {
    try {
      const msgs = await poolApi.getMessages({ session_id: sessionId, limit: 200 });
      dispatch(poolActions.setPoolMessages({ sessionId, messages: msgs }));
    } catch {
      // silently ignore
    }
  }, [dispatch]);

  useEffect(() => {
    loadSessions();
    if (kois.length === 0) {
      koiApi.list().then((list) => dispatch(koiActions.setKois(list))).catch(() => {});
    }
  }, [loadSessions, dispatch, kois.length]);

  useEffect(() => {
    if (!activeSessionId) return;
    loadMessages(activeSessionId);

    let unlisten: (() => void) | null = null;
    poolApi.onMessage(activeSessionId, (msg) => {
      dispatch(poolActions.appendPoolMessage(msg));
    }).then((fn) => { unlisten = fn; });

    return () => { unlisten?.(); };
  }, [activeSessionId, loadMessages, dispatch]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages.length]);

  const handleCreateSession = async () => {
    const name = newName.trim();
    if (!name) return;
    try {
      setCreating(true);
      const session = await poolApi.createSession(name);
      dispatch(poolActions.addPoolSession(session));
      dispatch(poolActions.setActivePoolSession(session.id));
      setNewName("");
      setShowNewDialog(false);
    } catch {
      // silently ignore
    } finally {
      setCreating(false);
    }
  };

  const handleDeleteSession = async (id: string) => {
    try {
      await poolApi.deleteSession(id);
      dispatch(poolActions.removePoolSession(id));
    } catch {
      // silently ignore
    }
  };

  return (
    <div className="chatpool">
      <div className="chatpool-sidebar">
        <button
          className="chatpool-new-btn"
          onClick={() => setShowNewDialog(true)}
        >
          + {t("pool.newSession")}
        </button>

        {showNewDialog && (
          <div className="chatpool-new-dialog">
            <input
              className="chatpool-input"
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              placeholder={t("pool.sessionPlaceholder")}
              autoFocus
              onKeyDown={(e) => e.key === "Enter" && handleCreateSession()}
            />
            <div className="chatpool-new-actions">
              <button
                className="chatpool-btn chatpool-btn-secondary"
                onClick={() => { setShowNewDialog(false); setNewName(""); }}
              >
                {t("koi.cancel")}
              </button>
              <button
                className="chatpool-btn chatpool-btn-primary"
                onClick={handleCreateSession}
                disabled={creating || !newName.trim()}
              >
                {t("koi.create")}
              </button>
            </div>
          </div>
        )}

        <div className="chatpool-session-list">
          {loading && sessions.length === 0 && (
            <div className="chatpool-empty-hint">{t("common.loading")}</div>
          )}
          {!loading && sessions.length === 0 && (
            <div className="chatpool-empty-hint">{t("pool.noSessions")}</div>
          )}
          {sessions.map((s) => (
            <div
              key={s.id}
              className={`chatpool-session-item ${s.id === activeSessionId ? "active" : ""}`}
              onClick={() => dispatch(poolActions.setActivePoolSession(s.id))}
            >
              <div className="chatpool-session-name">{s.name}</div>
              <div className="chatpool-session-time">{formatTime(s.updated_at)}</div>
              <button
                className="chatpool-session-delete"
                onClick={(e) => { e.stopPropagation(); handleDeleteSession(s.id); }}
                title={t("pool.deleteSession")}
              >
                ✕
              </button>
            </div>
          ))}
        </div>

        <div className="chatpool-participants">
          <div className="chatpool-participants-title">{t("pool.participants")}</div>
          <div className="chatpool-participant">
            <span className="chatpool-participant-icon">🐋</span>
            <span className="chatpool-participant-name">Pisci</span>
            <span className="chatpool-participant-badge">{t("pool.mainAgent")}</span>
          </div>
          {kois.map((koi) => (
            <div key={koi.id} className="chatpool-participant">
              <span className="chatpool-participant-icon">{koi.icon}</span>
              <span className="chatpool-participant-name" style={{ color: koi.color }}>
                {koi.name}
              </span>
              <span
                className="chatpool-participant-dot"
                style={{ background: STATUS_COLORS[koi.status] || "#6b7280" }}
              />
              {koi.active_todo_count > 0 && (
                <span className="chatpool-participant-todos">{koi.active_todo_count}</span>
              )}
            </div>
          ))}
        </div>
      </div>

      <div className="chatpool-main">
        {!activeSessionId ? (
          <div className="chatpool-empty">
            <span className="chatpool-empty-icon">💬</span>
            <p>{t("pool.noSessions")}</p>
          </div>
        ) : messages.length === 0 ? (
          <div className="chatpool-empty">
            <span className="chatpool-empty-icon">💬</span>
            <p>{t("pool.noMessages")}</p>
          </div>
        ) : (
          <div className="chatpool-messages">
            {messages.map((msg) => (
              <MessageBubble key={msg.id} msg={msg} kois={kois} />
            ))}
            <div ref={messagesEndRef} />
          </div>
        )}

        <div className="chatpool-readonly-bar">
          🔒 {t("pool.readonlyHint")}
        </div>
      </div>
    </div>
  );
}
