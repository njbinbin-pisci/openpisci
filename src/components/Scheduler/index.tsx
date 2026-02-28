import { useEffect, useState } from "react";
import { useDispatch, useSelector } from "react-redux";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { RootState, schedulerActions } from "../../store";
import { schedulerApi, ScheduledTask } from "../../services/tauri";

export default function Scheduler() {
  const { t } = useTranslation();
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
    }).catch((e: unknown) => {
      setError(e instanceof Error ? e.message : String(e));
    });
  }, [dispatch]);

  const handleCreate = async () => {
    if (!form.name.trim() || !form.task_prompt.trim()) {
      setError(t("scheduler.nameRequired"));
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
    if (!confirm(t("scheduler.confirmDelete"))) return;
    try {
      await schedulerApi.delete(id);
      dispatch(schedulerActions.removeTask(id));
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const handleToggle = async (task: ScheduledTask) => {
    const newStatus = task.status === "active" ? "paused" : "active";
    try {
      await schedulerApi.update({ task_id: task.id, status: newStatus });
      dispatch(schedulerActions.setTasks(
        tasks.map((t) => t.id === task.id ? { ...t, status: newStatus } : t)
      ));
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const handleRunNow = async (id: string) => {
    try {
      // Optimistically mark as running
      dispatch(schedulerActions.setTasks(
        tasks.map((t) => t.id === id ? { ...t, last_run_status: "running" } : t)
      ));
      await schedulerApi.runNow(id);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  // Listen for real-time status updates from backend
  useEffect(() => {
    const unlisteners = tasks.map((task) =>
      listen<{ status: string }>(`task_status_${task.id}`, (e) => {
        dispatch(schedulerActions.setTasks(
          tasks.map((t) => t.id === task.id ? { ...t, last_run_status: e.payload.status } : t)
        ));
      })
    );
    return () => {
      unlisteners.forEach((p) => p.then((u) => u()));
    };
  }, [tasks.map((t) => t.id).join(","), dispatch]);

  return (
    <div className="page">
      <div className="page-header">
        <h1 className="page-title">⏰ {t("scheduler.title")}</h1>
        <button className="btn btn-primary" onClick={() => setShowForm(!showForm)}>
          {t("scheduler.newTask")}
        </button>
      </div>

      <div className="page-body">
        {showForm && (
          <div className="card" style={{ marginBottom: 20 }}>
            <h3 style={{ marginBottom: 16, color: "var(--text-primary)" }}>{t("scheduler.newTaskTitle")}</h3>
            {error && (
              <div style={{ padding: "8px 12px", background: "rgba(248,113,113,0.1)", border: "1px solid var(--error)", borderRadius: "var(--radius)", color: "var(--error)", marginBottom: 12, fontSize: 13 }}>
                {error}
              </div>
            )}
            <div className="form-group">
              <label className="label">{t("scheduler.taskName")}</label>
              <input className="input" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder={t("scheduler.taskNamePlaceholder")} />
            </div>
            <div className="form-group">
              <label className="label">{t("scheduler.description")}</label>
              <input className="input" value={form.description} onChange={(e) => setForm({ ...form, description: e.target.value })} placeholder={t("scheduler.descriptionPlaceholder")} />
            </div>
            <div className="form-group">
              <label className="label">{t("scheduler.cronExpression")}</label>
              <input className="input" value={form.cron_expression} onChange={(e) => setForm({ ...form, cron_expression: e.target.value })} placeholder={t("scheduler.cronPlaceholder")} style={{ fontFamily: "var(--font-mono)" }} />
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>{t("scheduler.cronHelp")}</p>
            </div>
            <div className="form-group">
              <label className="label">{t("scheduler.taskPrompt")}</label>
              <textarea className="input" value={form.task_prompt} onChange={(e) => setForm({ ...form, task_prompt: e.target.value })} placeholder={t("scheduler.taskPromptPlaceholder")} rows={4} />
            </div>
            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
              <button className="btn btn-secondary" onClick={() => setShowForm(false)}>{t("common.cancel")}</button>
              <button className="btn btn-primary" onClick={handleCreate} disabled={saving}>
                {saving ? t("common.creating") : t("scheduler.newTask").replace("+ ", "")}
              </button>
            </div>
          </div>
        )}

        {tasks.length === 0 ? (
          <div className="empty-state">
            <div className="empty-state-icon">⏰</div>
            <div className="empty-state-title">{t("scheduler.noTasks")}</div>
            <div className="empty-state-desc">{t("scheduler.noTasksDesc")}</div>
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            {tasks.map((task) => (
              <div key={task.id} className="card">
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", gap: 12 }}>
                  <div style={{ flex: 1 }}>
                    <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6, flexWrap: "wrap" }}>
                      <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>{task.name}</span>
                      <span className={`badge ${task.status === "active" ? "badge-success" : "badge-warning"}`}>
                        {task.status === "active" ? t("scheduler.statusActive") : t("scheduler.statusPaused")}
                      </span>
                      {task.last_run_status && (
                        <span className={`badge ${
                          task.last_run_status === "running" ? "badge-info" :
                          task.last_run_status === "success" ? "badge-success" :
                          "badge-danger"
                        }`} style={{ fontSize: 11 }}>
                          {task.last_run_status === "running"
                            ? `⟳ ${t("scheduler.lastRunRunning")}`
                            : task.last_run_status === "success"
                            ? `✓ ${t("scheduler.lastRunSuccess")}`
                            : `✗ ${t("scheduler.lastRunFailed")}`}
                        </span>
                      )}
                    </div>
                    {task.description && (
                      <p style={{ fontSize: 13, color: "var(--text-secondary)", marginBottom: 6 }}>{task.description}</p>
                    )}
                    <div style={{ display: "flex", gap: 12, fontSize: 12, color: "var(--text-muted)" }}>
                      <span style={{ fontFamily: "var(--font-mono)" }}>⏱ {task.cron_expression}</span>
                      <span>{t("scheduler.runs", { count: task.run_count })}</span>
                      {task.last_run_at && <span>{t("scheduler.lastRun", { time: new Date(task.last_run_at).toLocaleString() })}</span>}
                    </div>
                  </div>
                  <div style={{ display: "flex", gap: 6, flexShrink: 0 }}>
                    <button className="btn btn-secondary" style={{ padding: "4px 10px", fontSize: 12 }} onClick={() => handleRunNow(task.id)}>
                      ▶ {t("common.run")}
                    </button>
                    <button className="btn btn-secondary" style={{ padding: "4px 10px", fontSize: 12 }} onClick={() => handleToggle(task)}>
                      {task.status === "active" ? `⏸ ${t("common.pause")}` : `▶ ${t("common.resume")}`}
                    </button>
                    <button className="btn btn-danger" style={{ padding: "4px 10px", fontSize: 12 }} onClick={() => handleDelete(task.id)}>
                      {t("common.delete")}
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
