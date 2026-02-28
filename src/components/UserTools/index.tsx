import { useState, useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { userToolsApi, UserToolInfo, ConfigFieldSchema } from "../../services/tauri";
import "./UserTools.css";

// ─── Config Form ─────────────────────────────────────────────────────────────

interface ConfigFormProps {
  tool: UserToolInfo;
  onClose: () => void;
  onSaved: () => void;
}

function ConfigForm({ tool, onClose, onSaved }: ConfigFormProps) {
  const { t } = useTranslation();
  const [values, setValues] = useState<Record<string, unknown>>({});
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState("");

  useEffect(() => {
    userToolsApi.getConfig(tool.name).then((cfg) => {
      setValues(cfg as Record<string, unknown>);
    });
  }, [tool.name]);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setSaving(true);
    setMessage("");
    try {
      await userToolsApi.saveConfig(tool.name, values);
      setMessage(t("userTools.configSaved"));
      onSaved();
    } catch (err) {
      setMessage(`${t("common.error")}: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  const renderField = (key: string, schema: ConfigFieldSchema) => {
    const label = schema.label ?? key;
    const placeholder = schema.placeholder ?? "";
    const value = (values[key] as string | number | boolean | undefined) ?? "";

    if (schema.type === "boolean") {
      return (
        <div key={key} className="config-field">
          <label className="config-label config-label--checkbox">
            <input
              type="checkbox"
              checked={Boolean(value)}
              onChange={(e) => setValues({ ...values, [key]: e.target.checked })}
            />
            {label}
          </label>
        </div>
      );
    }

    if (schema.type === "number") {
      return (
        <div key={key} className="config-field">
          <label className="config-label">{label}</label>
          <input
            type="number"
            className="config-input"
            value={value as number}
            placeholder={placeholder}
            onChange={(e) => setValues({ ...values, [key]: Number(e.target.value) })}
          />
        </div>
      );
    }

    const isPassword = schema.type === "password";
    return (
      <div key={key} className="config-field">
        <label className="config-label">
          {label}
          {isPassword && <span className="config-badge">•••</span>}
        </label>
        <input
          type={isPassword ? "password" : "text"}
          className="config-input"
          value={value as string}
          placeholder={isPassword ? (value === "••••••••" ? "已保存（留空不更改）" : placeholder) : placeholder}
          onChange={(e) => {
            // If user clears the masked value, treat as "keep existing"
            if (isPassword && e.target.value === "") {
              const newVals = { ...values };
              delete newVals[key];
              setValues(newVals);
            } else {
              setValues({ ...values, [key]: e.target.value });
            }
          }}
        />
        {schema.description && (
          <p className="config-hint">{schema.description}</p>
        )}
      </div>
    );
  };

  return (
    <div className="config-modal-overlay" onClick={onClose}>
      <div className="config-modal" onClick={(e) => e.stopPropagation()}>
        <div className="config-modal-header">
          <h3>{t("userTools.configTitle")} — {tool.name}</h3>
          <button className="config-close-btn" onClick={onClose}>✕</button>
        </div>
        <form onSubmit={handleSubmit} className="config-form">
          {Object.entries(tool.config_schema).map(([key, schema]) =>
            renderField(key, schema)
          )}
          {Object.keys(tool.config_schema).length === 0 && (
            <p className="config-empty">此工具无需配置</p>
          )}
          <div className="config-actions">
            <button type="button" className="btn btn-secondary" onClick={onClose}>
              {t("common.cancel")}
            </button>
            <button type="submit" className="btn btn-primary" disabled={saving}>
              {saving ? t("userTools.savingConfig") : t("userTools.saveConfig")}
            </button>
          </div>
          {message && <p className="config-message">{message}</p>}
        </form>
      </div>
    </div>
  );
}

// ─── Tool Card ────────────────────────────────────────────────────────────────

interface ToolCardProps {
  tool: UserToolInfo;
  onUninstall: (name: string) => void;
  onConfigure: (tool: UserToolInfo) => void;
}

function ToolCard({ tool, onUninstall, onConfigure }: ToolCardProps) {
  const { t } = useTranslation();

  const runtimeIcon: Record<string, string> = {
    deno: "🦕",
    node: "⬢",
    powershell: "🪟",
    ps1: "🪟",
    python: "🐍",
    python3: "🐍",
    bun: "🐰",
  };

  return (
    <div className="tool-card">
      <div className="tool-card-header">
        <span className="tool-runtime-icon">{runtimeIcon[tool.runtime] ?? "🔧"}</span>
        <div className="tool-meta">
          <span className="tool-name">{tool.name}</span>
          <span className="tool-desc">{tool.description}</span>
        </div>
        <div className="tool-badges">
          <span className="badge badge-runtime">{tool.runtime}</span>
          {tool.has_config ? (
            <span className="badge badge-ok">{t("userTools.hasConfig")}</span>
          ) : (
            <span className="badge badge-warn">{t("userTools.noConfig")}</span>
          )}
        </div>
      </div>
      <div className="tool-card-footer">
        <span className="tool-detail">
          v{tool.version}
          {tool.author && ` · ${tool.author}`}
        </span>
        <div className="tool-actions">
          <button
            className="btn btn-sm btn-secondary"
            onClick={() => onConfigure(tool)}
          >
            {t("userTools.configure")}
          </button>
          <button
            className="btn btn-sm btn-danger"
            onClick={() => {
              if (confirm(`确认卸载工具「${tool.name}」？`)) {
                onUninstall(tool.name);
              }
            }}
          >
            {t("userTools.uninstall")}
          </button>
        </div>
      </div>
    </div>
  );
}

// ─── Main Page ────────────────────────────────────────────────────────────────

export default function UserTools() {
  const { t } = useTranslation();
  const [tools, setTools] = useState<UserToolInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [installSource, setInstallSource] = useState("");
  const [installing, setInstalling] = useState(false);
  const [status, setStatus] = useState<{ type: "ok" | "err"; msg: string } | null>(null);
  const [configuringTool, setConfiguringTool] = useState<UserToolInfo | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const list = await userToolsApi.list();
      setTools(list);
    } catch (e) {
      setStatus({ type: "err", msg: String(e) });
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { refresh(); }, [refresh]);

  const handleInstall = async () => {
    if (!installSource.trim()) return;
    setInstalling(true);
    setStatus(null);
    try {
      await userToolsApi.install(installSource.trim());
      setStatus({ type: "ok", msg: t("userTools.installSuccess") });
      setInstallSource("");
      await refresh();
    } catch (err) {
      setStatus({ type: "err", msg: `${t("userTools.installFailed")}: ${err}` });
    } finally {
      setInstalling(false);
    }
  };

  const handleUninstall = async (name: string) => {
    try {
      await userToolsApi.uninstall(name);
      setStatus({ type: "ok", msg: t("userTools.uninstallSuccess") });
      await refresh();
    } catch (err) {
      setStatus({ type: "err", msg: `${t("userTools.uninstallFailed")}: ${err}` });
    }
  };

  return (
    <div className="user-tools-page">
      <div className="page-header">
        <h2>{t("userTools.title")}</h2>
        <p className="page-subtitle">{t("userTools.subtitle")}</p>
      </div>

      {/* Install box */}
      <div className="install-box">
        <h3 className="section-title">{t("userTools.installTitle")}</h3>
        <div className="install-row">
          <input
            className="install-input"
            type="text"
            value={installSource}
            onChange={(e) => setInstallSource(e.target.value)}
            placeholder={t("userTools.installPlaceholder")}
            onKeyDown={(e) => e.key === "Enter" && handleInstall()}
          />
          <button
            className="btn btn-primary"
            onClick={handleInstall}
            disabled={installing || !installSource.trim()}
          >
            {installing ? t("userTools.installing") : t("userTools.installBtn")}
          </button>
        </div>
        <p className="hint">{t("userTools.runtimeHint")}</p>
        {status && (
          <div className={`status-banner status-${status.type}`}>{status.msg}</div>
        )}
      </div>

      {/* Installed tools */}
      <div className="tools-list">
        <h3 className="section-title">{t("userTools.installed")} ({tools.length})</h3>
        {loading ? (
          <div className="loading-row">{t("common.loading")}</div>
        ) : tools.length === 0 ? (
          <div className="empty-state">{t("userTools.noTools")}</div>
        ) : (
          tools.map((tool) => (
            <ToolCard
              key={tool.name}
              tool={tool}
              onUninstall={handleUninstall}
              onConfigure={setConfiguringTool}
            />
          ))
        )}
      </div>

      {/* Config modal */}
      {configuringTool && (
        <ConfigForm
          tool={configuringTool}
          onClose={() => setConfiguringTool(null)}
          onSaved={() => {
            setConfiguringTool(null);
            refresh();
          }}
        />
      )}
    </div>
  );
}
