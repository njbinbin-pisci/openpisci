import { useEffect, useState } from "react";
import { useDispatch, useSelector } from "react-redux";
import { RootState, memoryActions } from "../../store";
import { memoryApi } from "../../services/tauri";

export default function Memory() {
  const dispatch = useDispatch();
  const { memories, loading } = useSelector((s: RootState) => s.memory);
  const [newContent, setNewContent] = useState("");
  const [newCategory, setNewCategory] = useState("general");
  const [adding, setAdding] = useState(false);
  const [showAdd, setShowAdd] = useState(false);

  useEffect(() => {
    memoryApi.list().then(({ memories }) => {
      dispatch(memoryActions.setMemories(memories));
    });
  }, [dispatch]);

  const handleAdd = async () => {
    if (!newContent.trim()) return;
    setAdding(true);
    try {
      const memory = await memoryApi.add(newContent.trim(), newCategory);
      dispatch(memoryActions.addMemory(memory));
      setNewContent("");
      setShowAdd(false);
    } finally {
      setAdding(false);
    }
  };

  const handleDelete = async (id: string) => {
    await memoryApi.delete(id);
    dispatch(memoryActions.removeMemory(id));
  };

  const handleClear = async () => {
    if (!confirm("Clear all memories? This cannot be undone.")) return;
    await memoryApi.clear();
    dispatch(memoryActions.setMemories([]));
  };

  const categories = [...new Set(memories.map((m) => m.category))];

  return (
    <div className="page">
      <div className="page-header">
        <h1 className="page-title">🧠 Memory</h1>
        <div style={{ display: "flex", gap: 8 }}>
          {memories.length > 0 && (
            <button className="btn btn-danger" onClick={handleClear}>
              Clear All
            </button>
          )}
          <button className="btn btn-primary" onClick={() => setShowAdd(!showAdd)}>
            + Add Memory
          </button>
        </div>
      </div>

      <div className="page-body">
        {showAdd && (
          <div className="card" style={{ marginBottom: 20 }}>
            <div className="form-group">
              <label className="label">Content</label>
              <textarea
                className="input"
                value={newContent}
                onChange={(e) => setNewContent(e.target.value)}
                placeholder="What should Pisci remember?"
                rows={3}
              />
            </div>
            <div className="form-group">
              <label className="label">Category</label>
              <input
                className="input"
                value={newCategory}
                onChange={(e) => setNewCategory(e.target.value)}
                placeholder="e.g. preference, fact, task"
              />
            </div>
            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
              <button className="btn btn-secondary" onClick={() => setShowAdd(false)}>
                Cancel
              </button>
              <button className="btn btn-primary" onClick={handleAdd} disabled={adding}>
                {adding ? "Saving..." : "Save"}
              </button>
            </div>
          </div>
        )}

        {memories.length === 0 ? (
          <div className="empty-state">
            <div className="empty-state-icon">🧠</div>
            <div className="empty-state-title">No memories yet</div>
            <div className="empty-state-desc">
              Pisci will automatically extract memories from your conversations,
              or you can add them manually.
            </div>
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
                        Confidence: {Math.round(m.confidence * 100)}%
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
                    Delete
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
