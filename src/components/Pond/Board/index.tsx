import { useState, useEffect, useCallback, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useSelector, useDispatch } from "react-redux";
import { listen } from "@tauri-apps/api/event";
import { boardApi, koiApi, poolApi, KoiTodo, KoiWithStats, PoolSession } from "../../../services/tauri";
import { RootState, boardActions, koiActions } from "../../../store";
import "./Board.css";

const COLUMNS = [
  { id: "todo", icon: "📋", labelKey: "board.columnTodo" },
  { id: "in_progress", icon: "🔄", labelKey: "board.columnInProgress" },
  { id: "done", icon: "✅", labelKey: "board.columnDone" },
  { id: "blocked", icon: "🚫", labelKey: "board.columnBlocked" },
  { id: "cancelled", icon: "❌", labelKey: "board.columnCancelled" },
];

const PRIORITY_COLORS: Record<string, string> = {
  urgent: "#eb3b5a",
  high: "#fd9644",
  medium: "#45b7d1",
  low: "#778ca3",
};

const PRIORITIES = ["low", "medium", "high", "urgent"] as const;

const SOURCE_ICONS: Record<string, string> = {
  pisci: "🐋",
  koi: "🐟",
  user: "👤",
  system: "⚙️",
};

function TaskCard({
  todo,
  kois,
  t,
  onAction,
  onDetail,
}: {
  todo: KoiTodo;
  kois: KoiWithStats[];
  t: (key: string) => string;
  onAction: (action: string, todoId: string) => void;
  onDetail: (todo: KoiTodo) => void;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const [menuPos, setMenuPos] = useState({ top: 0, left: 0 });
  const triggerRef = useRef<HTMLSpanElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const owner = kois.find((k) => k.id === todo.owner_id);
  const claimer = todo.claimed_by ? kois.find((k) => k.id === todo.claimed_by) : null;
  const color = owner?.color ?? "#6b7280";
  const icon = owner?.icon ?? "🐟";
  const priorityColor = PRIORITY_COLORS[todo.priority] ?? PRIORITY_COLORS.low;
  const priorityKey = `board.priority${todo.priority.charAt(0).toUpperCase() + todo.priority.slice(1)}`;
  const sourceIcon = SOURCE_ICONS[todo.source_type] ?? "?";

  useEffect(() => {
    if (!menuOpen) return;
    const close = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node) &&
          triggerRef.current && !triggerRef.current.contains(e.target as Node)) {
        setMenuOpen(false);
      }
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [menuOpen]);

  const handleAction = (action: string) => {
    setMenuOpen(false);
    onAction(action, todo.id);
  };

  return (
    <div
      className={`board-card ${todo.blocked_reason ? "board-card--blocked" : ""}${menuOpen ? " menu-open" : ""}`}
      onClick={() => onDetail(todo)}
    >
      <div className="board-card-bar" style={{ background: color }} />
      <div className="board-card-content">
        <div className="board-card-top">
          <span className="board-card-icon">{icon}</span>
          <span
            className="board-card-priority"
            style={{ background: priorityColor }}
          >
            {t(priorityKey)}
          </span>
          <span className="board-card-source" title={`Source: ${todo.source_type}`}>{sourceIcon}</span>
          <span
            ref={triggerRef}
            className="board-card-menu-trigger"
            onClick={(e) => {
              e.stopPropagation();
              if (!menuOpen && triggerRef.current) {
                const r = triggerRef.current.getBoundingClientRect();
                setMenuPos({
                  top: r.bottom + 2,
                  left: Math.max(4, r.left - 80),
                });
              }
              setMenuOpen(!menuOpen);
            }}
          >&#8942;</span>
          {menuOpen && (
            <div
              ref={menuRef}
              className="board-card-menu"
              style={{ top: menuPos.top, left: menuPos.left }}
              onClick={(e) => e.stopPropagation()}
            >
              {todo.status !== "done" && (
                <button onClick={(e) => { e.stopPropagation(); handleAction("complete"); }}>{t("board.markDone")}</button>
              )}
              {todo.status !== "cancelled" && (
                <button onClick={(e) => { e.stopPropagation(); handleAction("cancel"); }}>{t("board.markCancelled")}</button>
              )}
              {(todo.status === "done" || todo.status === "cancelled") && (
                <button onClick={(e) => { e.stopPropagation(); handleAction("reopen"); }}>{t("board.reopen")}</button>
              )}
              <button className="board-card-menu-danger" onClick={(e) => { e.stopPropagation(); handleAction("delete"); }}>{t("board.delete")}</button>
            </div>
          )}
        </div>
        <div className="board-card-body">
          <div className="board-card-title">{todo.title}</div>
          {todo.description && (
            <div className="board-card-desc">{todo.description}</div>
          )}
          {todo.blocked_reason && (
            <div className="board-card-blocked-reason">{todo.blocked_reason}</div>
          )}
        </div>
        <div className="board-card-footer">
          <span className="board-card-assigned">
            {t("board.assignedBy")}: {todo.assigned_by || "—"}
          </span>
          {claimer && (
            <span className="board-card-claimer" style={{ color: claimer.color }}>
              {claimer.icon} {claimer.name}
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

function TaskDetailModal({
  todo,
  kois,
  t,
  onClose,
  onAction,
}: {
  todo: KoiTodo;
  kois: KoiWithStats[];
  t: (key: string) => string;
  onClose: () => void;
  onAction: (action: string, todoId: string) => void;
}) {
  const owner = kois.find((k) => k.id === todo.owner_id);
  const claimer = todo.claimed_by ? kois.find((k) => k.id === todo.claimed_by) : null;
  const priorityColor = PRIORITY_COLORS[todo.priority] ?? PRIORITY_COLORS.low;
  const priorityKey = `board.priority${todo.priority.charAt(0).toUpperCase() + todo.priority.slice(1)}`;
  const colMeta = COLUMNS.find((c) => c.id === todo.status);

  return (
    <div className="board-modal-overlay" onClick={onClose}>
      <div className="board-modal board-detail-modal" onClick={(e) => e.stopPropagation()}>
        <div className="board-detail-header">
          <h3 className="board-modal-title" style={{ margin: 0 }}>{t("board.detail")}</h3>
          <button className="board-detail-close" onClick={onClose}>&#10005;</button>
        </div>

        <div className="board-detail-title">{todo.title}</div>

        <div className="board-detail-meta">
          <div className="board-detail-row">
            <span className="board-detail-label">{t("board.status")}:</span>
            <span>{colMeta?.icon} {colMeta ? t(colMeta.labelKey) : todo.status}</span>
          </div>
          <div className="board-detail-row">
            <span className="board-detail-label">{t("board.filterByPriority")}:</span>
            <span className="board-card-priority" style={{ background: priorityColor }}>{t(priorityKey)}</span>
          </div>
          {owner && (
            <div className="board-detail-row">
              <span className="board-detail-label">{t("board.owner")}:</span>
              <span style={{ color: owner.color }}>{owner.icon} {owner.name}</span>
            </div>
          )}
          {claimer && (
            <div className="board-detail-row">
              <span className="board-detail-label">{t("board.claimer")}:</span>
              <span style={{ color: claimer.color }}>{claimer.icon} {claimer.name}</span>
            </div>
          )}
          <div className="board-detail-row">
            <span className="board-detail-label">{t("board.assignedBy")}:</span>
            <span>{todo.assigned_by || "—"}</span>
          </div>
          {todo.created_at && (
            <div className="board-detail-row">
              <span className="board-detail-label">{t("board.createdAt")}:</span>
              <span>{new Date(todo.created_at).toLocaleString()}</span>
            </div>
          )}
        </div>

        {todo.description && (
          <div className="board-detail-section">
            <div className="board-detail-section-title">{t("board.description")}</div>
            <div className="board-detail-desc">{todo.description}</div>
          </div>
        )}

        {todo.blocked_reason && (
          <div className="board-detail-section">
            <div className="board-detail-section-title">{t("board.blockedReason")}</div>
            <div className="board-detail-blocked">{todo.blocked_reason}</div>
          </div>
        )}

        <div className="board-modal-actions">
          {todo.status !== "done" && (
            <button className="board-btn board-btn-primary" onClick={() => { onAction("complete", todo.id); onClose(); }}>
              {t("board.markDone")}
            </button>
          )}
          {todo.status !== "cancelled" && (
            <button className="board-btn board-btn-secondary" onClick={() => { onAction("cancel", todo.id); onClose(); }}>
              {t("board.markCancelled")}
            </button>
          )}
          {(todo.status === "done" || todo.status === "cancelled") && (
            <button className="board-btn board-btn-secondary" onClick={() => { onAction("reopen", todo.id); onClose(); }}>
              {t("board.reopen")}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

interface CreateFormData {
  title: string;
  description: string;
  owner_id: string;
  priority: string;
}

const EMPTY_FORM: CreateFormData = {
  title: "",
  description: "",
  owner_id: "",
  priority: "medium",
};

function CreateTaskDialog({
  kois,
  saving,
  t,
  onSave,
  onCancel,
}: {
  kois: KoiWithStats[];
  saving: boolean;
  t: (key: string) => string;
  onSave: (data: CreateFormData) => void;
  onCancel: () => void;
}) {
  const [form, setForm] = useState<CreateFormData>({
    ...EMPTY_FORM,
    owner_id: kois[0]?.id ?? "",
  });

  const set = <K extends keyof CreateFormData>(key: K, value: CreateFormData[K]) =>
    setForm((prev) => ({ ...prev, [key]: value }));

  return (
    <div className="board-modal-overlay" onClick={onCancel}>
      <div className="board-modal" onClick={(e) => e.stopPropagation()}>
        <h3 className="board-modal-title">{t("board.createTask")}</h3>

        <div className="board-form-field">
          <label className="board-form-label">{t("board.taskTitle")}</label>
          <input
            className="board-input"
            value={form.title}
            onChange={(e) => set("title", e.target.value)}
            placeholder={t("board.taskTitle")}
            autoFocus
          />
        </div>

        <div className="board-form-field">
          <label className="board-form-label">{t("board.taskDesc")}</label>
          <textarea
            className="board-textarea"
            value={form.description}
            onChange={(e) => set("description", e.target.value)}
            placeholder={t("board.taskDesc")}
            rows={3}
          />
        </div>

        <div className="board-form-field">
          <label className="board-form-label">{t("board.assignTo")}</label>
          <select
            className="board-select"
            value={form.owner_id}
            onChange={(e) => set("owner_id", e.target.value)}
          >
            <option value="" disabled>—</option>
            {kois.map((k) => (
              <option key={k.id} value={k.id}>
                {k.icon} {k.name}
              </option>
            ))}
          </select>
        </div>

        <div className="board-form-field">
          <label className="board-form-label">{t("board.filterByPriority")}</label>
          <div className="board-priority-radios">
            {PRIORITIES.map((p) => {
              const labelKey = `board.priority${p.charAt(0).toUpperCase() + p.slice(1)}`;
              return (
                <label
                  key={p}
                  className={`board-priority-radio ${form.priority === p ? "selected" : ""}`}
                  style={{
                    borderColor: form.priority === p ? PRIORITY_COLORS[p] : undefined,
                    color: form.priority === p ? PRIORITY_COLORS[p] : undefined,
                  }}
                >
                  <input
                    type="radio"
                    name="priority"
                    value={p}
                    checked={form.priority === p}
                    onChange={() => set("priority", p)}
                  />
                  {t(labelKey)}
                </label>
              );
            })}
          </div>
        </div>

        <div className="board-modal-actions">
          <button
            className="board-btn board-btn-secondary"
            onClick={onCancel}
            disabled={saving}
          >
            {t("koi.cancel")}
          </button>
          <button
            className="board-btn board-btn-primary"
            onClick={() => onSave(form)}
            disabled={saving || !form.title.trim() || !form.owner_id}
          >
            {saving ? t("common.creating") : t("board.createTask")}
          </button>
        </div>
      </div>
    </div>
  );
}

export default function Board() {
  const { t } = useTranslation();
  const dispatch = useDispatch();

  const todos = useSelector((s: RootState) => s.board.todos);
  const filterOwnerId = useSelector((s: RootState) => s.board.filterOwnerId);
  const filterPriority = useSelector((s: RootState) => s.board.filterPriority);
  const filterSessionId = useSelector((s: RootState) => s.board.filterSessionId);
  const loading = useSelector((s: RootState) => s.board.loading);
  const kois = useSelector((s: RootState) => s.koi.kois);

  const [showCreate, setShowCreate] = useState(false);
  const [saving, setSaving] = useState(false);
  const [detailTodo, setDetailTodo] = useState<KoiTodo | null>(null);
  const [sessions, setSessions] = useState<PoolSession[]>([]);

  const loadTodos = useCallback(async () => {
    try {
      dispatch(boardActions.setLoading(true));
      const list = await boardApi.listTodos(filterOwnerId ?? undefined);
      dispatch(boardActions.setTodos(list));
    } catch (e) {
    } finally {
      dispatch(boardActions.setLoading(false));
    }
  }, [dispatch, filterOwnerId]);

  useEffect(() => {
    loadTodos();
    if (kois.length === 0) {
      koiApi.list().then((list) => dispatch(koiActions.setKois(list))).catch(() => {});
    }
    poolApi.listSessions().then(setSessions).catch(() => {});
  }, [loadTodos, dispatch, kois.length]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    boardApi.onTodoUpdated(() => { loadTodos(); })
      .then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [loadTodos]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen("koi_status_changed", () => {
      koiApi.list().then((list) => dispatch(koiActions.setKois(list))).catch(() => {});
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [dispatch]);

  const filtered = todos.filter((todo) => {
    if (filterPriority && todo.priority !== filterPriority) return false;
    if (filterSessionId && todo.pool_session_id !== filterSessionId) return false;
    return true;
  });

  const columnTodos = (colId: string): KoiTodo[] =>
    filtered.filter((t) => t.status === colId);

  const handleCardAction = useCallback(async (action: string, todoId: string) => {
    try {
      switch (action) {
        case "complete":
          await boardApi.updateTodo({ id: todoId, status: "done" });
          break;
        case "cancel":
          await boardApi.updateTodo({ id: todoId, status: "cancelled" });
          break;
        case "reopen":
          await boardApi.updateTodo({ id: todoId, status: "todo" });
          break;
        case "delete":
          await boardApi.deleteTodo(todoId);
          break;
      }
      loadTodos();
    } catch (e) {
      console.error("[Board] card action error:", e);
    }
  }, [loadTodos]);

  const handleCreate = async (data: CreateFormData) => {
    try {
      setSaving(true);
      const created = await boardApi.createTodo({
        owner_id: data.owner_id,
        title: data.title,
        description: data.description || undefined,
        priority: data.priority,
        assigned_by: "user",
      });
      dispatch(boardActions.addTodo(created));
      setShowCreate(false);
    } catch (e) {
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="board">
      <div className="board-toolbar">
        <div className="board-filters">
          <select
            className="board-filter-select"
            value={filterOwnerId ?? ""}
            onChange={(e) =>
              dispatch(boardActions.setFilterOwnerId(e.target.value || null))
            }
          >
            <option value="">{t("board.filterByKoi")}: {t("board.filterAll")}</option>
            {kois.map((k) => (
              <option key={k.id} value={k.id}>
                {k.icon} {k.name}
              </option>
            ))}
          </select>

          <select
            className="board-filter-select"
            value={filterPriority ?? ""}
            onChange={(e) =>
              dispatch(boardActions.setFilterPriority(e.target.value || null))
            }
          >
            <option value="">{t("board.filterByPriority")}: {t("board.filterAll")}</option>
            {PRIORITIES.map((p) => {
              const labelKey = `board.priority${p.charAt(0).toUpperCase() + p.slice(1)}`;
              return (
                <option key={p} value={p}>{t(labelKey)}</option>
              );
            })}
          </select>

          <select
            className="board-filter-select"
            value={filterSessionId ?? ""}
            onChange={(e) =>
              dispatch(boardActions.setFilterSessionId(e.target.value || null))
            }
          >
            <option value="">{t("board.filterByProject")}: {t("board.filterAll")}</option>
            {sessions.map((s) => (
              <option key={s.id} value={s.id}>{s.name}</option>
            ))}
          </select>
        </div>

        <button
          className="board-btn board-btn-primary"
          onClick={() => setShowCreate(true)}
        >
          + {t("board.createTask")}
        </button>
      </div>

      {loading && todos.length === 0 ? (
        <div className="board-empty">{t("common.loading")}</div>
      ) : (
        <div className="board-columns">
          {COLUMNS.map((col) => {
            const items = columnTodos(col.id);
            return (
              <div key={col.id} className={`board-column board-column--${col.id}`}>
                <div className="board-column-header">
                  <span className="board-column-icon">{col.icon}</span>
                  <span className="board-column-label">{t(col.labelKey)}</span>
                  <span className="board-column-count">{items.length}</span>
                </div>
                <div className="board-column-body">
                  {items.length === 0 ? (
                    <div className="board-column-empty">{t("board.noTasks")}</div>
                  ) : (
                    items.map((todo) => (
                      <TaskCard key={todo.id} todo={todo} kois={kois} t={t} onAction={handleCardAction} onDetail={setDetailTodo} />
                    ))
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}

      {showCreate && (
        <CreateTaskDialog
          kois={kois}
          saving={saving}
          t={t}
          onSave={handleCreate}
          onCancel={() => setShowCreate(false)}
        />
      )}

      {detailTodo && (
        <TaskDetailModal
          todo={detailTodo}
          kois={kois}
          t={t}
          onClose={() => setDetailTodo(null)}
          onAction={handleCardAction}
        />
      )}
    </div>
  );
}
