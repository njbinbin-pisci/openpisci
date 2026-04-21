import { useEffect, useState } from "react";
import { useDispatch, useSelector } from "react-redux";
import { useTranslation } from "react-i18next";
import { RootState, memoryActions } from "../../store";
import { memoryApi } from "../../services/tauri";
import ConfirmDialog from "../ConfirmDialog";

export default function Memory() {
  const { t } = useTranslation();
  const dispatch = useDispatch();
  const { memories } = useSelector((s: RootState) => s.memory);
  const [newContent, setNewContent] = useState("");
  const [newCategory, setNewCategory] = useState("general");
  const [adding, setAdding] = useState(false);
  const [showAdd, setShowAdd] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmClearOpen, setConfirmClearOpen] = useState(false);
  const [clearing, setClearing] = useState(false);

  useEffect(() => {
    memoryApi.list().then(({ memories }) => {
      dispatch(memoryActions.setMemories(memories));
    }).catch((e) => setError(t("memory.failedLoad", { error: String(e) })));
  }, [dispatch, t]);

  const handleAdd = async () => {
    if (!newContent.trim()) return;
    setAdding(true);
    setError(null);
    try {
      const memory = await memoryApi.add(newContent.trim(), newCategory);
      dispatch(memoryActions.addMemory(memory));
      setNewContent("");
      setShowAdd(false);
    } catch (e) {
      setError(t("memory.failedAdd", { error: String(e) }));
    } finally {
      setAdding(false);
    }
  };

  const handleDelete = async (id: string) => {
    try {
      await memoryApi.delete(id);
      dispatch(memoryActions.removeMemory(id));
    } catch (e) {
      setError(t("memory.failedDelete", { error: String(e) }));
    }
  };

  const handleClearConfirmed = async () => {
    setClearing(true);
    try {
      await memoryApi.clear();
      dispatch(memoryActions.setMemories([]));
    } catch (e) {
      setError(t("memory.failedClear", { error: String(e) }));
    } finally {
      setClearing(false);
      setConfirmClearOpen(false);
    }
  };

  return (
    <div className="page">
      <div className="page-header">
        <h1 className="page-title">💡 {t("memory.title")}</h1>
        <div style={{ display: "flex", gap: 8 }}>
          {memories.length > 0 && (
            <button className="btn btn-danger" onClick={() => setConfirmClearOpen(true)}>
              {t("memory.clearAll")}
            </button>
          )}
          <button className="btn btn-primary" onClick={() => setShowAdd(!showAdd)}>
            {t("memory.addMemory")}
          </button>
        </div>
      </div>

      <div className="page-body">
        {error && (
          <div style={{ padding: "8px 14px", background: "rgba(220,53,69,0.15)", borderLeft: "3px solid #dc3545", color: "#ff6b6b", fontSize: "0.85rem", marginBottom: 12, display: "flex", justifyContent: "space-between" }}>
            <span>{error}</span>
            <button onClick={() => setError(null)} style={{ background: "none", border: "none", color: "#ff6b6b", cursor: "pointer" }}>✕</button>
          </div>
        )}
        {showAdd && (
          <div className="card" style={{ marginBottom: 20 }}>
            <div className="form-group">
              <label className="label">{t("memory.content")}</label>
              <textarea
                className="input"
                value={newContent}
                onChange={(e) => setNewContent(e.target.value)}
                placeholder={t("memory.contentPlaceholder")}
                rows={3}
              />
            </div>
            <div className="form-group">
              <label className="label">{t("memory.category")}</label>
              <input
                className="input"
                value={newCategory}
                onChange={(e) => setNewCategory(e.target.value)}
                placeholder={t("memory.categoryPlaceholder")}
              />
            </div>
            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
              <button className="btn btn-secondary" onClick={() => setShowAdd(false)}>
                {t("common.cancel")}
              </button>
              <button className="btn btn-primary" onClick={handleAdd} disabled={adding}>
                {adding ? t("common.saving") : t("common.save")}
              </button>
            </div>
          </div>
        )}

        {memories.length === 0 ? (
          <div className="empty-state">
            <div className="empty-state-icon">💡</div>
            <div className="empty-state-title">{t("memory.noMemories")}</div>
            <div className="empty-state-desc">{t("memory.noMemoriesDesc")}</div>
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            {memories.map((m) => (
              <div key={m.id} className="card memory-item">
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", gap: 12 }}>
                  <div style={{ flex: 1 }}>
                    <p style={{ color: "var(--text-primary)", marginBottom: 8 }}>{m.content}</p>
                    <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
                      <span className="badge badge-info">{m.category}</span>
                      <span style={{ fontSize: 12, color: "var(--text-muted)" }}>
                        {t("memory.confidence", { value: Math.round(m.confidence * 100) })}
                      </span>
                      <span style={{ fontSize: 12, color: "var(--text-muted)" }}>
                        {new Date(m.updated_at).toLocaleDateString()}
                      </span>
                    </div>
                  </div>
                  <button
                    className="btn btn-danger"
                    style={{ padding: "4px 10px", fontSize: 12 }}
                    onClick={() => handleDelete(m.id)}
                  >
                    {t("common.delete")}
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      <ConfirmDialog
        open={confirmClearOpen}
        title={t("memory.clearAll")}
        message={t("memory.confirmClear")}
        confirmLabel={t("memory.clearAll")}
        cancelLabel={t("common.cancel")}
        loading={clearing}
        onConfirm={handleClearConfirmed}
        onCancel={() => !clearing && setConfirmClearOpen(false)}
      />
    </div>
  );
}
