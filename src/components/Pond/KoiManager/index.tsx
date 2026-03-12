import { useState, useEffect, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { useSelector, useDispatch } from "react-redux";
import { boardApi, koiApi, KoiWithStats, KoiPalette } from "../../../services/tauri";
import { RootState, koiActions } from "../../../store";
import ConfirmDialog from "../../ConfirmDialog";
import "./KoiManager.css";

const STATUS_COLORS: Record<string, string> = {
  idle: "#22c55e",
  busy: "#f59e0b",
  offline: "#6b7280",
};

interface KoiFormData {
  name: string;
  role: string;
  icon: string;
  color: string;
  description: string;
  system_prompt: string;
}

const EMPTY_FORM: KoiFormData = {
  name: "",
  role: "",
  icon: "🐟",
  color: "#7c3aed",
  description: "",
  system_prompt: "",
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
        <span className="koi-stat">
          <span className="koi-stat-icon koi-stat-icon--memory" />
          {t("koi.memoryCount")} {koi.memory_count}
        </span>
        <span className="koi-stat">
          <span className="koi-stat-icon koi-stat-icon--todo" />
          {t("koi.todoCount")} {koi.active_todo_count}
        </span>
      </div>
      <div className="koi-card-actions">
        <button
          className={`koi-btn ${koi.status === "offline" ? "koi-btn-primary" : "koi-btn-secondary"}`}
          onClick={onToggleActive}
        >
          {koi.status === "offline" ? t("koi.activate") : t("koi.deactivate")}
        </button>
        <button className="koi-btn koi-btn-secondary" onClick={onEdit}>
          {t("koi.editBtn")}
        </button>
        <button className="koi-btn koi-btn-danger" onClick={onDelete}>
          {t("koi.deleteBtn")}
        </button>
      </div>
    </div>
  );
}

function KoiDialog({
  mode,
  initial,
  palette,
  saving,
  t,
  onSave,
  onCancel,
}: {
  mode: "create" | "edit";
  initial: KoiFormData;
  palette: KoiPalette | null;
  saving: boolean;
  t: (key: string) => string;
  onSave: (data: KoiFormData) => void;
  onCancel: () => void;
}) {
  const [form, setForm] = useState<KoiFormData>(initial);
  const [customIcon, setCustomIcon] = useState("");

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

  const [palette, setPalette] = useState<KoiPalette | null>(null);
  const [dialogMode, setDialogMode] = useState<"create" | "edit" | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [dialogInit, setDialogInit] = useState<KoiFormData>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<KoiWithStats | null>(null);
  const [deleting, setDeleting] = useState(false);

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
    });
    setEditingId(koi.id);
    setDialogMode("edit");
  };

  const handleSave = async (data: KoiFormData) => {
    try {
      setSaving(true);
      setError(null);
      if (dialogMode === "create") {
        const created = await koiApi.create(data);
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

  const handleDeleteConfirm = async () => {
    if (!deleteTarget) return;
    try {
      setDeleting(true);
      await koiApi.delete(deleteTarget.id);
      dispatch(koiActions.removeKoi(deleteTarget.id));
      setDeleteTarget(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setDeleting(false);
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
              onDelete={() => setDeleteTarget(koi)}
              onToggleActive={async () => {
                try {
                  await koiApi.setActive(koi.id, koi.status === "offline");
                  loadKois();
                } catch (e) {
                  console.error("toggle active error:", e);
                }
              }}
            />
          ))}
        </div>
      )}

      {dialogMode && (
        <KoiDialog
          mode={dialogMode}
          initial={dialogInit}
          palette={palette}
          saving={saving}
          t={t}
          onSave={handleSave}
          onCancel={() => setDialogMode(null)}
        />
      )}

      <ConfirmDialog
        open={!!deleteTarget}
        title={t("koi.deleteBtn")}
        message={t("koi.confirmDelete")}
        confirmLabel={t("common.delete")}
        cancelLabel={t("common.cancel")}
        variant="danger"
        loading={deleting}
        onConfirm={handleDeleteConfirm}
        onCancel={() => setDeleteTarget(null)}
      />
    </div>
  );
}
