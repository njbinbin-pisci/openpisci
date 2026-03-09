import { useState, useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { useSelector, useDispatch } from "react-redux";
import { boardApi, koiApi, KoiTodo, KoiWithStats } from "../../../services/tauri";
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

function TaskCard({
  todo,
  kois,
  t,
}: {
  todo: KoiTodo;
  kois: KoiWithStats[];
  t: (key: string) => string;
}) {
  const owner = kois.find((k) => k.id === todo.owner_id);
  const color = owner?.color ?? "#6b7280";
  const icon = owner?.icon ?? "🐟";
  const priorityColor = PRIORITY_COLORS[todo.priority] ?? PRIORITY_COLORS.low;
  const priorityKey = `board.priority${todo.priority.charAt(0).toUpperCase() + todo.priority.slice(1)}`;

  return (
    <div className="board-card">
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
        </div>
        <div className="board-card-title">{todo.title}</div>
        {todo.description && (
          <div className="board-card-desc">{todo.description}</div>
        )}
        <div className="board-card-footer">
          <span className="board-card-assigned">
            {t("board.assignedBy")}: {todo.assigned_by || "—"}
          </span>
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
  const loading = useSelector((s: RootState) => s.board.loading);
  const kois = useSelector((s: RootState) => s.koi.kois);

  const [showCreate, setShowCreate] = useState(false);
  const [saving, setSaving] = useState(false);

  const loadTodos = useCallback(async () => {
    try {
      dispatch(boardActions.setLoading(true));
      const list = await boardApi.listTodos(filterOwnerId ?? undefined);
      dispatch(boardActions.setTodos(list));
    } catch {
      // silently ignore
    } finally {
      dispatch(boardActions.setLoading(false));
    }
  }, [dispatch, filterOwnerId]);

  useEffect(() => {
    loadTodos();
    if (kois.length === 0) {
      koiApi.list().then((list) => dispatch(koiActions.setKois(list))).catch(() => {});
    }
  }, [loadTodos, dispatch, kois.length]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    boardApi.onTodoUpdated(() => { loadTodos(); })
      .then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [loadTodos]);

  const filtered = todos.filter((todo) => {
    if (filterPriority && todo.priority !== filterPriority) return false;
    return true;
  });

  const columnTodos = (colId: string): KoiTodo[] =>
    filtered.filter((t) => t.status === colId);

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
    } catch {
      // silently ignore
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
                      <TaskCard key={todo.id} todo={todo} kois={kois} t={t} />
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
    </div>
  );
}
