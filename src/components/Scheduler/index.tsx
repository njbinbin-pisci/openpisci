import { useEffect, useState } from "react";
import { useDispatch, useSelector } from "react-redux";
import { RootState, schedulerActions } from "../../store";
import { schedulerApi, ScheduledTask } from "../../services/tauri";

export default function Scheduler() {
  const dispatch = useDispatch();
  const { tasks } = useSelector((s: RootState) => s.scheduler);
  const [showForm, setShowForm] = useState(false);
  const [form, setForm] = useState({
    name: "",
    description: "",
    cron_expression: "0 9 * * 1-5",
    task_prompt: "",
  });
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    schedulerApi.list().then(({ tasks }) => {
      dispatch(schedulerActions.setTasks(tasks));
    });
  }, [dispatch]);

  const handleCreate = async () => {
    if (!form.name.trim() || !form.task_prompt.trim()) {
      setError("Name and task prompt are required");
      return;
    }
    setSaving(true);
    setError("");
    try {
      const task = await schedulerApi.create({
        name: form.name,
        description: form.description || undefined,
        cron_expression: form.cron_expression,
        task_prompt: form.task_prompt,
      });
      dispatch(schedulerActions.addTask(task));
      setShowForm(false);
      setForm({ name: "", description: "", cron_expression: "0 9 * * 1-5", task_prompt: "" });
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async (id: string) => {
    if (!confirm("Delete this scheduled task?")) return;
    await schedulerApi.delete(id);
    dispatch(schedulerActions.removeTask(id));
  };

  const handleToggle = async (task: ScheduledTask) => {
    const newStatus = task.status === "active" ? "paused" : "active";
    await schedulerApi.update({ task_id: task.id, status: newStatus });
    dispatch(schedulerActions.setTasks(
      tasks.map((t) => t.id === task.id ? { ...t, status: newStatus } : t)
    ));
  };

  const handleRunNow = async (id: string) => {
    await schedulerApi.runNow(id);
  };

  return (
    <div className="page">
      <div className="page-header">
        <h1 className="page-title">⏰ Scheduler</h1>
        <button className="btn btn-primary" onClick={() => setShowForm(!showForm)}>
          + New Task
        </button>
      </div>

      <div className="page-body">
        {showForm && (
          <div className="card" style={{ marginBottom: 20 }}>
            <h3 style={{ marginBottom: 16, color: "var(--text-primary)" }}>New Scheduled Task</h3>
            {error && (
              <div style={{ padding: "8px 12px", background: "rgba(248,113,113,0.1)", border: "1px solid var(--error)", borderRadius: "var(--radius)", color: "var(--error)", marginBottom: 12, fontSize: 13 }}>
                {error}
              </div>
            )}
            <div className="form-group">
              <label className="label">Task Name *</label>
              <input className="input" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="e.g. Daily Report" />
            </div>
            <div className="form-group">
              <label className="label">Description</label>
              <input className="input" value={form.description} onChange={(e) => setForm({ ...form, description: e.target.value })} placeholder="Optional description" />
            </div>
            <div className="form-group">
              <label className="label">Cron Expression</label>
              <input className="input" value={form.cron_expression} onChange={(e) => setForm({ ...form, cron_expression: e.target.value })} placeholder="min hour day month weekday" style={{ fontFamily: "var(--font-mono)" }} />
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>
                Format: minute hour day month weekday. Example: "0 9 * * 1-5" = 9am weekdays
              </p>
            </div>
            <div className="form-group">
              <label className="label">Task Prompt *</label>
              <textarea className="input" value={form.task_prompt} onChange={(e) => setForm({ ...form, task_prompt: e.target.value })} placeholder="What should Pisci do when this task runs?" rows={4} />
            </div>
            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
              <button className="btn btn-secondary" onClick={() => setShowForm(false)}>Cancel</button>
              <button className="btn btn-primary" onClick={handleCreate} disabled={saving}>
                {saving ? "Creating..." : "Create Task"}
              </button>
            </div>
          </div>
        )}

        {tasks.length === 0 ? (
          <div className="empty-state">
            <div className="empty-state-icon">⏰</div>
            <div className="empty-state-title">No scheduled tasks</div>
            <div className="empty-state-desc">
              Create recurring tasks to automate your workflows.
            </div>
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            {tasks.map((task) => (
              <div key={task.id} className="card">
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", gap: 12 }}>
                  <div style={{ flex: 1 }}>
                    <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6 }}>
                      <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>{task.name}</span>
                      <span className={`badge ${task.status === "active" ? "badge-success" : "badge-warning"}`}>
                        {task.status}
                      </span>
                    </div>
                    {task.description && (
                      <p style={{ fontSize: 13, color: "var(--text-secondary)", marginBottom: 6 }}>{task.description}</p>
                    )}
                    <div style={{ display: "flex", gap: 12, fontSize: 12, color: "var(--text-muted)" }}>
                      <span style={{ fontFamily: "var(--font-mono)" }}>⏱ {task.cron_expression}</span>
                      <span>Runs: {task.run_count}</span>
                      {task.last_run_at && <span>Last: {new Date(task.last_run_at).toLocaleString()}</span>}
                    </div>
                  </div>
                  <div style={{ display: "flex", gap: 6, flexShrink: 0 }}>
                    <button className="btn btn-secondary" style={{ padding: "4px 10px", fontSize: 12 }} onClick={() => handleRunNow(task.id)}>
                      ▶ Run
                    </button>
                    <button className="btn btn-secondary" style={{ padding: "4px 10px", fontSize: 12 }} onClick={() => handleToggle(task)}>
                      {task.status === "active" ? "⏸ Pause" : "▶ Resume"}
                    </button>
                    <button className="btn btn-danger" style={{ padding: "4px 10px", fontSize: 12 }} onClick={() => handleDelete(task.id)}>
                      Delete
                    </button>
                  </div>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
