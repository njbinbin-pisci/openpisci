import { useState } from "react";
import { useDispatch } from "react-redux";
import { useTranslation } from "react-i18next";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { settingsActions } from "../../store";
import { settingsApi } from "../../services/tauri";
import { setLanguage } from "../../i18n";

interface Props {
  onComplete: () => void;
}

type Step = "welcome" | "provider" | "policy" | "done";

export default function Onboarding({ onComplete }: Props) {
  const { t, i18n } = useTranslation();
  const dispatch = useDispatch();
  const [step, setStep] = useState<Step>("welcome");
  const [provider, setProvider] = useState("anthropic");
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState("claude-sonnet-4-5");
  const [workspace, setWorkspace] = useState("");
  const [policyMode, setPolicyMode] = useState("balanced");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  const browseWorkspace = async () => {
    try {
      const selected = await openDialog({ directory: true, multiple: false, title: t("onboarding.workspaceBrowseTitle") });
      if (selected && typeof selected === "string") {
        setWorkspace(selected);
      }
    } catch {
      // dialog cancelled or not available
    }
  };

  const currentLang = i18n.language === "en" ? "en" : "zh";
  const toggleLang = () => {
    const next = currentLang === "zh" ? "en" : "zh";
    setLanguage(next);
  };

  const handleSave = async () => {
    if (!apiKey.trim()) {
      setError(t("onboarding.apiKeyRequired"));
      return;
    }
    setSaving(true);
    setError("");
    try {
      const updates: Record<string, unknown> = { provider, model, policy_mode: policyMode };
      if (provider === "anthropic") updates.anthropic_api_key = apiKey;
      else if (provider === "openai") updates.openai_api_key = apiKey;
      else if (provider === "deepseek") updates.deepseek_api_key = apiKey;
      else if (provider === "qwen") updates.qwen_api_key = apiKey;
      else updates.openai_api_key = apiKey;
      if (workspace.trim()) updates.workspace_root = workspace;

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

  const getKeyHelp = () => {
    if (provider === "anthropic") return t("onboarding.anthropicKeyHelp");
    if (provider === "openai") return t("onboarding.openaiKeyHelp");
    if (provider === "deepseek") return "platform.deepseek.com";
    if (provider === "qwen") return "dashscope.aliyuncs.com";
    return "";
  };

  const getDefaultModel = (p: string) => {
    if (p === "anthropic") return "claude-sonnet-4-5";
    if (p === "openai") return "gpt-4o";
    if (p === "deepseek") return "deepseek-chat";
    if (p === "qwen") return "qwen-max";
    return "";
  };

  return (
    <div style={{ display: "flex", alignItems: "center", justifyContent: "center", height: "100vh", background: "var(--bg-primary)", padding: 24, position: "relative" }}>
      {/* Language toggle — always visible in top-right corner */}
      <button
        onClick={toggleLang}
        style={{
          position: "fixed",
          top: 16,
          right: 16,
          padding: "6px 14px",
          borderRadius: 20,
          border: "1px solid var(--border-color)",
          background: "var(--bg-secondary)",
          color: "var(--text-secondary)",
          cursor: "pointer",
          fontSize: 13,
          fontWeight: 500,
          zIndex: 100,
        }}
      >
        {currentLang === "zh" ? "EN" : "中文"}
      </button>
      <div style={{ maxWidth: 480, width: "100%" }}>
        {step === "welcome" && (
          <div style={{ textAlign: "center" }}>
            <div style={{ fontSize: 64, marginBottom: 16 }}>🐟</div>
            <h1 style={{ fontSize: 28, fontWeight: 700, color: "var(--text-primary)", marginBottom: 12 }}>
              {t("onboarding.welcomeTitle")}
            </h1>
            <p style={{ color: "var(--text-secondary)", marginBottom: 32, lineHeight: 1.7 }}>
              {t("onboarding.welcomeDesc")}
            </p>
            <button className="btn btn-primary" style={{ padding: "12px 32px", fontSize: 16 }} onClick={() => setStep("provider")}>
              {t("onboarding.getStarted")}
            </button>
          </div>
        )}

        {step === "provider" && (
          <div>
            <h2 style={{ fontSize: 22, fontWeight: 600, color: "var(--text-primary)", marginBottom: 8 }}>
              {t("onboarding.configureTitle")}
            </h2>
            <p style={{ color: "var(--text-secondary)", marginBottom: 24, fontSize: 14 }}>
              {t("onboarding.configureDesc")}
            </p>

            {error && (
              <div style={{ padding: "10px 14px", background: "rgba(248,113,113,0.1)", border: "1px solid var(--error)", borderRadius: "var(--radius)", color: "var(--error)", marginBottom: 16, fontSize: 13 }}>
                {error}
              </div>
            )}

            <div className="form-group">
              <label className="label">{t("onboarding.aiProvider")}</label>
              <select className="input" value={provider} onChange={(e) => {
                setProvider(e.target.value);
                setModel(getDefaultModel(e.target.value));
              }}>
                <option value="anthropic">{t("onboarding.anthropicRecommended")}</option>
                <option value="openai">OpenAI (GPT)</option>
                <option value="deepseek">DeepSeek（深度求索）</option>
                <option value="qwen">通义千问 (Qwen)</option>
              </select>
            </div>

            <div className="form-group">
              <label className="label">{t("onboarding.apiKey")}</label>
              <input
                className="input"
                type="password"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder={provider === "anthropic" ? "sk-ant-..." : "sk-..."}
                autoFocus
              />
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>{getKeyHelp()}</p>
            </div>

            <div className="form-group">
              <label className="label">{t("onboarding.model")}</label>
              <input className="input" value={model} onChange={(e) => setModel(e.target.value)} />
            </div>

            <div className="form-group">
              <label className="label">{t("onboarding.workspace")}</label>
              <div style={{ display: "flex", gap: 8 }}>
                <input
                  className="input"
                  value={workspace}
                  onChange={(e) => setWorkspace(e.target.value)}
                  placeholder={t("onboarding.workspacePlaceholder")}
                  style={{ flex: 1 }}
                />
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={browseWorkspace}
                  style={{ whiteSpace: "nowrap", padding: "0 14px" }}
                >
                  {t("onboarding.workspaceBrowse")}
                </button>
              </div>
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>
                {t("onboarding.workspaceHelp")}
              </p>
            </div>

            <div style={{ display: "flex", gap: 8, justifyContent: "space-between", marginTop: 24 }}>
              <button className="btn btn-secondary" onClick={() => setStep("welcome")}>{t("onboarding.backBtn")}</button>
              <button className="btn btn-primary" onClick={() => setStep("policy")} disabled={!apiKey.trim()}>
                {t("onboarding.nextBtn")}
              </button>
            </div>
          </div>
        )}

        {step === "policy" && (
          <div>
            <h2 style={{ fontSize: 22, fontWeight: 600, color: "var(--text-primary)", marginBottom: 8 }}>
              {t("onboarding.policyTitle")}
            </h2>
            <p style={{ color: "var(--text-secondary)", marginBottom: 24, fontSize: 14 }}>
              {t("onboarding.policyDesc")}
            </p>

            {[
              { value: "strict", label: t("onboarding.policyStrict"), desc: t("onboarding.policyStrictDesc") },
              { value: "balanced", label: t("onboarding.policyBalanced"), desc: t("onboarding.policyBalancedDesc") },
              { value: "dev", label: t("onboarding.policyDev"), desc: t("onboarding.policyDevDesc") },
            ].map((opt) => (
              <div
                key={opt.value}
                onClick={() => setPolicyMode(opt.value)}
                style={{
                  padding: "14px 16px",
                  borderRadius: "var(--radius)",
                  border: `2px solid ${policyMode === opt.value ? "var(--accent)" : "var(--border)"}`,
                  marginBottom: 10,
                  cursor: "pointer",
                  background: policyMode === opt.value ? "rgba(var(--accent-rgb),0.06)" : "transparent",
                  transition: "border-color 0.15s",
                }}
              >
                <div style={{ fontWeight: 600, color: "var(--text-primary)", marginBottom: 2 }}>{opt.label}</div>
                <div style={{ fontSize: 13, color: "var(--text-secondary)" }}>{opt.desc}</div>
              </div>
            ))}

            {error && (
              <div style={{ padding: "10px 14px", background: "rgba(248,113,113,0.1)", border: "1px solid var(--error)", borderRadius: "var(--radius)", color: "var(--error)", marginBottom: 16, fontSize: 13 }}>
                {error}
              </div>
            )}

            <div style={{ display: "flex", gap: 8, justifyContent: "space-between", marginTop: 24 }}>
              <button className="btn btn-secondary" onClick={() => setStep("provider")}>{t("onboarding.backBtn")}</button>
              <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                {saving ? t("common.saving") : t("onboarding.saveAndContinue")}
              </button>
            </div>
          </div>
        )}

        {step === "done" && (
          <div style={{ textAlign: "center" }}>
            <div style={{ fontSize: 64, marginBottom: 16 }}>✅</div>
            <h2 style={{ fontSize: 22, fontWeight: 600, color: "var(--text-primary)", marginBottom: 12 }}>
              {t("onboarding.doneTitle")}
            </h2>
            <p style={{ color: "var(--text-secondary)", marginBottom: 32 }}>
              {t("onboarding.doneDesc")}
            </p>
            <button className="btn btn-primary" style={{ padding: "12px 32px", fontSize: 16 }} onClick={onComplete}>
              {t("onboarding.startChatting")}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
