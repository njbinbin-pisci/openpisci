import { useState, useEffect } from "react";
import { useDispatch, useSelector } from "react-redux";
import { RootState, settingsActions } from "../../store";
import { settingsApi, Settings } from "../../services/tauri";

export default function Settings() {
  const dispatch = useDispatch();
  const { settings } = useSelector((s: RootState) => s.settings);
  const [form, setForm] = useState<Partial<Settings>>({});
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [showKeys, setShowKeys] = useState(false);

  useEffect(() => {
    if (settings) {
      setForm({ ...settings });
    }
  }, [settings]);

  const handleSave = async () => {
    setSaving(true);
    try {
      const updated = await settingsApi.save(form);
      dispatch(settingsActions.setSettings(updated));
      dispatch(settingsActions.setConfigured(
        !!(updated.anthropic_api_key || updated.openai_api_key)
      ));
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } finally {
      setSaving(false);
    }
  };

  const update = (key: keyof Settings, value: unknown) => {
    setForm((prev) => ({ ...prev, [key]: value }));
  };

  return (
    <div className="page">
      <div className="page-header">
        <h1 className="page-title">⚙️ Settings</h1>
        <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
          {saved ? "✓ Saved" : saving ? "Saving..." : "Save Changes"}
        </button>
      </div>

      <div className="page-body" style={{ maxWidth: 640 }}>
        {/* AI Provider */}
        <section style={{ marginBottom: 32 }}>
          <h2 style={{ fontSize: 15, fontWeight: 600, color: "var(--text-primary)", marginBottom: 16, paddingBottom: 8, borderBottom: "1px solid var(--border)" }}>
            AI Provider
          </h2>

          <div className="form-group">
            <label className="label">Provider</label>
            <select className="input" value={form.provider ?? "anthropic"} onChange={(e) => update("provider", e.target.value)}>
              <option value="anthropic">Anthropic (Claude)</option>
              <option value="openai">OpenAI (GPT)</option>
              <option value="custom">Custom (OpenAI-compatible)</option>
            </select>
          </div>

          <div className="form-group">
            <label className="label">Model</label>
            <input className="input" value={form.model ?? ""} onChange={(e) => update("model", e.target.value)} placeholder="e.g. claude-sonnet-4-5 or gpt-4o" />
          </div>

          {(form.provider === "anthropic" || !form.provider) && (
            <div className="form-group">
              <label className="label">Anthropic API Key</label>
              <div style={{ position: "relative" }}>
                <input
                  className="input"
                  type={showKeys ? "text" : "password"}
                  value={form.anthropic_api_key ?? ""}
                  onChange={(e) => update("anthropic_api_key", e.target.value)}
                  placeholder="sk-ant-..."
                  style={{ paddingRight: 80 }}
                />
                <button
                  style={{ position: "absolute", right: 8, top: "50%", transform: "translateY(-50%)", background: "none", border: "none", color: "var(--text-muted)", cursor: "pointer", fontSize: 12 }}
                  onClick={() => setShowKeys(!showKeys)}
                >
                  {showKeys ? "Hide" : "Show"}
                </button>
              </div>
            </div>
          )}

          {(form.provider === "openai" || form.provider === "custom") && (
            <>
              <div className="form-group">
                <label className="label">OpenAI API Key</label>
                <input
                  className="input"
                  type={showKeys ? "text" : "password"}
                  value={form.openai_api_key ?? ""}
                  onChange={(e) => update("openai_api_key", e.target.value)}
                  placeholder="sk-..."
                />
              </div>
              {form.provider === "custom" && (
                <div className="form-group">
                  <label className="label">Custom Base URL</label>
                  <input className="input" value={form.custom_base_url ?? ""} onChange={(e) => update("custom_base_url", e.target.value)} placeholder="https://your-api.example.com/v1" />
                </div>
              )}
            </>
          )}

          <div className="form-group">
            <label className="label">Max Tokens per Response</label>
            <input className="input" type="number" value={form.max_tokens ?? 4096} onChange={(e) => update("max_tokens", parseInt(e.target.value))} min={256} max={32768} />
          </div>
        </section>

        {/* Workspace */}
        <section style={{ marginBottom: 32 }}>
          <h2 style={{ fontSize: 15, fontWeight: 600, color: "var(--text-primary)", marginBottom: 16, paddingBottom: 8, borderBottom: "1px solid var(--border)" }}>
            Workspace
          </h2>
          <div className="form-group">
            <label className="label">Workspace Root</label>
            <input className="input" value={form.workspace_root ?? ""} onChange={(e) => update("workspace_root", e.target.value)} placeholder="C:\Users\YourName\Documents\Pisci" />
            <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>
              Files are restricted to this directory for security.
            </p>
          </div>
        </section>

        {/* Security */}
        <section style={{ marginBottom: 32 }}>
          <h2 style={{ fontSize: 15, fontWeight: 600, color: "var(--text-primary)", marginBottom: 16, paddingBottom: 8, borderBottom: "1px solid var(--border)" }}>
            Security
          </h2>
          <div className="form-group" style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
            <div>
              <div style={{ fontWeight: 500, color: "var(--text-primary)" }}>Confirm Shell Commands</div>
              <div style={{ fontSize: 12, color: "var(--text-secondary)" }}>Ask before executing shell commands</div>
            </div>
            <input type="checkbox" checked={form.confirm_shell_commands ?? true} onChange={(e) => update("confirm_shell_commands", e.target.checked)} style={{ width: 18, height: 18 }} />
          </div>
          <div className="form-group" style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
            <div>
              <div style={{ fontWeight: 500, color: "var(--text-primary)" }}>Confirm File Writes</div>
              <div style={{ fontSize: 12, color: "var(--text-secondary)" }}>Ask before writing or modifying files</div>
            </div>
            <input type="checkbox" checked={form.confirm_file_writes ?? true} onChange={(e) => update("confirm_file_writes", e.target.checked)} style={{ width: 18, height: 18 }} />
          </div>
        </section>

        {/* Language */}
        <section>
          <h2 style={{ fontSize: 15, fontWeight: 600, color: "var(--text-primary)", marginBottom: 16, paddingBottom: 8, borderBottom: "1px solid var(--border)" }}>
            Interface
          </h2>
          <div className="form-group">
            <label className="label">Language</label>
            <select className="input" value={form.language ?? "zh"} onChange={(e) => update("language", e.target.value)}>
              <option value="zh">中文</option>
              <option value="en">English</option>
            </select>
          </div>
        </section>
      </div>
    </div>
  );
}
