import { useState, useEffect, useRef, useCallback, useMemo, UIEvent } from "react";
import { useTranslation } from "react-i18next";
import { useSelector, useDispatch } from "react-redux";
import { listen } from "@tauri-apps/api/event";
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { poolApi, koiApi, PoolMessage, KoiWithStats } from "../../../services/tauri";
import { RootState, poolActions, koiActions } from "../../../store";
import ConfirmDialog from "../../ConfirmDialog";
import { linkifyPaths, isLocalPath, uriToNativePath } from "../../../utils/linkify";
import "./ChatPool.css";

/** Render pool message content with Markdown + clickable local file paths */
function PoolMessageContent({ content }: { content: string }) {
  const processed = linkifyPaths(content);
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      components={{
        a: ({ href, children }) => {
          if (isLocalPath(href)) {
            return (
              <a
                href="#"
                title={href}
                style={{ cursor: "pointer", color: "var(--accent)" }}
                onClick={(e) => {
                  e.preventDefault();
                  shellOpen(uriToNativePath(href!)).catch(console.error);
                }}
              >
                {children}
              </a>
            );
          }
          return <a href={href} target="_blank" rel="noopener noreferrer">{children}</a>;
        },
      }}
    >
      {processed}
    </ReactMarkdown>
  );
}

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
            {msg.todo_id && <span className="pool-msg-todo-link">📋 {msg.todo_id.slice(0, 8)}</span>}
            {!meta.title && <div className="pool-msg-text">{msg.content}</div>}
          </div>
        ) : msg.msg_type === "task_claimed" ? (
          <div className="pool-msg-event-line pool-msg-event--claimed">
            ✋ {msg.content}
          </div>
        ) : msg.msg_type === "task_blocked" ? (
          <div className="pool-msg-event-line pool-msg-event--blocked">
            🚫 {msg.content}
          </div>
        ) : msg.msg_type === "task_done" ? (
          <div className="pool-msg-event-line pool-msg-event--done">
            ✅ {msg.content}
          </div>
        ) : msg.msg_type === "status_update" ? (
          <div className="pool-msg-status-line"><PoolMessageContent content={msg.content} /></div>
        ) : msg.msg_type === "result" ? (
          <div className="pool-msg-result-card"><PoolMessageContent content={msg.content} /></div>
        ) : msg.msg_type === "mention" ? (
          <div className="pool-msg-mention"><PoolMessageContent content={msg.content} /></div>
        ) : (
          <div className="pool-msg-text"><PoolMessageContent content={msg.content} /></div>
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

  const allMessages = activeSessionId ? messagesBySession[activeSessionId] ?? [] : [];
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const messagesContainerRef = useRef<HTMLDivElement>(null);
  const PAGE_SIZE = 100;
  const [visibleCount, setVisibleCount] = useState(PAGE_SIZE);

  const [showNewDialog, setShowNewDialog] = useState(false);
  const [newName, setNewName] = useState("");
  const [creating, setCreating] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<{ id: string; name: string } | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [orgSpecOpen, setOrgSpecOpen] = useState(false);
  const [orgSpecDraft, setOrgSpecDraft] = useState("");
  const [orgSpecSaving, setOrgSpecSaving] = useState(false);

  const loadSessions = useCallback(async () => {
    try {
      dispatch(poolActions.setLoading(true));
      const list = await poolApi.listSessions();
      dispatch(poolActions.setPoolSessions(list));
      const stillValid = activeSessionId && list.some(s => s.id === activeSessionId);
      if (!stillValid && list.length > 0) {
        dispatch(poolActions.setActivePoolSession(list[0].id));
      }
    } catch (e) {
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

  // Listen for Koi status changes (busy/idle) to update participant dots in real-time
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<{ id: string; status: string }>("koi_status_changed", () => {
      koiApi.list().then((list) => dispatch(koiActions.setKois(list))).catch(() => {});
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [dispatch]);

  useEffect(() => {
    if (!activeSessionId) return;
    loadMessages(activeSessionId);

    let unlisten: (() => void) | null = null;
    poolApi.onMessage(activeSessionId, (msg) => {
      dispatch(poolActions.appendPoolMessage(msg));
    }).then((fn) => { unlisten = fn; });

    return () => { unlisten?.(); };
  }, [activeSessionId, loadMessages, dispatch]);

  const messages = useMemo(
    () => allMessages.slice(-visibleCount),
    [allMessages, visibleCount],
  );
  const hasMore = allMessages.length > visibleCount;

  useEffect(() => {
    setVisibleCount(PAGE_SIZE);
  }, [activeSessionId]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [allMessages.length]);

  const handleMessagesScroll = useCallback((e: UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    if (el.scrollTop < 40 && hasMore) {
      setVisibleCount((prev) => Math.min(prev + PAGE_SIZE, allMessages.length));
    }
  }, [hasMore, allMessages.length]);

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
    } catch (e) {
    } finally {
      setCreating(false);
    }
  };

  const activeSession = useMemo(() => sessions.find((s) => s.id === activeSessionId), [sessions, activeSessionId]);

  useEffect(() => {
    if (activeSession) setOrgSpecDraft(activeSession.org_spec || "");
  }, [activeSession]);

  const handleSaveOrgSpec = async () => {
    if (!activeSessionId) return;
    setOrgSpecSaving(true);
    try {
      await poolApi.updateOrgSpec(activeSessionId, orgSpecDraft);
      loadSessions();
    } catch (e) {
      console.error("[ChatPool] save org_spec error:", e);
    } finally {
      setOrgSpecSaving(false);
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

  const confirmDeleteSession = async () => {
    if (!deleteTarget) return;
    try {
      setDeleting(true);
      await handleDeleteSession(deleteTarget.id);
      setDeleteTarget(null);
    } finally {
      setDeleting(false);
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
          {sessions.map((s) => {
            const statusColor = s.status === "active" ? "#22c55e" : s.status === "paused" ? "#f59e0b" : "#6b7280";
            return (
              <div
                key={s.id}
                className={`chatpool-session-item ${s.id === activeSessionId ? "active" : ""}${s.status === "archived" ? " chatpool-session-archived" : ""}`}
                onClick={() => dispatch(poolActions.setActivePoolSession(s.id))}
              >
                <div className="chatpool-session-name">
                  <span className="chatpool-status-dot" style={{ background: statusColor }} />
                  {s.name}
                </div>
                <div className="chatpool-session-time">{formatTime(s.updated_at)}</div>
                <button
                  className="chatpool-session-delete"
                  onClick={(e) => { e.stopPropagation(); setDeleteTarget({ id: s.id, name: s.name }); }}
                  title={t("pool.deleteSession")}
                >
                  ✕
                </button>
              </div>
            );
          })}
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

        {activeSessionId && (
          <div className="chatpool-orgspec-panel">
            <div
              className="chatpool-orgspec-header"
              onClick={() => setOrgSpecOpen(!orgSpecOpen)}
            >
              <span>{t("pool.orgSpec") || "Project Spec"}</span>
              <span className="chatpool-orgspec-chevron">{orgSpecOpen ? "▲" : "▼"}</span>
            </div>
            {orgSpecOpen && (
              <div className="chatpool-orgspec-body">
                <textarea
                  className="chatpool-orgspec-editor"
                  value={orgSpecDraft}
                  onChange={(e) => setOrgSpecDraft(e.target.value)}
                  placeholder="# Project Goal\n\n# Koi Roles\n\n# Collaboration Rules\n\n# Success Metrics"
                  rows={10}
                />
                <button
                  className="chatpool-btn chatpool-btn-primary"
                  onClick={handleSaveOrgSpec}
                  disabled={orgSpecSaving || orgSpecDraft === (activeSession?.org_spec || "")}
                  style={{ alignSelf: "flex-end", marginTop: 6 }}
                >
                  {orgSpecSaving ? "Saving..." : (t("common.save") || "Save")}
                </button>
              </div>
            )}
          </div>
        )}
      </div>

      <div className="chatpool-main" style={{ flex: 1, display: "flex", flexDirection: "column", overflow: "hidden", minWidth: 0, minHeight: 0 }}>
        {!activeSessionId ? (
          <div className="chatpool-scroll" style={{ flex: 1, overflowY: "auto", minHeight: 0 }}>
            <div className="chatpool-empty">
              <span className="chatpool-empty-icon">💬</span>
              <p>{t("pool.noSessions")}</p>
            </div>
          </div>
        ) : messages.length === 0 ? (
          <div className="chatpool-scroll" style={{ flex: 1, overflowY: "auto", minHeight: 0 }}>
            <div className="chatpool-empty">
              <span className="chatpool-empty-icon">💬</span>
              <p>{t("pool.noMessages")}</p>
            </div>
          </div>
        ) : (
          <div
            className="chatpool-scroll"
            style={{ flex: 1, overflowY: "auto", minHeight: 0 }}
            ref={messagesContainerRef}
            onScroll={handleMessagesScroll}
          >
            {hasMore && (
              <div className="chatpool-load-more">{t("common.loadMore")}</div>
            )}
            {messages.map((msg) => (
              <MessageBubble key={msg.id} msg={msg} kois={kois} />
            ))}
            <div ref={messagesEndRef} />
          </div>
        )}
        <div className="chatpool-readonly-bar">
          {t("pool.readonlyHint")}
        </div>
      </div>
      <ConfirmDialog
        open={!!deleteTarget}
        title={t("pool.confirmDeleteTitle")}
        message={t("pool.confirmDeleteMessage", { name: deleteTarget?.name ?? "" })}
        confirmLabel={t("common.delete")}
        cancelLabel={t("common.cancel")}
        variant="danger"
        loading={deleting}
        onConfirm={confirmDeleteSession}
        onCancel={() => !deleting && setDeleteTarget(null)}
      />
    </div>
  );
}
