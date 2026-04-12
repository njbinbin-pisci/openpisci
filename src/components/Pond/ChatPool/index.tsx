import { useState, useEffect, useLayoutEffect, useRef, useCallback, useMemo, UIEvent } from "react";
import { useTranslation } from "react-i18next";
import { useSelector, useDispatch } from "react-redux";
import { listen } from "@tauri-apps/api/event";
import { openPath } from "../../../services/tauri";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { poolApi, koiApi, PoolMessage, KoiWithStats, PoolSession } from "../../../services/tauri";
import { RootState, poolActions, koiActions, POOL_DEFAULT_CAPACITY } from "../../../store";
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
                  openPath(uriToNativePath(href!)).catch(console.error);
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

/** Initial load: fetch the latest N messages */
const INITIAL_LOAD_SIZE = 100;
/** How many older messages to load per lazy-load trigger */
const LAZY_LOAD_STEP = 10;

export default function ChatPool() {
  const { t } = useTranslation();
  const dispatch = useDispatch();

  const sessions = useSelector((s: RootState) => s.pool.sessions);
  const activeSessionId = useSelector((s: RootState) => s.pool.activeSessionId);
  const messagesBySession = useSelector((s: RootState) => s.pool.messagesBySession);
  const hasMoreBySession = useSelector((s: RootState) => s.pool.hasMoreBySession);
  const loading = useSelector((s: RootState) => s.pool.loading);
  const kois = useSelector((s: RootState) => s.koi.kois);

  const messages = activeSessionId ? messagesBySession[activeSessionId] ?? [] : [];
  const hasMore = activeSessionId ? hasMoreBySession[activeSessionId] ?? false : false;
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const messagesContainerRef = useRef<HTMLDivElement>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const [unreadCount, setUnreadCount] = useState(0);
  /** Current FIFO capacity for this session. Starts at POOL_DEFAULT_CAPACITY, grows by LAZY_LOAD_STEP on each lazy-load. */
  const [capacity, setCapacity] = useState(POOL_DEFAULT_CAPACITY);
  // Track whether the current session's initial load is done so we can scroll to bottom
  const initialLoadDoneRef = useRef<string | null>(null);

  const [showNewDialog, setShowNewDialog] = useState(false);
  const [newName, setNewName] = useState("");
  const [newTaskTimeoutSecs, setNewTaskTimeoutSecs] = useState(0);
  const [creating, setCreating] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<{ id: string; name: string } | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [orgSpecOpen, setOrgSpecOpen] = useState(false);
  const [orgSpecDraft, setOrgSpecDraft] = useState("");
  const [sessionTaskTimeoutSecs, setSessionTaskTimeoutSecs] = useState(0);
  const [orgSpecSaving, setOrgSpecSaving] = useState(false);
  // Session action menu (⋯)
  const [menuOpenId, setMenuOpenId] = useState<string | null>(null);
  const [actionTarget, setActionTarget] = useState<{ id: string; name: string; action: "pause" | "resume" | "archive" } | null>(null);
  const [actioning, setActioning] = useState(false);

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

  /** Load the latest INITIAL_LOAD_SIZE messages for a session (initial load) */
  const loadMessages = useCallback(async (sessionId: string) => {
    try {
      const msgs = await poolApi.getMessages({ session_id: sessionId, limit: INITIAL_LOAD_SIZE });
      dispatch(poolActions.setPoolMessages({
        sessionId,
        messages: msgs,
        hasMore: msgs.length === INITIAL_LOAD_SIZE,
      }));
      // Seed the last-id tracker so the first real-time message after load
      // is correctly detected as an append (not a false positive).
      if (msgs.length > 0) {
        prevLastIdRef.current = msgs[msgs.length - 1].id;
      }
      initialLoadDoneRef.current = sessionId;
    } catch {
      // silently ignore
    }
  }, [dispatch]);

  /** Load LAZY_LOAD_STEP older messages when user scrolls to the top, expanding capacity. */
  const loadOlderMessages = useCallback(async (sessionId: string, currentCount: number) => {
    if (loadingMore) return;
    setLoadingMore(true);
    try {
      const msgs = await poolApi.getMessages({
        session_id: sessionId,
        limit: LAZY_LOAD_STEP,
        offset: currentCount,
      });
      if (msgs.length > 0) {
        dispatch(poolActions.prependPoolMessages({
          sessionId,
          messages: msgs,
          hasMore: msgs.length === LAZY_LOAD_STEP,
        }));
        // Expand capacity so the new older messages are not immediately evicted on next append
        setCapacity((c) => c + LAZY_LOAD_STEP);
      } else {
        dispatch(poolActions.prependPoolMessages({ sessionId, messages: [], hasMore: false }));
      }
    } catch {
      // silently ignore
    } finally {
      setLoadingMore(false);
    }
  }, [dispatch, loadingMore]);

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
    setUnreadCount(0);
    setCapacity(POOL_DEFAULT_CAPACITY);
    prevLastIdRef.current = null;
    loadMessages(activeSessionId);

    let unlisten: (() => void) | null = null;
    poolApi.onMessage(activeSessionId, (msg) => {
      dispatch(poolActions.appendPoolMessage(msg));
    }).then((fn) => { unlisten = fn; });

    return () => { unlisten?.(); };
  }, [activeSessionId, loadMessages, dispatch]);

  // After initial load for a session: immediately jump to bottom (no animation)
  useLayoutEffect(() => {
    if (initialLoadDoneRef.current === activeSessionId && messagesContainerRef.current) {
      messagesContainerRef.current.scrollTop = messagesContainerRef.current.scrollHeight;
      initialLoadDoneRef.current = null;
    }
  });

  // When a new message arrives (real-time append): trim FIFO, auto-scroll if near bottom,
  // otherwise increment unread counter to show the "new messages" badge.
  // We distinguish real-time appends from prepend (load-older) by tracking the
  // last known bottom message id — if the tail changed, it's a real new message.
  const prevLastIdRef = useRef<number | null>(null);
  useEffect(() => {
    const el = messagesContainerRef.current;
    if (!el || messages.length === 0) return;
    const lastId = messages[messages.length - 1].id;
    const isAppend = lastId !== prevLastIdRef.current && prevLastIdRef.current !== null;
    prevLastIdRef.current = lastId;
    if (!isAppend) return;

    // FIFO trim: evict oldest messages beyond current capacity
    if (activeSessionId && messages.length > capacity) {
      dispatch(poolActions.trimPoolMessages({ sessionId: activeSessionId, capacity }));
    }

    // Auto-scroll only if user is within the bottom 10% of the scroll area
    const scrollable = el.scrollHeight - el.clientHeight;
    const nearBottom = scrollable <= 0 || el.scrollTop >= scrollable * 0.9;
    if (nearBottom) {
      messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
      setUnreadCount(0);
    } else {
      setUnreadCount((n) => n + 1);
    }
  }, [messages, capacity, activeSessionId, dispatch]);

  const scrollToBottom = useCallback(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
    setUnreadCount(0);
  }, []);

  const handleMessagesScroll = useCallback((e: UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    // Clear unread badge when user manually scrolls to bottom 10%
    const scrollable = el.scrollHeight - el.clientHeight;
    const nearBottom = scrollable <= 0 || el.scrollTop >= scrollable * 0.9;
    if (nearBottom) {
      setUnreadCount(0);
    }
    if (el.scrollTop < 60 && hasMore && activeSessionId && !loadingMore) {
      const prevScrollHeight = el.scrollHeight;
      loadOlderMessages(activeSessionId, messages.length).then(() => {
        // Restore scroll position so the view doesn't jump to the top
        requestAnimationFrame(() => {
          el.scrollTop = el.scrollHeight - prevScrollHeight;
        });
      });
    }
  }, [hasMore, activeSessionId, loadingMore, loadOlderMessages, messages.length]);

  const handleCreateSession = async () => {
    const name = newName.trim();
    if (!name) return;
    try {
      setCreating(true);
      const session = await poolApi.createSession(name, newTaskTimeoutSecs);
      dispatch(poolActions.addPoolSession(session));
      dispatch(poolActions.setActivePoolSession(session.id));
      setNewName("");
      setNewTaskTimeoutSecs(0);
      setShowNewDialog(false);
    } catch (e) {
    } finally {
      setCreating(false);
    }
  };

  const activeSession = useMemo(() => sessions.find((s) => s.id === activeSessionId), [sessions, activeSessionId]);

  useEffect(() => {
    if (activeSession) {
      setOrgSpecDraft(activeSession.org_spec || "");
      setSessionTaskTimeoutSecs(activeSession.task_timeout_secs ?? 0);
    }
  }, [activeSession]);

  const handleSaveOrgSpec = async () => {
    if (!activeSessionId) return;
    setOrgSpecSaving(true);
    try {
      await poolApi.updateOrgSpec(activeSessionId, orgSpecDraft);
      await poolApi.updateConfig(activeSessionId, sessionTaskTimeoutSecs);
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

  const confirmSessionAction = async () => {
    if (!actionTarget) return;
    setActioning(true);
    try {
      if (actionTarget.action === "pause") {
        await poolApi.pauseSession(actionTarget.id);
        dispatch(poolActions.updatePoolSessionStatus({ id: actionTarget.id, status: "paused" }));
      } else if (actionTarget.action === "resume") {
        await poolApi.resumeSession(actionTarget.id);
        dispatch(poolActions.updatePoolSessionStatus({ id: actionTarget.id, status: "active" }));
      } else if (actionTarget.action === "archive") {
        await poolApi.archiveSession(actionTarget.id);
        dispatch(poolActions.updatePoolSessionStatus({ id: actionTarget.id, status: "archived" }));
      }
    } catch (e) {
      console.error("[ChatPool] session action error:", e);
    } finally {
      setActioning(false);
      setActionTarget(null);
    }
  };

  // Close menu when clicking outside
  useEffect(() => {
    if (!menuOpenId) return;
    const handler = () => setMenuOpenId(null);
    document.addEventListener("click", handler);
    return () => document.removeEventListener("click", handler);
  }, [menuOpenId]);

  // Listen for pool_session_updated events from backend (pause/resume/archive)
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<{ id: string; status: string }>("pool_session_updated", (e) => {
      dispatch(poolActions.updatePoolSessionStatus({ id: e.payload.id, status: e.payload.status }));
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [dispatch]);

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
            <input
              className="chatpool-input"
              type="number"
              min={0}
              max={7200}
              value={newTaskTimeoutSecs}
              onChange={(e) => {
                const v = Number(e.target.value);
                setNewTaskTimeoutSecs(Number.isFinite(v) ? Math.max(0, Math.min(7200, v)) : 0);
              }}
              placeholder={t("pool.taskTimeoutPlaceholder")}
            />
            <div className="chatpool-empty-hint">{t("pool.taskTimeoutHelp")}</div>
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
            const isMenuOpen = menuOpenId === s.id;
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
                <div className="chatpool-session-menu-wrap" onClick={(e) => e.stopPropagation()}>
                  <button
                    className="chatpool-session-menu-btn"
                    title={t("pool.sessionActions")}
                    onClick={(e) => { e.stopPropagation(); setMenuOpenId(isMenuOpen ? null : s.id); }}
                  >
                    ⋯
                  </button>
                  {isMenuOpen && (
                    <div className="chatpool-session-menu">
                      {s.status === "active" && (
                        <button className="chatpool-menu-item chatpool-menu-item--warn"
                          onClick={() => { setMenuOpenId(null); setActionTarget({ id: s.id, name: s.name, action: "pause" }); }}>
                          ⏸ {t("pool.pauseSession")}
                        </button>
                      )}
                      {(s.status === "paused" || s.status === "archived") && (
                        <button className="chatpool-menu-item chatpool-menu-item--ok"
                          onClick={() => { setMenuOpenId(null); setActionTarget({ id: s.id, name: s.name, action: "resume" }); }}>
                          ▶ {t("pool.resumeSession")}
                        </button>
                      )}
                      {s.status !== "archived" && (
                        <button className="chatpool-menu-item"
                          onClick={() => { setMenuOpenId(null); setActionTarget({ id: s.id, name: s.name, action: "archive" }); }}>
                          🗄 {t("pool.archiveSession")}
                        </button>
                      )}
                      <div className="chatpool-menu-divider" />
                      <button className="chatpool-menu-item chatpool-menu-item--danger"
                        onClick={() => { setMenuOpenId(null); setDeleteTarget({ id: s.id, name: s.name }); }}>
                        🗑 {t("pool.deleteSession")}
                      </button>
                    </div>
                  )}
                </div>
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
                <label className="koi-form-label">{t("pool.taskTimeoutField")}</label>
                <input
                  className="chatpool-input"
                  type="number"
                  min={0}
                  max={7200}
                  value={sessionTaskTimeoutSecs}
                  onChange={(e) => {
                    const v = Number(e.target.value);
                    setSessionTaskTimeoutSecs(Number.isFinite(v) ? Math.max(0, Math.min(7200, v)) : 0);
                  }}
                />
                <div className="chatpool-empty-hint">{t("pool.taskTimeoutHelp")}</div>
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
                  disabled={
                    orgSpecSaving
                    || (
                      orgSpecDraft === (activeSession?.org_spec || "")
                      && sessionTaskTimeoutSecs === (activeSession?.task_timeout_secs ?? 0)
                    )
                  }
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
              <div className="chatpool-load-more">
                {loadingMore ? t("common.loading") : t("common.loadMore")}
              </div>
            )}
            {messages.map((msg) => (
              <MessageBubble key={msg.id} msg={msg} kois={kois} />
            ))}
            <div ref={messagesEndRef} />
          </div>
        )}
        {unreadCount > 0 && (
          <button className="chatpool-unread-badge" onClick={scrollToBottom}>
            ↓ {unreadCount} 条新消息
          </button>
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
      <ConfirmDialog
        open={!!actionTarget}
        title={
          actionTarget?.action === "pause" ? t("pool.confirmPauseTitle") :
          actionTarget?.action === "resume" ? t("pool.confirmResumeTitle") :
          t("pool.confirmArchiveTitle")
        }
        message={
          actionTarget?.action === "pause"
            ? t("pool.confirmPauseMessage", { name: actionTarget?.name ?? "" })
            : actionTarget?.action === "resume"
            ? t("pool.confirmResumeMessage", { name: actionTarget?.name ?? "" })
            : t("pool.confirmArchiveMessage", { name: actionTarget?.name ?? "" })
        }
        confirmLabel={
          actionTarget?.action === "pause" ? t("pool.pauseSession") :
          actionTarget?.action === "resume" ? t("pool.resumeSession") :
          t("pool.archiveSession")
        }
        cancelLabel={t("common.cancel")}
        variant={actionTarget?.action === "archive" ? "danger" : "primary"}
        loading={actioning}
        onConfirm={confirmSessionAction}
        onCancel={() => !actioning && setActionTarget(null)}
      />
    </div>
  );
}
