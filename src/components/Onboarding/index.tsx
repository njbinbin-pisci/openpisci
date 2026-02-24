import { useState } from "react";
import { useDispatch } from "react-redux";
import { settingsActions } from "../../store";
import { settingsApi } from "../../services/tauri";

interface Props {
  onComplete: () => void;
}

type Step = "welcome" | "provider" | "workspace" | "done";

export default function Onboarding({ onComplete }: Props) {
  const dispatch = useDispatch();
  const [step, setStep] = useState<Step>("welcome");
  const [provider, setProvider] = useState("anthropic");
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState("claude-sonnet-4-5");
  const [workspace, setWorkspace] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  const handleSave = async () => {
    if (!apiKey.trim()) {
      setError("API key is required");
      return;
    }
    setSaving(true);
    setError("");
    try {
      const updates: Record<string, unknown> = {
        provider,
        model,
      };
      if (provider === "anthropic") {
        updates.anthropic_api_key = apiKey;
      } else {
        updates.openai_api_key = apiKey;
      }
      if (workspace.trim()) {
        updates.workspace_root = workspace;
      }

      const settings = await settingsApi.save(updates);
      dispatch(settingsActions.setSettings(settings));
      dispatch(settingsActions.setConfigured(true));
      setStep("done");
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div style={{
      display: "flex",
      alignItems: "center",
      justifyContent: "center",
      height: "100vh",
      background: "var(--bg-primary)",
      padding: 24,
    }}>
      <div style={{ maxWidth: 480, width: "100%" }}>
        {step === "welcome" && (
          <div style={{ textAlign: "center" }}>
            <div style={{ fontSize: 64, marginBottom: 16 }}>🐟</div>
            <h1 style={{ fontSize: 28, fontWeight: 700, color: "var(--text-primary)", marginBottom: 12 }}>
              Welcome to Pisci
            </h1>
            <p style={{ color: "var(--text-secondary)", marginBottom: 32, lineHeight: 1.7 }}>
              Your AI-powered desktop assistant for Windows.
              Pisci can help you with files, shell commands, web search,
              and Windows UI automation — all from a single chat interface.
            </p>
            <button className="btn btn-primary" style={{ padding: "12px 32px", fontSize: 16 }} onClick={() => setStep("provider")}>
              Get Started →
            </button>
          </div>
        )}

        {step === "provider" && (
          <div>
            <h2 style={{ fontSize: 22, fontWeight: 600, color: "var(--text-primary)", marginBottom: 8 }}>
              Configure AI Provider
            </h2>
            <p style={{ color: "var(--text-secondary)", marginBottom: 24, fontSize: 14 }}>
              Pisci needs an AI API key to work. Your key is stored locally and never sent anywhere except the AI provider.
            </p>

            {error && (
              <div style={{ padding: "10px 14px", background: "rgba(248,113,113,0.1)", border: "1px solid var(--error)", borderRadius: "var(--radius)", color: "var(--error)", marginBottom: 16, fontSize: 13 }}>
                {error}
              </div>
            )}

            <div className="form-group">
              <label className="label">AI Provider</label>
              <select className="input" value={provider} onChange={(e) => {
                setProvider(e.target.value);
                setModel(e.target.value === "anthropic" ? "claude-sonnet-4-5" : "gpt-4o");
              }}>
                <option value="anthropic">Anthropic (Claude) — Recommended</option>
                <option value="openai">OpenAI (GPT)</option>
              </select>
            </div>

            <div className="form-group">
              <label className="label">API Key *</label>
              <input
                className="input"
                type="password"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder={provider === "anthropic" ? "sk-ant-..." : "sk-..."}
                autoFocus
              />
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>
                {provider === "anthropic"
                  ? "Get your key at console.anthropic.com"
                  : "Get your key at platform.openai.com"}
              </p>
            </div>

            <div className="form-group">
              <label className="label">Model</label>
              <input className="input" value={model} onChange={(e) => setModel(e.target.value)} />
            </div>

            <div className="form-group">
              <label className="label">Workspace Directory (optional)</label>
              <input
                className="input"
                value={workspace}
                onChange={(e) => setWorkspace(e.target.value)}
                placeholder="C:\Users\YourName\Documents\Pisci"
              />
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>
                Files created by Pisci will be saved here. Leave blank to use default.
              </p>
            </div>

            <div style={{ display: "flex", gap: 8, justifyContent: "space-between", marginTop: 24 }}>
              <button className="btn btn-secondary" onClick={() => setStep("welcome")}>← Back</button>
              <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                {saving ? "Saving..." : "Save & Continue →"}
              </button>
            </div>
          </div>
        )}

        {step === "done" && (
          <div style={{ textAlign: "center" }}>
            <div style={{ fontSize: 64, marginBottom: 16 }}>✅</div>
            <h2 style={{ fontSize: 22, fontWeight: 600, color: "var(--text-primary)", marginBottom: 12 }}>
              You're all set!
            </h2>
            <p style={{ color: "var(--text-secondary)", marginBottom: 32 }}>
              Pisci is ready to help. Start a new chat to begin.
            </p>
            <button className="btn btn-primary" style={{ padding: "12px 32px", fontSize: 16 }} onClick={onComplete}>
              Start Chatting 🐟
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
