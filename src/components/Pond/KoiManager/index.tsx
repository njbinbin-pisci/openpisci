import { useState, useEffect, useCallback, useRef } from "react";
import { createPortal } from "react-dom";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { useSelector, useDispatch } from "react-redux";
import { boardApi, koiApi, KoiWithStats, KoiPalette, LlmProviderConfig, Memory, KoiTodo } from "../../../services/tauri";
import { RootState, koiActions } from "../../../store";
import ConfirmDialog from "../../ConfirmDialog";
import "./KoiManager.css";

const STATUS_COLORS: Record<string, string> = {
  idle: "#22c55e",
  busy: "#f59e0b",
  offline: "#6b7280",
};

// ---------------------------------------------------------------------------
// StatTooltip — hover popup for memory / todo details
// ---------------------------------------------------------------------------

type TooltipKind = "memory" | "todo";

interface TooltipState {
  koiId: string;
  kind: TooltipKind;
  anchorRect: DOMRect;
}

const TODO_STATUS_ORDER: Record<string, number> = {
  in_progress: 0, todo: 1, blocked: 2, done: 3, cancelled: 4,
};

function sortTodos(todos: KoiTodo[]): KoiTodo[] {
  return [...todos].sort((a, b) => {
    const oa = TODO_STATUS_ORDER[a.status] ?? 9;
    const ob = TODO_STATUS_ORDER[b.status] ?? 9;
    if (oa !== ob) return oa - ob;
    return a.created_at < b.created_at ? 1 : -1;
  });
}

interface StatTooltipProps extends TooltipState {
  onMouseEnter: () => void;
  onMouseLeave: () => void;
}

function StatTooltip({ koiId, kind, anchorRect, onMouseEnter, onMouseLeave }: StatTooltipProps) {
  const [memories, setMemories] = useState<Memory[] | null>(null);
  const [todos, setTodos] = useState<KoiTodo[] | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    setLoading(true);
    if (kind === "memory") {
      koiApi.listMemories(koiId)
        .then((r) => { setMemories(r.memories); setLoading(false); })
        .catch(() => { setMemories([]); setLoading(false); });
    } else {
      koiApi.listTodos(koiId)
        .then((r) => { setTodos(sortTodos(r)); setLoading(false); })
        .catch(() => { setTodos([]); setLoading(false); });
    }
  }, [koiId, kind]);

  // Position: prefer below the anchor, flip up if not enough space
  const vpH = window.innerHeight;
  const vpW = window.innerWidth;
  const tooltipW = 280;
  const tooltipMaxH = 240;
  const gap = 6;

  let top = anchorRect.bottom + gap;
  if (top + tooltipMaxH > vpH - 8) {
    top = anchorRect.top - tooltipMaxH - gap;
  }
  let left = anchorRect.left;
  if (left + tooltipW > vpW - 8) {
    left = vpW - tooltipW - 8;
  }

  const statusLabel: Record<string, string> = {
    todo: "待办", in_progress: "进行中", done: "已完成",
    cancelled: "已取消", blocked: "阻塞",
  };

  return createPortal(
    <div
      className="koi-stat-tooltip"
      style={{ top, left, width: tooltipW }}
      onMouseEnter={onMouseEnter}
      onMouseLeave={onMouseLeave}
    >
      <div className="koi-stat-tooltip-title">
        {kind === "memory" ? "📚 记忆详情" : "📋 待办详情"}
      </div>
      <div className="koi-stat-tooltip-body">
        {loading && <div className="koi-stat-tooltip-empty">加载中…</div>}
        {!loading && kind === "memory" && (
          memories && memories.length > 0 ? memories.map((m) => (
            <div key={m.id} className="koi-stat-tooltip-item">
              <span className="koi-stat-tooltip-tag">{m.category}</span>
              <span className="koi-stat-tooltip-text">{m.content}</span>
            </div>
          )) : <div className="koi-stat-tooltip-empty">暂无记忆</div>
        )}
        {!loading && kind === "todo" && (
          todos && todos.length > 0 ? todos.map((td) => (
            <div key={td.id} className="koi-stat-tooltip-item">
              <span className={`koi-stat-tooltip-tag koi-stat-tooltip-tag--${td.status}`}>
                {statusLabel[td.status] ?? td.status}
              </span>
              <span className="koi-stat-tooltip-text">{td.title}</span>
            </div>
          )) : <div className="koi-stat-tooltip-empty">暂无待办</div>
        )}
      </div>
    </div>,
    document.body,
  );
}

interface KoiFormData {
  name: string;
  role: string;
  icon: string;
  color: string;
  description: string;
  system_prompt: string;
  /** Empty string = use global default */
  llm_provider_id: string;
}

const EMPTY_FORM: KoiFormData = {
  name: "",
  role: "",
  icon: "🐟",
  color: "#7c3aed",
  description: "",
  system_prompt: "",
  llm_provider_id: "",
};

function KoiCard({
  koi,
  t,
  onEdit,
  onDelete,
  onToggleActive,
}: {
  koi: KoiWithStats;
  t: (key: string) => string;
  onEdit: () => void;
  onDelete: () => void;
  onToggleActive: () => void;
}) {
  const hasActiveTodos = koi.active_todo_count > 0;
  const displayStatus = koi.status === "busy" ? "busy"
    : hasActiveTodos ? "has_tasks"
    : koi.status === "idle" ? "idle"
    : "offline";
  const statusKey = displayStatus === "busy" ? "koi.statusBusy"
    : displayStatus === "has_tasks" ? "koi.statusHasTasks"
    : displayStatus === "idle" ? "koi.statusIdle"
    : "koi.statusVacation";
  const dotColor = displayStatus === "busy" ? "#f59e0b"
    : displayStatus === "has_tasks" ? "#3b82f6"
    : displayStatus === "idle" ? "#22c55e"
    : "#6b7280";

  const [tooltip, setTooltip] = useState<TooltipState | null>(null);
  const openTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const closeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const cancelClose = () => {
    if (closeTimerRef.current) {
      clearTimeout(closeTimerRef.current);
      closeTimerRef.current = null;
    }
  };

  const scheduleClose = () => {
    cancelClose();
    closeTimerRef.current = setTimeout(() => setTooltip(null), 150);
  };

  const handleStatEnter = (e: React.MouseEvent, kind: TooltipKind) => {
    cancelClose();
    if (openTimerRef.current) clearTimeout(openTimerRef.current);
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openTimerRef.current = setTimeout(() => {
      setTooltip({ koiId: koi.id, kind, anchorRect: rect });
    }, 300);
  };

  const handleStatLeave = () => {
    if (openTimerRef.current) clearTimeout(openTimerRef.current);
    scheduleClose();
  };

  return (
    <div className="koi-card" style={{ borderLeftColor: koi.color }}>
      <div className="koi-card-header">
        <span className="koi-card-icon">{koi.icon}</span>
        <div className="koi-card-info">
          <span className="koi-card-name">{koi.name}</span>
          {koi.role && <span className="koi-card-role">{koi.role}</span>}
          <span className="koi-card-status">
            <span
              className="koi-status-dot"
              style={{ background: dotColor }}
            />
            {t(statusKey)}
          </span>
        </div>
      </div>
      {koi.description && (
        <p className="koi-card-desc">{koi.description}</p>
      )}
      <div className="koi-card-stats">
        <span
          className="koi-stat koi-stat--hoverable"
          onMouseEnter={(e) => handleStatEnter(e, "memory")}
          onMouseLeave={handleStatLeave}
        >
          <span className="koi-stat-icon koi-stat-icon--memory" />
          {t("koi.memoryCount")} {koi.memory_count}
        </span>
        <span
          className="koi-stat koi-stat--hoverable"
          onMouseEnter={(e) => handleStatEnter(e, "todo")}
          onMouseLeave={handleStatLeave}
        >
          <span className="koi-stat-icon koi-stat-icon--todo" />
          {t("koi.todoCount")} {koi.active_todo_count}
        </span>
      </div>
      <div className="koi-card-actions">
        <button
          className={`koi-btn ${koi.status === "offline" ? "koi-btn-primary" : "koi-btn-secondary"}`}
          onClick={onToggleActive}
        >
          {koi.status === "offline" ? t("koi.returnFromVacation") : t("koi.deactivate")}
        </button>
        <button className="koi-btn koi-btn-secondary" onClick={onEdit}>
          {t("koi.editBtn")}
        </button>
        <button className="koi-btn koi-btn-danger" onClick={onDelete}>
          {t("koi.fire")}
        </button>
      </div>
      {tooltip && (
        <StatTooltip
          {...tooltip}
          onMouseEnter={cancelClose}
          onMouseLeave={scheduleClose}
        />
      )}
    </div>
  );
}

function KoiDialog({
  mode,
  initial,
  originalKoi,
  palette,
  llmProviders,
  saving,
  t,
  onSave,
  onCancel,
}: {
  mode: "create" | "edit";
  initial: KoiFormData;
  originalKoi?: KoiWithStats | null;
  palette: KoiPalette | null;
  llmProviders: LlmProviderConfig[];
  saving: boolean;
  t: (key: string) => string;
  onSave: (data: KoiFormData) => void;
  onCancel: () => void;
}) {
  const [form, setForm] = useState<KoiFormData>(initial);
  const [customIcon, setCustomIcon] = useState("");

  const nameChanged = mode === "edit" && originalKoi && form.name !== originalKoi.name;
  const promptChanged = mode === "edit" && originalKoi && form.system_prompt !== originalKoi.system_prompt;

  const icons = palette?.icons ?? [
    "🐟", "🐠", "🐡", "🦈", "🐋", "🐳", "🦑", "🐙",
    "🦐", "🦀", "🐢", "🦭", "🐬", "🦞", "🐚", "🪸",
  ];
  const colors = palette?.colors ?? [
    ["#7c3aed", "Violet"],
    ["#2563eb", "Blue"],
    ["#0891b2", "Cyan"],
    ["#059669", "Green"],
    ["#d97706", "Amber"],
    ["#dc2626", "Red"],
    ["#db2777", "Pink"],
    ["#7c3aed", "Purple"],
    ["#4b5563", "Gray"],
    ["#0f172a", "Dark"],
  ];

  const set = (key: keyof KoiFormData, value: string) =>
    setForm((prev) => ({ ...prev, [key]: value }));

  return (
    <div className="koi-modal-overlay" onClick={onCancel}>
      <div className="koi-modal" onClick={(e) => e.stopPropagation()}>
        <h3 className="koi-modal-title">
          {mode === "create" ? t("koi.createTitle") : t("koi.editTitle")}
        </h3>

        <div className="koi-form-field">
          <label className="koi-form-label">{t("koi.name")}</label>
          <input
            className="koi-input"
            value={form.name}
            onChange={(e) => set("name", e.target.value)}
            placeholder={t("koi.namePlaceholder")}
            autoFocus
          />
          {nameChanged && originalKoi && (
            <p className="koi-form-help koi-form-warn">
              {t("koi.editRenameWarning").replace("{{oldName}}", originalKoi.name)}
            </p>
          )}
        </div>

        <div className="koi-form-field">
          <label className="koi-form-label">{t("koi.role")}</label>
          <input
            className="koi-input"
            value={form.role}
            onChange={(e) => set("role", e.target.value)}
            placeholder={t("koi.rolePlaceholder")}
          />
        </div>

        <div className="koi-form-field">
          <label className="koi-form-label">{t("koi.icon")}</label>
          <div className="koi-icon-picker">
            {icons.map((ic) => (
              <button
                key={ic}
                className={`koi-icon-option ${form.icon === ic ? "selected" : ""}`}
                onClick={() => set("icon", ic)}
              >
                {ic}
              </button>
            ))}
            <div className="koi-icon-custom">
              <input
                className="koi-input koi-icon-custom-input"
                value={customIcon}
                onChange={(e) => setCustomIcon(e.target.value)}
                placeholder={t("koi.customIcon")}
                maxLength={2}
              />
              {customIcon && (
                <button
                  className="koi-icon-option"
                  onClick={() => {
                    set("icon", customIcon);
                    setCustomIcon("");
                  }}
                >
                  ✓
                </button>
              )}
            </div>
          </div>
        </div>

        <div className="koi-form-field">
          <label className="koi-form-label">{t("koi.color")}</label>
          <div className="koi-color-picker">
            {colors.map(([hex, label]) => (
              <button
                key={hex + label}
                className={`koi-color-option ${form.color === hex ? "selected" : ""}`}
                style={{ background: hex }}
                onClick={() => set("color", hex)}
                title={label}
              />
            ))}
          </div>
        </div>

        <div className="koi-form-field">
          <label className="koi-form-label">{t("koi.description")}</label>
          <textarea
            className="koi-textarea"
            value={form.description}
            onChange={(e) => set("description", e.target.value)}
            placeholder={t("koi.descPlaceholder")}
            rows={2}
          />
        </div>

        <div className="koi-form-field">
          <label className="koi-form-label">{t("koi.systemPrompt")}</label>
          <textarea
            className="koi-textarea koi-textarea-lg"
            value={form.system_prompt}
            onChange={(e) => set("system_prompt", e.target.value)}
            placeholder={t("koi.systemPromptPlaceholder")}
            rows={5}
          />
          <p className="koi-form-help">{t("koi.systemPromptHelp")}</p>
          {promptChanged && (
            <p className="koi-form-help koi-form-warn">{t("koi.editPromptWarning")}</p>
          )}
        </div>

        <div className="koi-form-field">
          <label className="koi-form-label">🤖 LLM 供应商</label>
          <select
            className="koi-input"
            value={form.llm_provider_id}
            onChange={(e) => set("llm_provider_id", e.target.value)}
          >
            <option value="">全局默认（继承系统设置）</option>
            {llmProviders.map((p) => (
              <option key={p.id} value={p.id}>
                {p.label || p.id} — {p.provider} / {p.model}
              </option>
            ))}
          </select>
          {llmProviders.length === 0 && (
            <p className="koi-form-help" style={{ color: "var(--text-muted)" }}>
              在"设置 → LLM 供应商管理"中添加命名供应商后，可在此为每个 Koi 单独选择。
            </p>
          )}
        </div>

        <div className="koi-modal-actions">
          <button
            className="koi-btn koi-btn-secondary"
            onClick={onCancel}
            disabled={saving}
          >
            {t("koi.cancel")}
          </button>
          <button
            className="koi-btn koi-btn-primary"
            onClick={() => onSave(form)}
            disabled={saving || !form.name.trim()}
          >
            {saving
              ? t("common.creating")
              : mode === "create"
                ? t("koi.create")
                : t("koi.save")}
          </button>
        </div>
      </div>
    </div>
  );
}

export default function KoiManager() {
  const { t } = useTranslation();
  const dispatch = useDispatch();
  const kois = useSelector((s: RootState) => s.koi.kois);
  const loading = useSelector((s: RootState) => s.koi.loading);
  const settings = useSelector((s: RootState) => s.settings.settings);

  const [palette, setPalette] = useState<KoiPalette | null>(null);
  const [llmProviders, setLlmProviders] = useState<LlmProviderConfig[]>([]);
  const [dialogMode, setDialogMode] = useState<"create" | "edit" | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editingKoi, setEditingKoi] = useState<KoiWithStats | null>(null);
  const [dialogInit, setDialogInit] = useState<KoiFormData>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<KoiWithStats | null>(null);
  const [deleteInfo, setDeleteInfo] = useState<{ name: string; icon: string; todo_count: number; memory_count: number; is_busy: boolean } | null>(null);
  const [deleting, setDeleting] = useState(false);
  // Busy confirmation for vacation
  const [vacationBusyTarget, setVacationBusyTarget] = useState<KoiWithStats | null>(null);

  const loadKois = useCallback(async () => {
    try {
      dispatch(koiActions.setLoading(true));
      const list = await koiApi.list();
      dispatch(koiActions.setKois(list));
    } catch (e) {
      setError(String(e));
    } finally {
      dispatch(koiActions.setLoading(false));
    }
  }, [dispatch]);

  useEffect(() => {
    loadKois();
    koiApi.palette().then(setPalette).catch(() => {});
  }, [loadKois]);

  useEffect(() => {
    setLlmProviders(settings?.llm_providers ?? []);
  }, [settings]);

  // Keep Koi stats in sync with the board/chat by reloading whenever todo or status
  // events fire. Otherwise the management panel can show stale todo counts.
  useEffect(() => {
    let unlistenTodo: (() => void) | null = null;
    let unlistenStatus: (() => void) | null = null;

    boardApi.onTodoUpdated(() => {
      loadKois();
    }).then((fn) => {
      unlistenTodo = fn;
    });

    listen("koi_status_changed", () => {
      loadKois();
    }).then((fn) => {
      unlistenStatus = fn;
    });

    return () => {
      unlistenTodo?.();
      unlistenStatus?.();
    };
  }, [loadKois]);

  const openCreate = () => {
    setDialogInit(EMPTY_FORM);
    setEditingId(null);
    setDialogMode("create");
  };

  const openEdit = (koi: KoiWithStats) => {
    setDialogInit({
      name: koi.name,
      role: koi.role,
      icon: koi.icon,
      color: koi.color,
      description: koi.description,
      system_prompt: koi.system_prompt,
      llm_provider_id: koi.llm_provider_id ?? "",
    });
    setEditingId(koi.id);
    setEditingKoi(koi);
    setDialogMode("edit");
  };

  const handleSave = async (data: KoiFormData) => {
    try {
      setSaving(true);
      setError(null);
      // Normalize: empty string → undefined for create, keep as-is for update (empty = clear)
      const providerIdForCreate = data.llm_provider_id || undefined;
      if (dialogMode === "create") {
        const created = await koiApi.create({ ...data, llm_provider_id: providerIdForCreate });
        dispatch(koiActions.addKoi({
          ...created,
          memory_count: 0,
          todo_count: 0,
          active_todo_count: 0,
        }));
      } else if (editingId) {
        await koiApi.update({ id: editingId, ...data });
        dispatch(koiActions.updateKoiInList({ id: editingId, ...data }));
      }
      setDialogMode(null);
    } catch (e) {
      const msg = String(e);
      setError(msg.includes("数量上限") || msg.includes("limit reached")
        ? t("koi.maxKoisReached")
        : msg);
    } finally {
      setSaving(false);
    }
  };

  const handleDeleteRequest = async (koi: KoiWithStats) => {
    try {
      const info = await koiApi.getDeleteInfo(koi.id);
      setDeleteInfo(info);
      setDeleteTarget(koi);
    } catch (e) {
      setError(String(e));
    }
  };

  const handleDeleteConfirm = async () => {
    if (!deleteTarget) return;
    try {
      setDeleting(true);
      await koiApi.delete(deleteTarget.id);
      dispatch(koiActions.removeKoi(deleteTarget.id));
      setDeleteTarget(null);
      setDeleteInfo(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setDeleting(false);
    }
  };

  const handleToggleActive = async (koi: KoiWithStats) => {
    try {
      await koiApi.setActive(koi.id, koi.status === "offline");
      loadKois();
    } catch (e) {
      const msg = String(e);
      if (msg.includes("BUSY:")) {
        // Backend returned BUSY sentinel — show confirmation
        setVacationBusyTarget(koi);
      } else {
        setError(msg);
      }
    }
  };

  const handleForceVacation = async () => {
    if (!vacationBusyTarget) return;
    try {
      await koiApi.setActive(vacationBusyTarget.id, false, true);
      setVacationBusyTarget(null);
      loadKois();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="koi-manager">
      <div className="koi-manager-header">
        <h3 className="koi-manager-title">{t("koi.title")}</h3>
        <button className="koi-btn koi-btn-primary" onClick={openCreate}>
          + {t("koi.createBtn")}
        </button>
      </div>

      {error && (
        <div className="koi-error">
          <span>{error}</span>
          <button onClick={() => setError(null)}>✕</button>
        </div>
      )}

      {loading ? (
        <div className="koi-empty">{t("common.loading")}</div>
      ) : kois.length === 0 ? (
        <div className="koi-empty">
          <span className="koi-empty-icon">🐟</span>
          <p>{t("common.noData")}</p>
        </div>
      ) : (
        <div className="koi-grid">
          {kois.map((koi) => (
            <KoiCard
              key={koi.id}
              koi={koi}
              t={t}
              onEdit={() => openEdit(koi)}
              onDelete={() => handleDeleteRequest(koi)}
              onToggleActive={() => handleToggleActive(koi)}
            />
          ))}
        </div>
      )}

      {dialogMode && (
        <KoiDialog
          mode={dialogMode}
          initial={dialogInit}
          originalKoi={editingKoi}
          palette={palette}
          llmProviders={llmProviders}
          saving={saving}
          t={t}
          onSave={handleSave}
          onCancel={() => { setDialogMode(null); setEditingKoi(null); }}
        />
      )}

      <ConfirmDialog
        open={!!deleteTarget}
        title={t("koi.confirmFireTitle")}
        message={
          deleteInfo
            ? [
                t("koi.confirmFireMessage")
                  .replace("{{icon}}", deleteInfo.icon)
                  .replace("{{name}}", deleteInfo.name),
                deleteInfo.is_busy
                  ? t("koi.confirmFireBusyWarning").replace("{{name}}", deleteInfo.name)
                  : "",
                deleteInfo.todo_count > 0
                  ? t("koi.confirmFireTodosWarning")
                      .replace("{{name}}", deleteInfo.name)
                      .replace("{{count}}", String(deleteInfo.todo_count))
                  : "",
                deleteInfo.memory_count > 0
                  ? t("koi.confirmFireMemoryWarning")
                      .replace("{{name}}", deleteInfo.name)
                      .replace("{{count}}", String(deleteInfo.memory_count))
                  : "",
              ]
                .filter(Boolean)
                .join("\n")
            : t("koi.confirmDelete")
        }
        confirmLabel={t("koi.fire")}
        cancelLabel={t("common.cancel")}
        variant="danger"
        loading={deleting}
        onConfirm={handleDeleteConfirm}
        onCancel={() => { setDeleteTarget(null); setDeleteInfo(null); }}
      />

      <ConfirmDialog
        open={!!vacationBusyTarget}
        title={t("koi.confirmVacationTitle")}
        message={
          vacationBusyTarget
            ? [
                t("koi.confirmVacationBusyWarning").replace("{{name}}", vacationBusyTarget.name),
                vacationBusyTarget.active_todo_count > 0
                  ? t("koi.confirmVacationTodosWarning")
                      .replace("{{name}}", vacationBusyTarget.name)
                      .replace("{{count}}", String(vacationBusyTarget.active_todo_count))
                  : "",
              ]
                .filter(Boolean)
                .join("\n")
            : ""
        }
        confirmLabel={t("koi.deactivate")}
        cancelLabel={t("common.cancel")}
        variant="danger"
        loading={false}
        onConfirm={handleForceVacation}
        onCancel={() => setVacationBusyTarget(null)}
      />
    </div>
  );
}
