import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useSelector, useDispatch } from "react-redux";
import { listen } from "@tauri-apps/api/event";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import FileTree from "../IDE/FileTree";
import type { FileTreeContextMenu } from "../IDE/FileTree";
import EditorTabs from "../IDE/EditorTabs";
import CodeEditor from "../IDE/CodeEditor";
import TerminalPanel from "../IDE/Terminal";
import AssistantPanel from "../IDE/AssistantPanel";
import GitPanel from "../IDE/GitPanel";
import SearchPanel from "../IDE/SearchPanel";
import Board from "../Board";
import PiscisInbox from "../PiscisInbox";
import KoiManager from "../KoiManager";
import PoolMemberPicker from "../PoolMemberPicker";
import { ideApi, onFileChanged } from "../../../services/tauri/ide";
import { openPath } from "../../../services/tauri";
import { poolApi, koiApi, PoolMessage, KoiWithStats } from "../../../services/tauri";
import { RootState, poolActions, koiActions, boardActions, POOL_DEFAULT_CAPACITY, parseMentions, hasMentions } from "../../../store";
import { useScrollPrependedHistory } from "../../../hooks/useScrollPrependedHistory";
import { containsDelegatedPiscisMention } from "../../../utils/poolMention";
import ConfirmDialog from "../../ConfirmDialog";
import { linkifyPaths, isLocalPath, uriToNativePath } from "../../../utils/linkify";
import type { FileNode, OpenTab, GitFileStatus } from "../IDE/types";
import "../IDE/IDE.css";
import "../ChatPool/ChatPool.css";
import "./Collab.css";

type ContentView = "chat" | "explorer" | "search" | "git" | "board" | "inbox" | "koiObserver";

const VIEW_ICONS: Record<ContentView, string> = {
  chat: "💬", explorer: "📁", search: "🔍", git: "⑂", board: "📋", inbox: "📬", koiObserver: "🔎",
};

// ─── Message rendering (borrowed from ChatPool) ──────────────────────

function PoolMessageContent({ content }: { content: string }) {
  const processed = linkifyPaths(content);
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      urlTransform={(url) => url.startsWith("file://") ? url : (url.startsWith("http://") || url.startsWith("https://") || url.startsWith("mailto:") || url.startsWith("#") || url.startsWith("/") || !url.includes(":")) ? url : ""}
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

function formatTime(iso: string): string {
  // chrono DateTime<Utc> serializes to RFC 3339, usually with Z suffix.
  // Defensively append Z if timezone info is absent so new Date() treats
  // the value as UTC rather than local time.
  let dateStr = iso;
  if (!/[Zz]$/.test(dateStr) && !/[+-]\d{2}:\d{2}$/.test(dateStr)) {
    dateStr = iso + "Z";
  }
  const d = new Date(dateStr);
  if (isNaN(d.getTime())) return iso;
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

const STATUS_COLORS: Record<string, string> = {
  idle: "#6b7280",
  busy: "#22c55e",
  offline: "#6b7280",
};

function MessageBubble({ msg, kois }: { msg: PoolMessage; kois: KoiWithStats[] }) {
  const sender = kois.find((k) => k.id === msg.sender_id);
  const isPiscis = msg.sender_id === "piscis";
  const icon = isPiscis ? "🐋" : sender?.icon ?? "🐟";
  const color = isPiscis ? "#7c3aed" : sender?.color ?? "#6b7280";
  const name = isPiscis ? "Piscis" : sender?.name ?? msg.sender_id;
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
          <div className="pool-msg-event-line pool-msg-event--claimed">✋ {msg.content}</div>
        ) : msg.msg_type === "task_blocked" ? (
          <div className="pool-msg-event-line pool-msg-event--blocked">🚫 {msg.content}</div>
        ) : msg.msg_type === "task_done" ? (
          <div className="pool-msg-event-line pool-msg-event--done">✅ {msg.content}</div>
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

// ─── Main Collab component ──────────────────────────────────────────

const INITIAL_LOAD_SIZE = 100;
const LAZY_LOAD_STEP = 10;

export default function Collab() {
  const { t } = useTranslation();
  const dispatch = useDispatch();

  // Pool state
  const sessions = useSelector((s: RootState) => s.pool.sessions);
  const activeSessionId = useSelector((s: RootState) => s.pool.activeSessionId);
  const messagesBySession = useSelector((s: RootState) => s.pool.messagesBySession);
  const hasMoreBySession = useSelector((s: RootState) => s.pool.hasMoreBySession);
  const loading = useSelector((s: RootState) => s.pool.loading);
  const kois = useSelector((s: RootState) => s.koi.kois);

  const messages = activeSessionId ? messagesBySession[activeSessionId] ?? [] : [];
  const hasMore = activeSessionId ? hasMoreBySession[activeSessionId] ?? false : false;
  const activeSession = useMemo(() => sessions.find((s) => s.id === activeSessionId), [sessions, activeSessionId]);
  const projectDir = activeSession?.project_dir ?? null;
  // Only Koi explicitly added to the active project are participants.
  const poolMembers = useMemo(() => {
    const ids = new Set(activeSession?.member_koi_ids ?? []);
    return kois.filter((k) => ids.has(k.id));
  }, [kois, activeSession]);

  // Chat state
  const [loadingMore, setLoadingMore] = useState(false);
  const [unreadCount, setUnreadCount] = useState(0);
  const [capacity, setCapacity] = useState(POOL_DEFAULT_CAPACITY);
  // Track which session we've scrolled to bottom for — reset on switch.
  const scrolledSessionRef = useRef<string | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const messagesContainerRef = useRef<HTMLDivElement>(null);
  const prevLastIdRef = useRef<number | null>(null);

  // User input
  const [userInput, setUserInput] = useState("");
  const [mentionError, setMentionError] = useState("");
  const [sending, setSending] = useState(false);
  // @mention autocomplete
  const [mentionFilter, setMentionFilter] = useState<string | null>(null);
  const [mentionIndex, setMentionIndex] = useState(0);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  // Pool chat: @!Koi only — Piscis is reached via IDE CLI (main chat · Pool CLI).
  const mentionCandidates = useMemo(() => {
    const list: { name: string; icon: string; desc: string }[] = [];
    poolMembers.filter(k => k.status !== "offline").forEach(k => {
      list.push({ name: k.name, icon: k.icon || "🐡", desc: k.description || k.role });
    });
    return list;
  }, [poolMembers]);

  // Filter candidates when user types @
  const filteredMentions = useMemo(() => {
    if (mentionFilter === null) return [];
    if (!mentionFilter) return mentionCandidates;
    const lower = mentionFilter.toLowerCase();
    const exact = mentionCandidates.filter(c => c.name.toLowerCase() === lower);
    const partial = mentionCandidates.filter(c => c.name.toLowerCase().startsWith(lower) && c.name.toLowerCase() !== lower);
    return [...exact, ...partial];
  }, [mentionCandidates, mentionFilter]);

  // Detect @mention typing
  const handleInputChange = useCallback((e: React.ChangeEvent<HTMLTextAreaElement>) => {
    const val = e.target.value;
    setUserInput(val);
    setMentionError("");
    // Detect active @mention at cursor position
    const cursor = e.target.selectionStart;
    const before = val.slice(0, cursor);
    const match = before.match(/(?:^|\s)@(\S*)$/);
    if (match) {
      setMentionFilter(match[1]);
      setMentionIndex(0);
    } else {
      setMentionFilter(null);
    }
  }, []);

  // Insert selected mention
  const insertMention = useCallback((name: string) => {
    const cursor = inputRef.current?.selectionStart ?? userInput.length;
    const before = userInput.slice(0, cursor);
    const after = userInput.slice(cursor);
    const replaced = before.replace(/@\S*$/, `@!${name} `);
    setUserInput(replaced + after);
    setMentionFilter(null);
    // move cursor after inserted mention
    setTimeout(() => {
      const pos = replaced.length;
      inputRef.current?.setSelectionRange(pos, pos);
      inputRef.current?.focus();
    }, 0);
  }, [userInput]);

  // Session management dialogs
  const [showNewDialog, setShowNewDialog] = useState(false);
  const [newName, setNewName] = useState("");
  const [newProjectDir, setNewProjectDir] = useState("");
  const [newTaskTimeoutSecs, setNewTaskTimeoutSecs] = useState(0);
  const [creating, setCreating] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<{ id: string; name: string } | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [orgSpecOpen, setOrgSpecOpen] = useState(false);
  const [orgSpecDraft, setOrgSpecDraft] = useState("");
  const [sessionTaskTimeoutSecs, setSessionTaskTimeoutSecs] = useState(0);
  const [orgSpecSaving, setOrgSpecSaving] = useState(false);
  const [menuOpenId, setMenuOpenId] = useState<string | null>(null);
  const [menuPlacement, setMenuPlacement] = useState<"down" | "up">("down");
  const [actionTarget, setActionTarget] = useState<{ id: string; name: string; action: "pause" | "resume" | "archive" } | null>(null);
  const [actioning, setActioning] = useState(false);
  const sessionListRef = useRef<HTMLDivElement>(null);

  // Panel / content view state
  const [contentView, setContentView] = useState<ContentView>("chat");
  const [leftCollapsed, setLeftCollapsed] = useState(false);
  const [leftWidth, setLeftWidth] = useState(280);
  const [participantsOpen, setParticipantsOpen] = useState(false);
  const [koiDialogOpen, setKoiDialogOpen] = useState(false);
  const [memberPickerOpen, setMemberPickerOpen] = useState(false);
  const [memberError, setMemberError] = useState("");

  // IDE state
  const [fileTree, setFileTree] = useState<FileNode[]>([]);
  const [tabs, setTabs] = useState<OpenTab[]>([]);
  const [activeTabPath, setActiveTabPath] = useState<string | null>(null);
  const [gitModified, setGitModified] = useState<Set<string>>(new Set());
  const [gitAdded, setGitAdded] = useState<Set<string>>(new Set());
  const [showTerminal, setShowTerminal] = useState(false);
  const [showAssistant, setShowAssistant] = useState(false);
  // File tree multi-select + context menu
  const [collabFileTreeSelection, setCollabFileTreeSelection] = useState<Set<string>>(new Set());
  const [collabFileTreeContextMenu, setCollabFileTreeContextMenu] = useState<FileTreeContextMenu | null>(null);
  const collabFileTreeRef = useRef<HTMLDivElement | null>(null);
  // VS Code-style: clicking an already-active IDE view button collapses
  // the side panel; clicking again expands it. Only applies to
  // explorer / search / git — the views that own the side panel.
  const [sideCollapsed, setSideCollapsed] = useState(false);
  const [terminalHeight, setTerminalHeight] = useState(200);
  const activeTab = tabs.find((t) => t.path === activeTabPath) || null;

  const ideRef = useRef<HTMLDivElement>(null);

  // ─── Load sessions ─────────────────────────────────────────────────
  const loadSessions = useCallback(async () => {
    try {
      dispatch(poolActions.setLoading(true));
      const list = await poolApi.listSessions();
      dispatch(poolActions.setPoolSessions(list));
      const stillValid = activeSessionId && list.some(s => s.id === activeSessionId);
      if (!stillValid && list.length > 0) {
        dispatch(poolActions.setActivePoolSession(list[0].id));
      }
    } catch {
      // silently ignore
    } finally {
      dispatch(poolActions.setLoading(false));
    }
  }, [dispatch, activeSessionId]);

  // ─── Load messages ─────────────────────────────────────────────────
  const loadMessages = useCallback(async (sessionId: string) => {
    try {
      const msgs = await poolApi.getMessages({ session_id: sessionId, limit: INITIAL_LOAD_SIZE });
      dispatch(poolActions.setPoolMessages({
        sessionId,
        messages: msgs,
        hasMore: msgs.length === INITIAL_LOAD_SIZE,
      }));
      if (msgs.length > 0) {
        prevLastIdRef.current = msgs[msgs.length - 1].id;
      }
    } catch {
      // silently ignore
    }
  }, [dispatch]);

  const scrollCancelRef = useRef<(() => void) | null>(null);

  const loadOlderMessages = useCallback(async (sessionId: string, currentCount: number) => {
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
      setCapacity((c) => c + LAZY_LOAD_STEP);
    } else {
      dispatch(poolActions.prependPoolMessages({ sessionId, messages: [], hasMore: false }));
      scrollCancelRef.current?.();
    }
  }, [dispatch]);

  const scrollHistory = useScrollPrependedHistory({
    containerRef: messagesContainerRef,
    itemCount: messages.length,
    hasMore,
    setLoading: setLoadingMore,
    loadOlder: () => {
      if (!activeSessionId) return Promise.resolve();
      return loadOlderMessages(activeSessionId, messages.length);
    },
    active: Boolean(activeSessionId),
  });
  scrollCancelRef.current = scrollHistory.cancelPendingRestore;

  // ─── Init ──────────────────────────────────────────────────────────
  useEffect(() => {
    loadSessions();
    if (kois.length === 0) {
      koiApi.list().then((list) => dispatch(koiActions.setKois(list))).catch(() => {});
    }
  }, [loadSessions, dispatch, kois.length]);

  // Scroll @mention dropdown to keep selected item visible
  useEffect(() => {
    if (mentionFilter === null) return;
    const activeEl = document.querySelector('.collab-mention-item.active');
    if (activeEl) {
      activeEl.scrollIntoView({ block: 'nearest' });
    }
  }, [mentionIndex, mentionFilter]);

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
    scrolledSessionRef.current = null; // reset scroll flag for new session
    loadMessages(activeSessionId);

    let unlisten: (() => void) | null = null;
    poolApi.onMessage(activeSessionId, (msg) => {
      dispatch(poolActions.appendPoolMessage(msg));
    }).then((fn) => { unlisten = fn; });

    return () => { unlisten?.(); };
  }, [activeSessionId, loadMessages, dispatch]);

  // Leaving chat unmounts the scroll container; clear the pin so re-entry scrolls to bottom.
  useEffect(() => {
    if (contentView !== "chat") {
      scrolledSessionRef.current = null;
    }
  }, [contentView]);

  // Scroll to bottom when messages are first loaded for a session.
  //
  // MessageBubble renders markdown / code blocks asynchronously, so a
  // single rAF after `messages.length` changes is not enough — the
  // container's scrollHeight keeps growing for several frames after
  // mount. We pin to the bottom for a short window (~600ms) using a
  // ResizeObserver, then mark this session as scrolled so subsequent
  // appends use the existing near-bottom heuristic.
  //
  // Also fires when the user switches *back* into the chat view from
  // explorer/search/git/board/inbox — the chat scroll container is
  // unmounted while those views are active, so the new mount needs to
  // be re-pinned (key includes contentView so each entry re-pins).
  useEffect(() => {
    if (contentView !== "chat") return;
    if (!activeSessionId || messages.length === 0) return;
    const pinKey = `${activeSessionId}|${contentView}`;
    if (scrolledSessionRef.current === pinKey) return;
    const el = messagesContainerRef.current;
    if (!el) return;

    const sessionAtStart = activeSessionId;
    let cancelled = false;

    const pin = () => {
      if (cancelled) return;
      if (scrolledSessionRef.current === pinKey) return;
      // Only scroll if we're still on the same session + chat view.
      if (activeSessionId !== sessionAtStart) return;
      el.scrollTop = el.scrollHeight;
    };

    // First frame.
    requestAnimationFrame(pin);

    // Keep pinning while content grows (markdown / code highlighting /
    // images). Stop after a short settle window.
    const ro = new ResizeObserver(() => pin());
    ro.observe(el);
    Array.from(el.children).forEach((child) => ro.observe(child as Element));

    const finalize = window.setTimeout(() => {
      cancelled = true;
      ro.disconnect();
      // Final pin in case a late layout pass landed exactly on this tick.
      if (scrolledSessionRef.current !== pinKey && activeSessionId === sessionAtStart) {
        el.scrollTop = el.scrollHeight;
      }
      scrolledSessionRef.current = pinKey;
    }, 600);

    return () => {
      cancelled = true;
      ro.disconnect();
      window.clearTimeout(finalize);
    };
  }, [activeSessionId, messages.length, contentView]);

  // Real-time append: trim FIFO, auto-scroll
  useEffect(() => {
    const el = messagesContainerRef.current;
    if (!el || messages.length === 0) return;
    const lastId = messages[messages.length - 1].id;
    const isAppend = lastId !== prevLastIdRef.current && prevLastIdRef.current !== null;
    prevLastIdRef.current = lastId;
    if (!isAppend) return;

    if (activeSessionId && messages.length > capacity) {
      dispatch(poolActions.trimPoolMessages({ sessionId: activeSessionId, capacity }));
    }

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

  const handleMessagesScroll = useCallback((e: React.UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    const scrollable = el.scrollHeight - el.clientHeight;
    const nearBottom = scrollable <= 0 || el.scrollTop >= scrollable * 0.9;
    if (nearBottom) setUnreadCount(0);
    scrollHistory.handleScroll(e);
  }, [scrollHistory]);

  const handleRemoveMember = useCallback(async (koiId: string) => {
    if (!activeSessionId) return;
    setMemberError("");
    try {
      await poolApi.removeMember(activeSessionId, koiId);
    } catch (e) {
      setMemberError(String(e));
    }
  }, [activeSessionId]);

  // ─── Pool session listeners ────────────────────────────────────────
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<{ id: string; status: string; member_koi_ids?: string[] }>("pool_session_updated", (e) => {
      dispatch(poolActions.updatePoolSessionStatus({ id: e.payload.id, status: e.payload.status }));
      if (e.payload.member_koi_ids) {
        dispatch(poolActions.updatePoolSessionMembers({ id: e.payload.id, memberKoiIds: e.payload.member_koi_ids }));
      }
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [dispatch]);

  // ─── File tree / git ────────────────────────────────────────────────
  const loadFileTree = useCallback(async () => {
    if (!projectDir) return;
    try {
      const nodes = await ideApi.listFiles(projectDir, 8);
      setFileTree(nodes);
    } catch (e) {
      console.error("Failed to load file tree:", e);
    }
  }, [projectDir]);

  const loadGitStatus = useCallback(async () => {
    if (!projectDir) return;
    try {
      const statuses = await ideApi.gitStatus(projectDir);
      const modified = new Set<string>();
      const added = new Set<string>();
      statuses.forEach((s: GitFileStatus) => {
        if (s.status === "modified") modified.add(s.path);
        else if (s.status === "added" || s.status === "untracked") added.add(s.path);
      });
      setGitModified(modified);
      setGitAdded(added);
    } catch {
      // ignore
    }
  }, [projectDir]);

  // Debounced file-change refresh
  const refreshTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const scheduleRefresh = useCallback(() => {
    if (refreshTimer.current) clearTimeout(refreshTimer.current);
    refreshTimer.current = setTimeout(() => {
      refreshTimer.current = null;
      loadFileTree();
      loadGitStatus();
    }, 250);
  }, [loadFileTree, loadGitStatus]);

  useEffect(() => {
    if (!projectDir) {
      setGitModified(new Set());
      setGitAdded(new Set());
      setFileTree([]);
      setTabs([]);
      setActiveTabPath(null);
      return;
    }
    loadFileTree();
    loadGitStatus();
    ideApi.startWatcher(projectDir).catch(() => {});
    const unlistenPromise = onFileChanged((evt) => {
      if (evt.project_dir !== projectDir) return;
      // Normalize to `/` so `tab.path` (always stored with `/`) compares
      // equal even when the watcher emits OS-native separators.
      const evtPath = evt.path.replace(/\\/g, "/");
      scheduleRefresh();
      setTabs((prev) =>
        prev.map((tab) => {
          if (tab.path === evtPath && !tab.isDirty) {
            ideApi.readFile(`${projectDir}/${evtPath}`).then((fc) => {
              setTabs((p) => p.map((t) =>
                t.path === evtPath && !t.isDirty ? { ...t, content: fc.content } : t));
            }).catch(() => {});
          }
          return tab;
        }),
      );
    });
    return () => {
      if (refreshTimer.current) { clearTimeout(refreshTimer.current); refreshTimer.current = null; }
      unlistenPromise.then((fn) => fn());
      ideApi.stopWatcher(projectDir).catch(() => {});
    };
  }, [projectDir, loadFileTree, loadGitStatus, scheduleRefresh]);

  // ─── File open ──────────────────────────────────────────────────────
  const openFile = useCallback(async (path: string, readOnly = false) => {
    const existing = tabs.find((t) => t.path === path);
    if (existing) { setActiveTabPath(path); return; }
    const fullPath = projectDir ? `${projectDir}/${path}` : path;
    try {
      const fc = await ideApi.readFile(fullPath);
      if (fc.is_binary) return;
      const newTab: OpenTab = { path, name: path.split("/").pop() || path, language: fc.language, content: fc.content, isDirty: false, isReadOnly: readOnly };
      setTabs((prev) => [...prev, newTab]);
      setActiveTabPath(path);
    } catch (e) { console.error("Failed to read file:", e); }
  }, [projectDir, tabs]);

  const openDiff = useCallback(async (path: string) => {
    if (!projectDir) return;
    const diffPath = `diff:${path}`;
    if (tabs.find((t) => t.path === diffPath)) { setActiveTabPath(diffPath); return; }
    try {
      const diff = await ideApi.gitDiff(projectDir, path);
      setTabs((prev) => [...prev, { path: diffPath, name: `${path} (diff)`, language: null, content: diff.modified, isDirty: false, isReadOnly: true, isDiff: true, originalContent: diff.original }]);
      setActiveTabPath(diffPath);
    } catch (e) { console.error("Failed to get diff:", e); }
  }, [projectDir, tabs]);

  // ─── Editor ─────────────────────────────────────────────────────────
  const handleEditorChange = useCallback((value: string) => {
    if (!activeTabPath) return;
    setTabs((prev) => prev.map((t) => t.path === activeTabPath ? { ...t, content: value, isDirty: true } : t));
  }, [activeTabPath]);

  const saveFile = useCallback(async (path: string) => {
    const tab = tabs.find((t) => t.path === path);
    if (!tab || !projectDir) return;
    try {
      await ideApi.writeFile(`${projectDir}/${path}`, tab.content);
      setTabs((prev) => prev.map((t) => (t.path === path ? { ...t, isDirty: false } : t)));
      loadGitStatus();
    } catch (e) { console.error("Failed to save:", e); }
  }, [tabs, projectDir, loadGitStatus]);

  const closeTab = useCallback((path: string) => {
    setTabs((prev) => {
      const idx = prev.findIndex((t) => t.path === path);
      const next = prev.filter((t) => t.path !== path);
      if (activeTabPath === path) setActiveTabPath(next[Math.min(idx, next.length - 1)]?.path || null);
      return next;
    });
  }, [activeTabPath]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === "s") {
        e.preventDefault();
        if (activeTabPath && !activeTabPath.startsWith("diff:")) saveFile(activeTabPath);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [activeTabPath, saveFile]);

  // ─── Panel resize handler (left) ───────────────────────────────────
  const startLeftResize = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    const startX = e.clientX;
    const startW = leftWidth;
    const onMove = (ev: MouseEvent) => setLeftWidth(Math.min(500, Math.max(220, startW + (ev.clientX - startX))));
    const onUp = () => { window.removeEventListener("mousemove", onMove); window.removeEventListener("mouseup", onUp); document.body.style.cursor = ""; document.body.style.userSelect = ""; };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  }, [leftWidth]);

  // Auto-set Board filter when viewing Board tab
  useEffect(() => {
    if (contentView === "board" && activeSessionId) {
      dispatch(boardActions.setFilterSessionId(activeSessionId));
    }
  }, [contentView, activeSessionId, dispatch]);

  useEffect(() => {
    if (!collabFileTreeContextMenu) return;
    const dismiss = () => setCollabFileTreeContextMenu(null);
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") dismiss(); };
    window.addEventListener("click", dismiss);
    window.addEventListener("contextmenu", dismiss);
    window.addEventListener("keydown", onKey);
    window.addEventListener("scroll", dismiss, true);
    return () => {
      window.removeEventListener("click", dismiss);
      window.removeEventListener("contextmenu", dismiss);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", dismiss, true);
    };
  }, [collabFileTreeContextMenu]);

  const startTerminalResize = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    const startY = e.clientY;
    const startH = terminalHeight;
    const onMove = (ev: MouseEvent) => setTerminalHeight(Math.min(400, Math.max(120, startH + (startY - ev.clientY))));
    const onUp = () => { window.removeEventListener("mousemove", onMove); window.removeEventListener("mouseup", onUp); document.body.style.cursor = ""; document.body.style.userSelect = ""; };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    document.body.style.cursor = "row-resize";
    document.body.style.userSelect = "none";
  }, [terminalHeight]);

  // ─── Session management ────────────────────────────────────────────
  useEffect(() => {
    if (activeSession) {
      setOrgSpecDraft(activeSession.org_spec || "");
      setSessionTaskTimeoutSecs(activeSession.task_timeout_secs ?? 0);
    }
  }, [activeSession]);

  const handleCreateSession = async () => {
    const name = newName.trim();
    if (!name) return;
    if (!newProjectDir.trim()) return;
    try {
      setCreating(true);
      const session = await poolApi.createSession(name, newProjectDir.trim(), newTaskTimeoutSecs);
      dispatch(poolActions.addPoolSession(session));
      dispatch(poolActions.setActivePoolSession(session.id));
      setNewName("");
      setNewProjectDir("");
      setNewTaskTimeoutSecs(0);
      setShowNewDialog(false);
    } catch {
      // silently ignore
    } finally {
      setCreating(false);
    }
  };

  const handleDeleteSession = async (id: string) => {
    try { await poolApi.deleteSession(id); dispatch(poolActions.removePoolSession(id)); } catch {}
  };

  const handleBindProjectDir = async () => {
    if (!activeSessionId) return;
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const dir = await open({ directory: true, multiple: false, title: t("pool.selectProjectDir") || "Select working directory" });
      if (dir && typeof dir === "string") {
        await poolApi.updateSessionDir(activeSessionId, dir);
        dispatch(poolActions.updatePoolSessionDir({ id: activeSessionId, projectDir: dir }));
      }
    } catch { /* dialog not available */ }
  };

  const confirmDeleteSession = async () => {
    if (!deleteTarget) return;
    setDeleting(true);
    try { await handleDeleteSession(deleteTarget.id); setDeleteTarget(null); } finally { setDeleting(false); }
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
    } catch (e) { console.error("[Collab] session action error:", e); }
    finally { setActioning(false); setActionTarget(null); }
  };

  const handleSaveOrgSpec = async () => {
    if (!activeSessionId) return;
    setOrgSpecSaving(true);
    try {
      await poolApi.updateOrgSpec(activeSessionId, orgSpecDraft);
      await poolApi.updateConfig(activeSessionId, sessionTaskTimeoutSecs);
      loadSessions();
    } catch (e) { console.error("[Collab] save org_spec error:", e); }
    finally { setOrgSpecSaving(false); }
  };

  // Close menu on outside click
  useEffect(() => {
    if (!menuOpenId) return;
    const handler = () => setMenuOpenId(null);
    document.addEventListener("click", handler);
    return () => document.removeEventListener("click", handler);
  }, [menuOpenId]);

  // ─── User message input ────────────────────────────────────────────
  const handleSendMessage = async () => {
    const text = userInput.trim();
    if (!text || !activeSessionId) return;
    if (containsDelegatedPiscisMention(text)) {
      setMentionError(t("pool.noDelegateSelfPiscis"));
      setTimeout(() => setMentionError(""), 8000);
      return;
    }
    if (!hasMentions(text)) {
      setMentionError(t("pool.mustMention") || "Message requires a recipient. Use @name to mention someone, or @all to send to everyone.");
      setTimeout(() => setMentionError(""), 5000);
      return;
    }
    setSending(true);
    setMentionError("");
    try {
      const mentions = parseMentions(text);
      const metadata = mentions.includes("all") ? "all" : mentions.join(",");
      await poolApi.sendMessage({
        session_id: activeSessionId,
        sender_id: "piscis",
        content: text,
        msg_type: "mention",
        metadata,
      });
      setUserInput("");
    } catch (e) {
      console.error("[Collab] send message error:", e);
      const msg = typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
      setMentionError(msg);
      setTimeout(() => setMentionError(""), 8000);
    } finally {
      setSending(false);
    }
  };

  const handleInputKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // @mention dropdown navigation
    if (mentionFilter !== null) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setMentionIndex((i) => Math.min(i + 1, filteredMentions.length - 1));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setMentionIndex((i) => Math.max(i - 1, 0));
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setMentionFilter(null);
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        const target = filteredMentions[mentionIndex];
        if (target) insertMention(target.name);
        return;
      }
    }
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      handleSendMessage();
    }
  };

  // ─── Render ────────────────────────────────────────────────────────

  return (
    <div className="collab" ref={ideRef}>
      {/* LEFT: Project list panel (or collapsed bar) */}
      {!leftCollapsed ? (
        <>
          <div className="collab-left" style={{ width: leftWidth }}>
            <div className="collab-left-inner">
              <div className="collab-left-toolbar">
                <span className="collab-left-title">{t("pool.projects") || "Projects"}</span>
                <button className="collab-icon-btn" onClick={() => setLeftCollapsed(true)} title={t("common.collapse") || "Collapse"}>«</button>
              </div>
              <button className="chatpool-new-btn" onClick={() => setShowNewDialog(true)}>
                + {t("pool.newSession") || "New Project"}
              </button>

              {showNewDialog && (
                <div className="chatpool-new-dialog">
                  <input className="chatpool-input" value={newName} onChange={(e) => setNewName(e.target.value)} placeholder={t("pool.sessionPlaceholder") || "Project name"} autoFocus onKeyDown={(e) => e.key === "Enter" && handleCreateSession()} />
                  <div className="collab-project-dir-row">
                    <input className="chatpool-input" value={newProjectDir} onChange={(e) => setNewProjectDir(e.target.value)} placeholder={t("pool.selectProjectDir") || "Working directory"} onKeyDown={(e) => e.key === "Enter" && handleCreateSession()} />
                    <button className="chatpool-btn chatpool-btn-secondary" onClick={async () => { try { const { open } = await import("@tauri-apps/plugin-dialog"); const dir = await open({ directory: true, multiple: false, title: t("pool.selectProjectDir") || "Select working directory" }); if (dir && typeof dir === "string") setNewProjectDir(dir); } catch {} }}>{t("pool.selectProjectDirBrowse") || "Browse..."}</button>
                  </div>
                  <input className="chatpool-input" type="number" min={0} max={7200} value={newTaskTimeoutSecs} onChange={(e) => { const v = Number(e.target.value); setNewTaskTimeoutSecs(Number.isFinite(v) ? Math.max(0, Math.min(7200, v)) : 0); }} placeholder={t("pool.taskTimeoutPlaceholder") || "Timeout"} />
                  <div className="chatpool-empty-hint">{t("pool.taskTimeoutHelp")}</div>
                  <div className="chatpool-new-actions">
                    <button className="chatpool-btn chatpool-btn-secondary" onClick={() => { setShowNewDialog(false); setNewName(""); setNewProjectDir(""); }}>{t("koi.cancel") || "Cancel"}</button>
                    <button className="chatpool-btn chatpool-btn-primary" onClick={handleCreateSession} disabled={creating || !newName.trim() || !newProjectDir.trim()}>{t("koi.create") || "Create"}</button>
                  </div>
                </div>
              )}

              <div className="chatpool-session-list" ref={sessionListRef}>
                {loading && sessions.length === 0 && <div className="chatpool-empty-hint">{t("common.loading") || "Loading..."}</div>}
                {!loading && sessions.length === 0 && <div className="chatpool-empty-hint">{t("pool.noSessions") || "No projects yet"}</div>}
                {sessions.map((s) => {
                  const statusColor = s.status === "active" ? "#22c55e" : s.status === "paused" ? "#f59e0b" : "#6b7280";
                  const isMenuOpen = menuOpenId === s.id;
                  return (
                    <div key={s.id} className={`chatpool-session-item ${s.id === activeSessionId ? "active" : ""}${s.status === "archived" ? " chatpool-session-archived" : ""}${isMenuOpen ? " chatpool-session-item--menu-open" : ""}`}
                      onClick={() => {
                        dispatch(poolActions.setActivePoolSession(s.id));
                        setContentView("chat");
                      }}
                    >
                      <div className="chatpool-session-name"><span className="chatpool-status-dot" style={{ background: statusColor }} />{s.name}</div>
                      <div className="chatpool-session-time">{formatTime(s.updated_at)}</div>
                      <div className="chatpool-session-menu-wrap" onClick={(e) => e.stopPropagation()}>
                        <button className="chatpool-session-menu-btn" title={t("pool.sessionActions") || "Actions"}
                          onClick={(e) => {
                            e.stopPropagation();
                            if (isMenuOpen) { setMenuOpenId(null); return; }
                            const listRect = sessionListRef.current?.getBoundingClientRect();
                            const buttonRect = (e.currentTarget as HTMLButtonElement).getBoundingClientRect();
                            if (listRect) { const estimatedMenuHeight = s.status === "archived" ? 86 : 120; const spaceBelow = listRect.bottom - buttonRect.bottom; const spaceAbove = buttonRect.top - listRect.top; setMenuPlacement(spaceBelow < estimatedMenuHeight && spaceAbove > spaceBelow ? "up" : "down"); }
                            else { setMenuPlacement("down"); }
                            setMenuOpenId(s.id);
                          }}>⋯</button>
                        {isMenuOpen && (
                          <div className={`chatpool-session-menu ${menuPlacement === "up" ? "chatpool-session-menu--up" : ""}`}>
                            {s.status === "active" && <button className="chatpool-menu-item chatpool-menu-item--warn" onClick={() => { setMenuOpenId(null); setActionTarget({ id: s.id, name: s.name, action: "pause" }); }}>⏸ {t("pool.pauseSession") || "Pause"}</button>}
                            {(s.status === "paused" || s.status === "archived") && <button className="chatpool-menu-item chatpool-menu-item--ok" onClick={() => { setMenuOpenId(null); setActionTarget({ id: s.id, name: s.name, action: "resume" }); }}>▶ {t("pool.resumeSession") || "Resume"}</button>}
                            {s.status !== "archived" && <button className="chatpool-menu-item" onClick={() => { setMenuOpenId(null); setActionTarget({ id: s.id, name: s.name, action: "archive" }); }}>🗄 {t("pool.archiveSession") || "Archive"}</button>}
                            <div className="chatpool-menu-divider" />
                            <button className="chatpool-menu-item chatpool-menu-item--danger" onClick={() => { setMenuOpenId(null); setDeleteTarget({ id: s.id, name: s.name }); }}>🗑 {t("pool.deleteSession") || "Delete"}</button>
                          </div>
                        )}
                      </div>
                    </div>
                  );
                })}
              </div>

              {/* Participants section - collapsible */}
              <div className="chatpool-orgspec-panel">
                <div className="chatpool-orgspec-header" onClick={() => setParticipantsOpen(!participantsOpen)}>
                  <span>{t("pool.participants") || "Participants"}</span>
                  <span style={{ display: "flex", alignItems: "center", gap: 4 }}>
                    <button className="collab-icon-btn" disabled={!activeSessionId} title={t("pool.memberPickerTitle") || "Add members"} onClick={(e) => { e.stopPropagation(); if (activeSessionId) setMemberPickerOpen(true); }}>⚙</button>
                    <span className="chatpool-orgspec-chevron">{participantsOpen ? "▲" : "▼"}</span>
                  </span>
                </div>
                {participantsOpen && (
                  <div className="chatpool-orgspec-body chatpool-participants-body">
                    <div className="chatpool-participant">
                      <span className="chatpool-participant-icon">🐋</span>
                      <span className="chatpool-participant-name">Piscis</span>
                      <span className="chatpool-participant-badge" title={t("pool.actAsPiscisRole")}>{t("pool.mainAgent") || "Main Agent"}</span>
                    </div>
                    {poolMembers.map((koi) => (
                      <div key={koi.id} className="chatpool-participant">
                        <span className="chatpool-participant-icon">{koi.icon}</span>
                        <span className="chatpool-participant-name" style={{ color: koi.color }}>{koi.name}</span>
                        <span className="chatpool-participant-dot" style={{ background: STATUS_COLORS[koi.status] || "#6b7280" }} />
                        {koi.active_todo_count > 0 && <span className="chatpool-participant-todos">{koi.active_todo_count}</span>}
                        <button className="chatpool-participant-remove" title={t("pool.removeMember") || "Remove"} onClick={() => handleRemoveMember(koi.id)}>×</button>
                      </div>
                    ))}
                    {poolMembers.length === 0 && (
                      <div className="chatpool-empty-hint">{t("pool.noMembersHint")}</div>
                    )}
                    {memberError && <div className="chatpool-participant-error">{memberError}</div>}
                  </div>
                )}
              </div>

              {activeSessionId && (
                <div className="chatpool-orgspec-panel">
                  <div className="chatpool-orgspec-header" onClick={() => setOrgSpecOpen(!orgSpecOpen)}>
                    <span>{t("pool.orgSpec") || "Project Spec"}</span>
                    <span className="chatpool-orgspec-chevron">{orgSpecOpen ? "▲" : "▼"}</span>
                  </div>
                  {orgSpecOpen && (
                    <div className="chatpool-orgspec-body">
                      <label className="koi-form-label">{t("pool.taskTimeoutField")}</label>
                      <input className="chatpool-input" type="number" min={0} max={7200} value={sessionTaskTimeoutSecs} onChange={(e) => { const v = Number(e.target.value); setSessionTaskTimeoutSecs(Number.isFinite(v) ? Math.max(0, Math.min(7200, v)) : 0); }} />
                      <div className="chatpool-empty-hint">{t("pool.taskTimeoutHelp")}</div>
                      <textarea className="chatpool-orgspec-editor" value={orgSpecDraft} onChange={(e) => setOrgSpecDraft(e.target.value)} placeholder="# Project Goal\n\n# Koi Roles\n\n# Collaboration Rules\n\n# Success Metrics" rows={10} />
                      <button className="chatpool-btn chatpool-btn-primary" onClick={handleSaveOrgSpec} disabled={orgSpecSaving || (orgSpecDraft === (activeSession?.org_spec || "") && sessionTaskTimeoutSecs === (activeSession?.task_timeout_secs ?? 0))} style={{ alignSelf: "flex-end", marginTop: 6 }}>{orgSpecSaving ? "Saving..." : (t("common.save") || "Save")}</button>
                    </div>
                  )}
                </div>
              )}
            </div>
          </div>
          <div className="collab-resize-handle" onMouseDown={startLeftResize} />
        </>
      ) : (
        <div className="collab-left-collapsed-bar">
          <button className="collab-left-bookmark" onClick={() => setLeftCollapsed(false)} title={t("common.expand") || "Expand"}>
            📑
          </button>
        </div>
      )}

      {/* ── CENTER ────────────────────────────────────────────────────
          Vertical column:
            1) .collab-content-shell  (horizontal: main view + optional
               right-side IDE panel)
            2) .ide-resize-handle-v + bottom panel (terminal OR assistant)
               — spans the full center width, matching VS Code's
               integrated terminal behavior.
      ──────────────────────────────────────────────────────────────── */}
      <div className="collab-center">
        {/* ── Content shell (main view + optional right-side panel) ── */}
        <div className="collab-content-shell">
          {/* ── Main view host ───────────────────────────────────────
              All mutually-exclusive views (chat, IDE editor, board,
              inbox, koiObserver) live here. Future view additions
              only need to add a new conditional child — the layout
              structure above/below stays unchanged. ──────────────── */}
          <div className="collab-main-view">
            {/* Chat view */}
            {contentView === "chat" && (
              <>
                <div className="collab-chat-area">
                  {!activeSessionId ? (
                    <div className="collab-empty"><span className="collab-empty-icon">💬</span><p>{t("pool.noSessions") || "No projects yet"}</p><button className="chatpool-btn chatpool-btn-primary" onClick={() => setShowNewDialog(true)} style={{ marginTop: 12 }}>+ {t("pool.newSession") || "New Project"}</button></div>
                  ) : messages.length === 0 ? (
                    <div className="collab-empty"><span className="collab-empty-icon">💬</span><p>{t("pool.noMessages") || "No messages yet"}</p></div>
                  ) : (
                    <div className="collab-messages-scroll" ref={messagesContainerRef} onScroll={handleMessagesScroll}>
                      {hasMore && (
                        <button
                          type="button"
                          className="chatpool-load-more-btn"
                          disabled={loadingMore}
                          onClick={() => scrollHistory.loadOlder()}
                        >
                          {loadingMore ? (t("common.loading") || "Loading...") : (t("common.loadMore") || "Load more")}
                        </button>
                      )}
                      {messages.map((msg) => (<MessageBubble key={msg.id} msg={msg} kois={kois} />))}
                      <div ref={messagesEndRef} />
                    </div>
                  )}
                  {unreadCount > 0 && <button className="chatpool-unread-badge" onClick={scrollToBottom}>↓ {unreadCount} 条新消息</button>}
                </div>
                <div className="collab-input-area">
                  {mentionError && <div className="collab-mention-error">{mentionError}</div>}
                  {mentionFilter !== null && filteredMentions.length > 0 && (
                    <div className="collab-mention-dropdown">
                      {filteredMentions.map((m, i) => (
                        <div key={m.name} className={`collab-mention-item${i === mentionIndex ? " active" : ""}`}
                          onMouseDown={(e) => { e.preventDefault(); insertMention(m.name); }} onMouseEnter={() => setMentionIndex(i)}>
                          <span className="collab-mention-icon">{m.icon}</span>
                          <span className="collab-mention-name">@!{m.name}</span>
                          <span className="collab-mention-desc">{m.desc}</span>
                        </div>
                      ))}
                      <div className="collab-mention-hint">↑↓ {t("common.navigate") || "navigate"} &nbsp; Enter {t("common.select") || "select"} &nbsp; Esc {t("common.dismiss") || "dismiss"}</div>
                    </div>
                  )}
                  <div className="collab-input-row">
                    <textarea className="collab-input" ref={inputRef} value={userInput} onChange={handleInputChange} onKeyDown={handleInputKeyDown} placeholder={t("pool.messageInputPlaceholder")} rows={3} disabled={!activeSessionId || sending} />
                    <button className="chatpool-btn chatpool-btn-primary" onClick={handleSendMessage} disabled={sending || !userInput.trim() || !activeSessionId} title={t("pool.sendShortcut")}>{sending ? "..." : t("common.send")}</button>
                  </div>
                </div>
              </>
            )}

            {/* IDE editor view (file tabs + Monaco CodeEditor, or empty hint) */}
            {(contentView === "explorer" || contentView === "search" || contentView === "git") && (
              <div className="collab-ide-main">
                {activeTab ? (
                  <>
                    <EditorTabs tabs={tabs} activeTabPath={activeTabPath} onTabClick={setActiveTabPath} onTabClose={closeTab} contextMenu={null} />
                    <div className="ide-editor" style={{ flex: 1, minHeight: 120 }}><CodeEditor tab={activeTab} theme="violet" projectDir={projectDir} onChange={handleEditorChange} /></div>
                  </>
                ) : (
                  <div className="collab-empty">
                    <span className="collab-empty-icon">{contentView === "explorer" ? "📁" : contentView === "search" ? "🔍" : "⑂"}</span>
                    <p>{contentView === "explorer" ? (t("ide.openFileHint") || "Select a file from the explorer") : contentView === "search" ? (t("ide.searchHint") || "Search for files in the project") : (t("ide.gitHint") || "View source control changes")}</p>
                  </div>
                )}
              </div>
            )}

            {/* Board view */}
            {contentView === "board" && <Board />}

            {/* Inbox view */}
            {contentView === "inbox" && (
              <PiscisInbox mode="coordination" poolSessionId={activeSessionId} />
            )}

            {/* Koi Observer view */}
            {contentView === "koiObserver" && (
              <PiscisInbox mode="koiObserver" poolSessionId={activeSessionId} />
            )}
          </div>

          {/* ── IDE side panel (right side, adjacent to icon strip) ──
              Rendered LAST inside .collab-content-shell so it sits to
              the RIGHT of .collab-main-view (VS Code convention).
              Visible only when an IDE view is active and not collapsed. */}
          {(contentView === "explorer" || contentView === "search" || contentView === "git") && !sideCollapsed && (
            <div className="collab-ide-side">
              {projectDir ? (
                <>
                  {contentView === "explorer" && (
                    <FileTree
                                          nodes={fileTree}
                                          activePath={activeTabPath}
                                          selectedPaths={collabFileTreeSelection}
                                          gitModified={gitModified}
                                          gitAdded={gitAdded}
                                          projectDir={projectDir}
                                          onFileClick={(node) => openFile(node.path)}
                                          onRefresh={() => { loadFileTree(); loadGitStatus(); }}
                                          onSelect={(path, opts) => {
                                            setCollabFileTreeSelection((prev) => {
                                              if (opts.multi) {
                                                const next = new Set(prev);
                                                if (next.has(path)) next.delete(path);
                                                else next.add(path);
                                                return next;
                                              }
                                              return new Set([path]);
                                            });
                                          }}
                                          onContextMenu={(menu) => setCollabFileTreeContextMenu(menu)}
                                          containerRef={collabFileTreeRef}
                                        />
                  )}
                  {contentView === "search" && (
                    <SearchPanel projectDir={projectDir} onResultClick={(path, _line) => openFile(path)} />
                  )}
                  {contentView === "git" && (
                    <GitPanel projectDir={projectDir} onDiffClick={(path) => openDiff(path)} onRefresh={loadGitStatus} />
                  )}
                </>
              ) : (
                <div className="ide-no-project"><div className="icon">📂</div><div>{t("ide.noProjectDir") || "No project directory configured."}</div><button className="chatpool-btn chatpool-btn-primary" onClick={handleBindProjectDir} style={{ marginTop: 10 }}>{t("pool.bindProjectDir") || "Associate / Create Project Directory"}</button></div>
              )}
            </div>
          )}
        </div>

        {/* ── Bottom panel (terminal / assistant, full-width) ──────────
            Sibling of .collab-content-shell inside .collab-center, so
            it spans both the main view and the IDE side panel —
            matching VS Code's integrated terminal. Terminal and
            assistant are mutually exclusive. */}
        {showTerminal && projectDir && !showAssistant && (
          <>
            <div className="ide-resize-handle-v" onMouseDown={startTerminalResize} />
            <TerminalPanel projectDir={projectDir} visible={showTerminal} onClose={() => setShowTerminal(false)} height={terminalHeight} />
          </>
        )}
        {showAssistant && (
          <>
            <div className="ide-resize-handle-v" onMouseDown={startTerminalResize} />
            <AssistantPanel projectDir={projectDir} visible={showAssistant} onClose={() => setShowAssistant(false)} height={terminalHeight} />
          </>
        )}
      </div>

      {/* RIGHT: Icon tab strip (always visible, no collapse) */}
      <div className="collab-right">
        <div className="collab-right-icons">
          {Object.entries(VIEW_ICONS).map(([view, icon]) => {
            const isIdeView = view === "explorer" || view === "search" || view === "git";
            const isActiveView = contentView === view;
            // Active highlight on the icon: an IDE view counts as
            // "active" only while its side panel is expanded — so the
            // collapse state is reflected on the icon strip too.
            const isHighlighted = isActiveView && (!isIdeView || !sideCollapsed);
            return (
              <button
                key={view}
                className={`collab-right-icon${isHighlighted ? " active" : ""}`}
                onClick={() => {
                  if (isActiveView && isIdeView) {
                    // Same IDE view button clicked again → toggle the
                    // side panel (collapse / expand).
                    setSideCollapsed((c) => !c);
                  } else {
                    setContentView(view as ContentView);
                    // Switching to any view from a collapsed IDE view
                    // should re-expand the side panel.
                    setSideCollapsed(false);
                  }
                }}
                title={t(`pond.tab${view.charAt(0).toUpperCase() + view.slice(1)}`) || view}
              >
                <span className="activity-icon">{icon}</span>
                {view === "git" && (gitModified.size + gitAdded.size) > 0 && <span className="activity-badge">{gitModified.size + gitAdded.size}</span>}
              </button>
            );
          })}
          <div style={{ flex: 1 }} />
          <button
            className={`collab-right-icon${showAssistant ? " active" : ""}`}
            onClick={() => { setShowAssistant((v) => !v); if (!showAssistant) setShowTerminal(false); }}
            title={t("ide.assistant") || "Assistant"}
          >
            <span className="activity-icon">🤖</span>
          </button>
          <button
            className={`collab-right-icon${showTerminal ? " active" : ""}`}
            onClick={() => { setShowTerminal((v) => !v); if (!showTerminal) setShowAssistant(false); }}
            title={t("ide.terminal") || "Terminal"}
          >
            <span className="activity-icon">⌨</span>
          </button>
        </div>
      </div>

      {collabFileTreeContextMenu && (
        <div
          className="ide-tab-context-menu"
          style={{
            position: "fixed",
            left: collabFileTreeContextMenu.x,
            top: collabFileTreeContextMenu.y,
            zIndex: 1000,
          }}
          onClick={(e) => e.stopPropagation()}
        >
          <button onClick={() => { openFile(collabFileTreeContextMenu.targetPath); setCollabFileTreeContextMenu(null); }}>
            {t("ide.openFile") || "Open"}
          </button>
          <button onClick={() => { (collabFileTreeRef.current as HTMLDivElement & { renameActive?: () => void })?.renameActive?.(); setCollabFileTreeContextMenu(null); }}>
            {t("ide.renameFile") || "Rename"}
          </button>
          <button onClick={() => { (collabFileTreeRef.current as HTMLDivElement & { deleteSelected?: () => void })?.deleteSelected?.(); setCollabFileTreeContextMenu(null); }}>
            {t("ide.deleteFile") || "Delete"}
          </button>
          <div className="ide-tab-context-menu-sep" />
          <button onClick={() => {
            if (projectDir) navigator.clipboard.writeText(`${projectDir}/${collabFileTreeContextMenu.targetPath}`).catch(() => {});
            setCollabFileTreeContextMenu(null);
          }}>
            {t("ide.copyPath") || "Copy Path"}
          </button>
          <button onClick={() => {
            navigator.clipboard.writeText(collabFileTreeContextMenu.targetPath).catch(() => {});
            setCollabFileTreeContextMenu(null);
          }}>
            {t("ide.copyRelPath") || "Copy Relative Path"}
          </button>
          <button onClick={() => {
            if (projectDir) openPath(`${projectDir}/${collabFileTreeContextMenu.targetPath}`).catch(() => {});
            setCollabFileTreeContextMenu(null);
          }}>
            {t("ide.revealInExplorer") || "Reveal in File Manager"}
          </button>
          <div className="ide-tab-context-menu-sep" />
          <button onClick={() => { (collabFileTreeRef.current as HTMLDivElement & { startCreate?: (d: boolean) => void })?.startCreate?.(false); setCollabFileTreeContextMenu(null); }}>
            {t("ide.newFile") || "New File"}
          </button>
          <button onClick={() => { (collabFileTreeRef.current as HTMLDivElement & { startCreate?: (d: boolean) => void })?.startCreate?.(true); setCollabFileTreeContextMenu(null); }}>
            {t("ide.newFolder") || "New Folder"}
          </button>
        </div>
      )}

      {/* Dialogs */}
      <ConfirmDialog open={!!deleteTarget} title={t("pool.confirmDeleteTitle") || "Delete Project"} message={t("pool.confirmDeleteMessage", { name: deleteTarget?.name ?? "" }) || "Delete this project?"} confirmLabel={t("common.delete") || "Delete"} cancelLabel={t("common.cancel") || "Cancel"} variant="danger" loading={deleting} onConfirm={confirmDeleteSession} onCancel={() => !deleting && setDeleteTarget(null)} />
      <ConfirmDialog open={!!actionTarget} title={actionTarget?.action === "pause" ? (t("pool.confirmPauseTitle") || "Pause") : actionTarget?.action === "resume" ? (t("pool.confirmResumeTitle") || "Resume") : (t("pool.confirmArchiveTitle") || "Archive")} message={actionTarget?.action === "pause" ? (t("pool.confirmPauseMessage", { name: actionTarget?.name ?? "" }) || "") : actionTarget?.action === "resume" ? (t("pool.confirmResumeMessage", { name: actionTarget?.name ?? "" }) || "") : (t("pool.confirmArchiveMessage", { name: actionTarget?.name ?? "" }) || "")} confirmLabel={actionTarget?.action === "pause" ? (t("pool.pauseSession") || "Pause") : actionTarget?.action === "resume" ? (t("pool.resumeSession") || "Resume") : (t("pool.archiveSession") || "Archive")} cancelLabel={t("common.cancel") || "Cancel"} variant={actionTarget?.action === "archive" ? "danger" : "primary"} loading={actioning} onConfirm={confirmSessionAction} onCancel={() => !actioning && setActionTarget(null)} />

      {/* Project member picker */}
      {memberPickerOpen && activeSessionId && (
        <PoolMemberPicker
          poolId={activeSessionId}
          memberKoiIds={activeSession?.member_koi_ids ?? []}
          onClose={() => setMemberPickerOpen(false)}
          onManageKois={() => { setMemberPickerOpen(false); setKoiDialogOpen(true); }}
        />
      )}

      {/* Koi Manager dialog */}
      {koiDialogOpen && (
        <div className="koi-modal-overlay" onClick={() => setKoiDialogOpen(false)}>
          <div className="koi-modal koi-modal--wide" onClick={(e) => e.stopPropagation()}>
            <KoiManager />
          </div>
        </div>
      )}
    </div>
  );
}
