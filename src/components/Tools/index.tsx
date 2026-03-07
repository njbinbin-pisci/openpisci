import { useState, useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import {
  builtinToolsApi, userToolsApi, settingsApi, mcpApi,
  BuiltinToolInfo, UserToolInfo, ConfigFieldSchema,
  McpServerConfig, McpToolInfo,
} from "../../services/tauri";
import "./Tools.css";

// ─── Config Form (for user tools) ────────────────────────────────────────────

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
      setMessage(t("tools.configSaved"));
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
          placeholder={isPassword ? (value === "••••••••" ? t("tools.passwordSaved") : placeholder) : placeholder}
          onChange={(e) => {
            if (isPassword && e.target.value === "") {
              const newVals = { ...values };
              delete newVals[key];
              setValues(newVals);
            } else {
              setValues({ ...values, [key]: e.target.value });
            }
          }}
        />
        {schema.description && <p className="config-hint">{schema.description}</p>}
      </div>
    );
  };

  return (
    <div className="config-modal-overlay" onClick={onClose}>
      <div className="config-modal" onClick={(e) => e.stopPropagation()}>
        <div className="config-modal-header">
          <h3>{t("tools.configTitle")} — {tool.name}</h3>
          <button className="config-close-btn" onClick={onClose}>✕</button>
        </div>
        <form onSubmit={handleSubmit} className="config-form">
          {Object.entries(tool.config_schema).map(([key, schema]) => renderField(key, schema))}
          {Object.keys(tool.config_schema).length === 0 && (
            <p className="config-empty">{t("tools.noConfigNeeded")}</p>
          )}
          <div className="config-actions">
            <button type="button" className="btn btn-secondary" onClick={onClose}>
              {t("common.cancel")}
            </button>
            <button type="submit" className="btn btn-primary" disabled={saving}>
              {saving ? t("tools.savingConfig") : t("tools.saveConfig")}
            </button>
          </div>
          {message && <p className="config-message">{message}</p>}
        </form>
      </div>
    </div>
  );
}

// ─── Builtin Tool Card ────────────────────────────────────────────────────────

const TOOL_DISPLAY_NAME: Record<string, string> = {
  powershell_query: "PowerShell Query",
};

interface BuiltinToolCardProps {
  tool: BuiltinToolInfo;
  enabled: boolean;
  onToggle: (name: string, enabled: boolean) => void;
}

function BuiltinToolCard({ tool, enabled, onToggle }: BuiltinToolCardProps) {
  const { t } = useTranslation();
  const displayName = TOOL_DISPLAY_NAME[tool.name] ?? tool.name;
  const descKey = `tools.desc_${tool.name}` as any;
  const i18nDesc = t(descKey);
  const description = i18nDesc === descKey ? tool.description : i18nDesc;
  return (
    <div className={`tool-card tool-card--builtin ${enabled ? "" : "tool-card--disabled"}`}>
      <div className="tool-card-header">
        <span className="tool-runtime-icon">{tool.icon}</span>
        <div className="tool-meta">
          <span className="tool-name">{displayName}</span>
          <span className="tool-desc">{description}</span>
        </div>
        <div className="tool-right">
          <label className="tool-switch" title={enabled ? t("common.enable") : t("common.disable")}>
            <input
              type="checkbox"
              checked={enabled}
              onChange={(e) => onToggle(tool.name, e.target.checked)}
            />
            <span className="tool-switch-slider" />
          </label>
          <div className="tool-badges">
            <span className="badge badge-builtin">{t("common.builtin")}</span>
            {tool.windows_only && (
              <span className="badge badge-win">{t("tools.windowsOnly")}</span>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

// ─── User Tool Card ───────────────────────────────────────────────────────────

interface UserToolCardProps {
  tool: UserToolInfo;
  onUninstall: (name: string) => void;
  onConfigure: (tool: UserToolInfo) => void;
}

function UserToolCard({ tool, onUninstall, onConfigure }: UserToolCardProps) {
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
            <span className="badge badge-ok">{t("tools.hasConfig")}</span>
          ) : (
            <span className="badge badge-warn">{t("tools.noConfig")}</span>
          )}
        </div>
      </div>
      <div className="tool-card-footer">
        <span className="tool-detail">
          v{tool.version}
          {tool.author && ` · ${tool.author}`}
        </span>
        <div className="tool-actions">
          <button className="btn btn-sm btn-secondary" onClick={() => onConfigure(tool)}>
            {t("tools.configure")}
          </button>
          <button
            className="btn btn-sm btn-danger"
            onClick={() => {
              if (confirm(t("tools.confirmUninstall", { name: tool.name }))) {
                onUninstall(tool.name);
              }
            }}
          >
            {t("tools.uninstall")}
          </button>
        </div>
      </div>
    </div>
  );
}

// ─── MCP Server Form ──────────────────────────────────────────────────────────

function emptyServer(): McpServerConfig {
  return {
    name: "",
    transport: "stdio",
    command: "",
    args: [],
    url: "",
    env: {},
    enabled: true,
  };
}

interface McpServerFormProps {
  initial: McpServerConfig;
  onSave: (cfg: McpServerConfig) => void;
  onCancel: () => void;
}

function McpServerForm({ initial, onSave, onCancel }: McpServerFormProps) {
  const { t } = useTranslation();
  const [cfg, setCfg] = useState<McpServerConfig>({ ...initial });
  const [argsText, setArgsText] = useState(initial.args.join("\n"));
  const [envText, setEnvText] = useState(
    Object.entries(initial.env).map(([k, v]) => `${k}=${v}`).join("\n")
  );
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; msg: string; tools: McpToolInfo[] } | null>(null);

  const handleTest = async () => {
    setTesting(true);
    setTestResult(null);
    const built = buildConfig();
    try {
      const r = await mcpApi.test(built);
      if (r.success) {
        setTestResult({ ok: true, msg: t("tools.mcpTestSuccess", { count: r.tools.length }), tools: r.tools });
      } else {
        setTestResult({ ok: false, msg: t("tools.mcpTestFailed", { error: r.error ?? "unknown" }), tools: [] });
      }
    } catch (e) {
      setTestResult({ ok: false, msg: t("tools.mcpTestFailed", { error: String(e) }), tools: [] });
    } finally {
      setTesting(false);
    }
  };

  const buildConfig = (): McpServerConfig => ({
    ...cfg,
    args: argsText.split("\n").map(s => s.trim()).filter(Boolean),
    env: Object.fromEntries(
      envText.split("\n").map(s => s.trim()).filter(s => s.includes("=")).map(s => {
        const idx = s.indexOf("=");
        return [s.slice(0, idx), s.slice(idx + 1)];
      })
    ),
  });

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    onSave(buildConfig());
  };

  return (
    <div className="config-modal-overlay" onClick={onCancel}>
      <div className="config-modal mcp-server-modal" onClick={e => e.stopPropagation()}>
        <div className="config-modal-header">
          <h3>{initial.name ? t("tools.mcpEditServer") : t("tools.mcpAddServer")}</h3>
          <button className="config-close-btn" onClick={onCancel}>✕</button>
        </div>
        <form onSubmit={handleSubmit} className="config-form">
          {/* Name */}
          <div className="config-field">
            <label className="config-label">{t("tools.mcpName")}</label>
            <input
              className="config-input"
              value={cfg.name}
              onChange={e => setCfg({ ...cfg, name: e.target.value })}
              required
              placeholder="my-mcp-server"
            />
          </div>

          {/* Transport */}
          <div className="config-field">
            <label className="config-label">{t("tools.mcpTransport")}</label>
            <select
              className="config-input"
              value={cfg.transport}
              onChange={e => setCfg({ ...cfg, transport: e.target.value as "stdio" | "sse" })}
            >
              <option value="stdio">{t("tools.mcpTransportStdio")}</option>
              <option value="sse">{t("tools.mcpTransportSse")}</option>
            </select>
          </div>

          {/* Stdio fields */}
          {cfg.transport === "stdio" && (
            <>
              <div className="config-field">
                <label className="config-label">{t("tools.mcpCommand")}</label>
                <input
                  className="config-input"
                  value={cfg.command}
                  onChange={e => setCfg({ ...cfg, command: e.target.value })}
                  placeholder="npx"
                />
                <p className="config-hint">{t("tools.mcpCommandHint")}</p>
              </div>
              <div className="config-field">
                <label className="config-label">{t("tools.mcpArgs")}</label>
                <textarea
                  className="config-input config-textarea"
                  value={argsText}
                  onChange={e => setArgsText(e.target.value)}
                  rows={3}
                  placeholder="-y&#10;@modelcontextprotocol/server-filesystem&#10;/path/to/dir"
                />
                <p className="config-hint">{t("tools.mcpArgsHint")}</p>
              </div>
            </>
          )}

          {/* SSE field */}
          {cfg.transport === "sse" && (
            <div className="config-field">
              <label className="config-label">{t("tools.mcpUrl")}</label>
              <input
                className="config-input"
                value={cfg.url}
                onChange={e => setCfg({ ...cfg, url: e.target.value })}
                placeholder="http://localhost:3000"
              />
              <p className="config-hint">{t("tools.mcpUrlHint")}</p>
            </div>
          )}

          {/* Env vars */}
          <div className="config-field">
            <label className="config-label">{t("tools.mcpEnv")}</label>
            <textarea
              className="config-input config-textarea"
              value={envText}
              onChange={e => setEnvText(e.target.value)}
              rows={3}
              placeholder="API_KEY=your-key&#10;DEBUG=1"
            />
            <p className="config-hint">{t("tools.mcpEnvHint")}</p>
          </div>

          {/* Enabled toggle */}
          <div className="config-field">
            <label className="config-label config-label--checkbox">
              <input
                type="checkbox"
                checked={cfg.enabled}
                onChange={e => setCfg({ ...cfg, enabled: e.target.checked })}
              />
              {t("tools.mcpEnabled")}
            </label>
          </div>

          {/* Test result */}
          {testResult && (
            <div className={`mcp-test-result ${testResult.ok ? "mcp-test-ok" : "mcp-test-fail"}`}>
              <div className="mcp-test-msg">{testResult.ok ? "✓" : "✗"} {testResult.msg}</div>
              {testResult.tools.length > 0 && (
                <div className="mcp-test-tools">
                  <div className="mcp-test-tools-label">{t("tools.mcpToolsFound")}:</div>
                  <ul className="mcp-test-tools-list">
                    {testResult.tools.map(tool => (
                      <li key={tool.name}>
                        <span className="mcp-tool-name">{tool.name}</span>
                        {tool.description && <span className="mcp-tool-desc"> — {tool.description}</span>}
                      </li>
                    ))}
                  </ul>
                </div>
              )}
            </div>
          )}

          <div className="config-actions">
            <button type="button" className="btn btn-secondary" onClick={handleTest} disabled={testing}>
              {testing ? t("tools.mcpTesting") : t("tools.mcpTestServer")}
            </button>
            <div style={{ flex: 1 }} />
            <button type="button" className="btn btn-secondary" onClick={onCancel}>
              {t("tools.mcpCancel")}
            </button>
            <button type="submit" className="btn btn-primary">
              {t("tools.mcpSave")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

// ─── MCP Server Card ──────────────────────────────────────────────────────────

interface McpServerCardProps {
  server: McpServerConfig;
  onEdit: (s: McpServerConfig) => void;
  onDelete: (name: string) => void;
}

function McpServerCard({ server, onEdit, onDelete }: McpServerCardProps) {
  const { t } = useTranslation();
  return (
    <div className={`tool-card mcp-server-card ${server.enabled ? "" : "tool-card--disabled"}`}>
      <div className="tool-card-header">
        <span className="tool-runtime-icon">🔗</span>
        <div className="tool-meta">
          <span className="tool-name">{server.name}</span>
          <span className="tool-desc">
            {server.transport === "stdio"
              ? `${server.command} ${server.args.join(" ")}`
              : server.url}
          </span>
        </div>
        <div className="tool-badges">
          <span className="badge badge-runtime">{server.transport}</span>
          <span className={`badge ${server.enabled ? "badge-ok" : "badge-warn"}`}>
            {server.enabled ? t("tools.mcpEnabled_badge") : t("tools.mcpDisabled_badge")}
          </span>
        </div>
      </div>
      <div className="tool-card-footer">
        <span className="tool-detail" />
        <div className="tool-actions">
          <button className="btn btn-sm btn-secondary" onClick={() => onEdit(server)}>
            {t("tools.configure")}
          </button>
          <button
            className="btn btn-sm btn-danger"
            onClick={() => {
              if (confirm(t("tools.mcpDeleteConfirm", { name: server.name }))) {
                onDelete(server.name);
              }
            }}
          >
            {t("tools.mcpDeleteServer")}
          </button>
        </div>
      </div>
    </div>
  );
}

// ─── Main Page ────────────────────────────────────────────────────────────────

type ToolsTab = "builtin" | "user" | "mcp";

export default function Tools() {
  const { t } = useTranslation();
  const [activeTab, setActiveTab] = useState<ToolsTab>("builtin");

  // Builtin tools state
  const [builtinTools, setBuiltinTools] = useState<BuiltinToolInfo[]>([]);
  const [builtinEnabled, setBuiltinEnabled] = useState<Record<string, boolean>>({});
  const [loadingBuiltin, setLoadingBuiltin] = useState(true);

  // User tools state
  const [userTools, setUserTools] = useState<UserToolInfo[]>([]);
  const [loadingUser, setLoadingUser] = useState(true);
  const [installSource, setInstallSource] = useState("");
  const [installing, setInstalling] = useState(false);
  const [configuringTool, setConfiguringTool] = useState<UserToolInfo | null>(null);

  // MCP state
  const [mcpServers, setMcpServers] = useState<McpServerConfig[]>([]);
  const [loadingMcp, setLoadingMcp] = useState(true);
  const [editingServer, setEditingServer] = useState<McpServerConfig | null>(null);
  const [addingServer, setAddingServer] = useState(false);

  // Shared status
  const [status, setStatus] = useState<{ type: "ok" | "err"; msg: string } | null>(null);

  // ── Load builtin tools ──────────────────────────────────────────────────────
  useEffect(() => {
    Promise.all([builtinToolsApi.list(), settingsApi.get()])
      .then(([tools, settings]) => {
        setBuiltinTools(tools);
        setBuiltinEnabled(settings.builtin_tool_enabled ?? {});
      })
      .finally(() => setLoadingBuiltin(false));
  }, []);

  const getBuiltinEnabled = (name: string) => builtinEnabled[name] !== false;

  const handleBuiltinToggle = async (toolName: string, enabled: boolean) => {
    const next = { ...builtinEnabled, [toolName]: enabled };
    setBuiltinEnabled(next);
    try {
      await settingsApi.save({ builtin_tool_enabled: next });
      setStatus({ type: "ok", msg: t("tools.toggleSaved") });
    } catch (err) {
      setBuiltinEnabled((prev) => ({ ...prev, [toolName]: !enabled }));
      setStatus({ type: "err", msg: t("tools.toggleFailed", { error: String(err) }) });
    }
  };

  // ── Load user tools ─────────────────────────────────────────────────────────
  const refreshUserTools = useCallback(async () => {
    setLoadingUser(true);
    try {
      const list = await userToolsApi.list();
      setUserTools(list);
    } catch (e) {
      setStatus({ type: "err", msg: String(e) });
    } finally {
      setLoadingUser(false);
    }
  }, []);

  useEffect(() => { refreshUserTools(); }, [refreshUserTools]);

  const handleInstall = async () => {
    if (!installSource.trim()) return;
    setInstalling(true);
    setStatus(null);
    try {
      await userToolsApi.install(installSource.trim());
      setStatus({ type: "ok", msg: t("tools.installSuccess") });
      setInstallSource("");
      await refreshUserTools();
    } catch (err) {
      setStatus({ type: "err", msg: `${t("tools.installFailed")}: ${err}` });
    } finally {
      setInstalling(false);
    }
  };

  const handleUninstall = async (name: string) => {
    try {
      await userToolsApi.uninstall(name);
      setStatus({ type: "ok", msg: t("tools.uninstallSuccess") });
      await refreshUserTools();
    } catch (err) {
      setStatus({ type: "err", msg: `${t("tools.uninstallFailed")}: ${err}` });
    }
  };

  // ── Load MCP servers ────────────────────────────────────────────────────────
  const refreshMcpServers = useCallback(async () => {
    setLoadingMcp(true);
    try {
      const list = await mcpApi.list();
      setMcpServers(list);
    } catch (e) {
      setStatus({ type: "err", msg: String(e) });
    } finally {
      setLoadingMcp(false);
    }
  }, []);

  useEffect(() => { refreshMcpServers(); }, [refreshMcpServers]);

  const saveMcpServers = async (servers: McpServerConfig[]) => {
    try {
      await mcpApi.save(servers);
      setMcpServers(servers);
      setStatus({ type: "ok", msg: t("tools.mcpSaved") });
    } catch (err) {
      setStatus({ type: "err", msg: t("tools.mcpSaveFailed", { error: String(err) }) });
    }
  };

  const handleMcpSave = async (cfg: McpServerConfig) => {
    let next: McpServerConfig[];
    if (addingServer) {
      next = [...mcpServers, cfg];
    } else {
      next = mcpServers.map(s => s.name === (editingServer?.name ?? cfg.name) ? cfg : s);
    }
    await saveMcpServers(next);
    setEditingServer(null);
    setAddingServer(false);
  };

  const handleMcpDelete = async (name: string) => {
    const next = mcpServers.filter(s => s.name !== name);
    await saveMcpServers(next);
  };

  return (
    <div className="tools-page">
      <div className="page-header">
        <h2>🔧 {t("tools.title")}</h2>
      </div>

      {/* ── Tab Bar ── */}
      <div className="tools-tabs">
        <button
          className={`tools-tab ${activeTab === "builtin" ? "active" : ""}`}
          onClick={() => setActiveTab("builtin")}
        >
          ⚙️ {t("tools.tabBuiltin")}
        </button>
        <button
          className={`tools-tab ${activeTab === "user" ? "active" : ""}`}
          onClick={() => setActiveTab("user")}
        >
          🔌 {t("tools.tabUser")}
        </button>
        <button
          className={`tools-tab ${activeTab === "mcp" ? "active" : ""}`}
          onClick={() => setActiveTab("mcp")}
        >
          🔗 {t("tools.tabMcp")}
          {mcpServers.length > 0 && (
            <span className="tools-tab-badge">{mcpServers.length}</span>
          )}
        </button>
      </div>

      {/* Shared status banner */}
      {status && (
        <div className={`status-banner status-${status.type}`} style={{ margin: "0 0 12px" }}>
          {status.msg}
        </div>
      )}

      {/* ── Builtin Tab ── */}
      {activeTab === "builtin" && (
        <section className="tools-section">
          <div className="section-header">
            <p className="section-desc">{t("tools.builtinDesc")}</p>
          </div>
          {loadingBuiltin ? (
            <div className="loading-row">{t("common.loading")}</div>
          ) : (
            <div className="builtin-grid">
              {builtinTools.map((tool) => (
                <BuiltinToolCard
                  key={tool.name}
                  tool={tool}
                  enabled={getBuiltinEnabled(tool.name)}
                  onToggle={handleBuiltinToggle}
                />
              ))}
            </div>
          )}
        </section>
      )}

      {/* ── User Tools Tab ── */}
      {activeTab === "user" && (
        <section className="tools-section">
          <div className="section-header">
            <p className="section-desc">{t("tools.userDesc")}</p>
          </div>

          <div className="install-box">
            <div className="install-row">
              <input
                className="install-input"
                type="text"
                value={installSource}
                onChange={(e) => setInstallSource(e.target.value)}
                placeholder={t("tools.installPlaceholder")}
                onKeyDown={(e) => e.key === "Enter" && handleInstall()}
              />
              <button
                className="btn btn-primary"
                onClick={handleInstall}
                disabled={installing || !installSource.trim()}
              >
                {installing ? t("tools.installing") : t("tools.installBtn")}
              </button>
            </div>
            <p className="hint">{t("tools.runtimeHint")}</p>
          </div>

          <div className="tools-list">
            {loadingUser ? (
              <div className="loading-row">{t("common.loading")}</div>
            ) : userTools.length === 0 ? (
              <div className="empty-state">{t("tools.noUserTools")}</div>
            ) : (
              userTools.map((tool) => (
                <UserToolCard
                  key={tool.name}
                  tool={tool}
                  onUninstall={handleUninstall}
                  onConfigure={setConfiguringTool}
                />
              ))
            )}
          </div>
        </section>
      )}

      {/* ── MCP Tab ── */}
      {activeTab === "mcp" && (
        <section className="tools-section">
          <div className="section-header">
            <p className="section-desc">{t("tools.mcpDesc")}</p>
          </div>

          <div className="mcp-toolbar">
            <button
              className="btn btn-primary"
              onClick={() => { setAddingServer(true); setEditingServer(null); }}
            >
              + {t("tools.mcpAddServer")}
            </button>
          </div>

          <div className="tools-list">
            {loadingMcp ? (
              <div className="loading-row">{t("common.loading")}</div>
            ) : mcpServers.length === 0 ? (
              <div className="empty-state">{t("tools.mcpNoServers")}</div>
            ) : (
              mcpServers.map((server) => (
                <McpServerCard
                  key={server.name}
                  server={server}
                  onEdit={(s) => { setEditingServer(s); setAddingServer(false); }}
                  onDelete={handleMcpDelete}
                />
              ))
            )}
          </div>
        </section>
      )}

      {/* Config modal for user tools */}
      {configuringTool && (
        <ConfigForm
          tool={configuringTool}
          onClose={() => setConfiguringTool(null)}
          onSaved={() => {
            setConfiguringTool(null);
            refreshUserTools();
          }}
        />
      )}

      {/* MCP server add/edit modal */}
      {(addingServer || editingServer) && (
        <McpServerForm
          initial={editingServer ?? emptyServer()}
          onSave={handleMcpSave}
          onCancel={() => { setAddingServer(false); setEditingServer(null); }}
        />
      )}
    </div>
  );
}
