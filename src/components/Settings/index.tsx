import { useState, useEffect, useCallback } from "react";
import { useDispatch, useSelector } from "react-redux";
import { useTranslation } from "react-i18next";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { open as openUrl } from "@tauri-apps/plugin-shell";
import { RootState, settingsActions } from "../../store";
import { settingsApi, gatewayApi, systemApi, Settings as SettingsData, ChannelInfo, RuntimeCheckItem, SshServerConfig } from "../../services/tauri";
import { setLanguage } from "../../i18n";

const DEFAULT_SETTINGS: SettingsData = {
  anthropic_api_key: "",
  openai_api_key: "",
  deepseek_api_key: "",
  qwen_api_key: "",
  provider: "anthropic",
  model: "claude-sonnet-4-5",
  custom_base_url: "",
  workspace_root: "",
  allow_outside_workspace: false,
  language: "zh",
  max_tokens: 4096,
  context_window: 0,
  confirm_shell_commands: true,
  confirm_file_writes: true,
  browser_headless: true,
  feishu_app_id: "",
  feishu_app_secret: "",
  feishu_domain: "feishu",
  feishu_enabled: false,
  wecom_corp_id: "",
  wecom_agent_secret: "",
  wecom_agent_id: "",
  wecom_enabled: false,
  wecom_inbox_file: "",
  dingtalk_app_key: "",
  dingtalk_app_secret: "",
  dingtalk_enabled: false,
  telegram_bot_token: "",
  telegram_enabled: false,
  slack_webhook_url: "",
  slack_enabled: false,
  discord_webhook_url: "",
  discord_enabled: false,
  teams_webhook_url: "",
  teams_enabled: false,
  matrix_homeserver: "",
  matrix_access_token: "",
  matrix_room_id: "",
  matrix_enabled: false,
  webhook_outbound_url: "",
  webhook_auth_token: "",
  webhook_enabled: false,
  // Email
  smtp_host: "",
  smtp_port: 587,
  smtp_username: "",
  smtp_password: "",
  imap_host: "",
  imap_port: 993,
  smtp_from_name: "",
  email_enabled: false,
  // User Tool configs
  minimax_api_key: "",
  zhipu_api_key: "",
  kimi_api_key: "",
  user_tool_configs: {},
  // Builtin tool switches
  builtin_tool_enabled: {},
  // Agent config
  max_iterations: 50,
  heartbeat_enabled: false,
  heartbeat_interval_mins: 30,
  heartbeat_prompt: "检查是否有待处理任务，如无则回复 HEARTBEAT_OK",
  vision_enabled: false,
  ssh_servers: [],
};

interface SettingsProps {
  theme: 'violet' | 'gold';
  setTheme: (t: 'violet' | 'gold') => void;
}

export default function Settings({ theme, setTheme }: SettingsProps) {
  const { t } = useTranslation();
  const dispatch = useDispatch();
  const { settings } = useSelector((s: RootState) => s.settings);
  const [form, setForm] = useState<SettingsData>({ ...DEFAULT_SETTINGS });
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [showKeys, setShowKeys] = useState(false);
  const [gatewayStatus, setGatewayStatus] = useState<ChannelInfo[]>([]);
  const [gatewayConnecting, setGatewayConnecting] = useState(false);
  const [gatewayDisconnecting, setGatewayDisconnecting] = useState(false);
  const [gatewayMsg, setGatewayMsg] = useState<string | null>(null);
  const [runtimes, setRuntimes] = useState<RuntimeCheckItem[]>([]);
  const [runtimesLoading, setRuntimesLoading] = useState(false);
  const [runtimesSettingKey, setRuntimesSettingKey] = useState<string | null>(null);

  // SSH Servers
  const [sshServers, setSshServers] = useState<SshServerConfig[]>([]);
  const [sshEditIdx, setSshEditIdx] = useState<number | null>(null);
  const [sshEditForm, setSshEditForm] = useState<SshServerConfig>({ id: "", label: "", host: "", port: 22, username: "", password: "", private_key: "" });
  const [sshShowPassword, setSshShowPassword] = useState(false);

  useEffect(() => {
    if (settings) {
      setForm({ ...DEFAULT_SETTINGS, ...settings });
      setSshServers(settings.ssh_servers ?? []);
    }
  }, [settings]);

  // Refresh gateway status on mount and whenever settings change (catches post-restart state)
  useEffect(() => {
    gatewayApi.list().then((r) => setGatewayStatus(r.channels)).catch(() => setGatewayStatus([]));
  }, [settings]);

  const handleGatewayConnect = async () => {
    setGatewayConnecting(true);
    setGatewayMsg(null);
    const timeout = new Promise<never>((_, reject) =>
      setTimeout(() => reject(new Error(t("settings.channelTimeout"))), 20000)
    );
    try {
      const r = await Promise.race([gatewayApi.connect(), timeout]);
      setGatewayStatus(r.channels);
      setGatewayMsg(t("settings.channelConnected"));
    } catch (e) {
      setGatewayMsg(t("settings.channelFailed", { error: String(e) }));
    } finally {
      setGatewayConnecting(false);
    }
  };

  const handleGatewayDisconnect = async () => {
    setGatewayDisconnecting(true);
    setGatewayMsg(null);
    try {
      await gatewayApi.disconnect();
      setGatewayStatus([]);
      setGatewayMsg(t("settings.channelDisconnected"));
    } catch (e) {
      setGatewayMsg(t("settings.channelDisconnectFailed", { error: String(e) }));
    } finally {
      setGatewayDisconnecting(false);
    }
  };

  const handleCheckRuntimes = useCallback(async () => {
    setRuntimesLoading(true);
    try {
      const items = await systemApi.checkRuntimes();
      setRuntimes(items);
    } catch {
      // ignore
    } finally {
      setRuntimesLoading(false);
    }
  }, []);

  const handleSelectRuntimePath = useCallback(async (runtimeKey: string, runtimeName: string) => {
    setRuntimesSettingKey(runtimeKey);
    try {
      const exeFilter = runtimeName === "Node.js" || runtimeName === "npm"
        ? [{ name: "Executable", extensions: ["exe", "cmd", "bat", "*"] }]
        : [{ name: "Executable", extensions: ["exe", "*"] }];
      const selected = await openFileDialog({ multiple: false, filters: exeFilter });
      if (!selected) return;
      const items = await systemApi.setRuntimePath(runtimeKey, selected as string);
      setRuntimes(items);
    } catch {
      // ignore
    } finally {
      setRuntimesSettingKey(null);
    }
  }, []);

  const handleClearRuntimePath = useCallback(async (runtimeKey: string) => {
    setRuntimesSettingKey(runtimeKey);
    try {
      const items = await systemApi.setRuntimePath(runtimeKey, "");
      setRuntimes(items);
    } catch {
      // ignore
    } finally {
      setRuntimesSettingKey(null);
    }
  }, []);

  const statusBadge = (s: ChannelInfo["status"]) => {
    if (s === "Connected") return <span style={{ color: "#28a745", fontWeight: 600 }}>● {t("common.connected")}</span>;
    if (s === "Connecting") return <span style={{ color: "#ffc107", fontWeight: 600 }}>● {t("common.connecting")}</span>;
    if (s === "Disconnected") return <span style={{ color: "var(--text-muted)" }}>● {t("common.disconnected")}</span>;
    if (typeof s === "object" && "Error" in s) return <span style={{ color: "#dc3545" }}>● {t("common.error")}: {s.Error}</span>;
    return null;
  };

  const handleSave = async () => {
    // Validate: workspace_root is required unless allow_outside_workspace is enabled
    if (!form.allow_outside_workspace && !(form.workspace_root ?? "").trim()) {
      setSaveError(t("settings.workspaceRootRequired"));
      return;
    }
    setSaving(true);
    setSaveError(null);
    try {
      const updated = await settingsApi.save({ ...form, ssh_servers: sshServers });
      dispatch(settingsActions.setSettings(updated));
      dispatch(settingsActions.setConfigured(updated.is_configured ?? !!(updated.anthropic_api_key || updated.openai_api_key || updated.deepseek_api_key || updated.qwen_api_key)));
      // 立即切换语言
      if (updated.language) setLanguage(updated.language as "zh" | "en");
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      setSaveError(t("settings.failedSave", { error: String(e) }));
    } finally {
      setSaving(false);
    }
  };

  const update = <K extends keyof SettingsData>(key: K, value: SettingsData[K]) => {
    setForm((prev) => ({ ...prev, [key]: value }));
  };

  return (
    <div className="page">
      <div className="page-header">
        <h1 className="page-title">⚙️ {t("settings.title")}</h1>
        <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
          {saved ? t("settings.saved") : saving ? t("settings.saving") : t("settings.saveChanges")}
        </button>
      </div>
      {saveError && (
        <div style={{ margin: "0 0 12px 0", padding: "8px 14px", background: "rgba(220,53,69,0.15)", borderLeft: "3px solid #dc3545", color: "#ff6b6b", fontSize: "0.85rem", display: "flex", justifyContent: "space-between" }}>
          <span>{saveError}</span>
          <button onClick={() => setSaveError(null)} style={{ background: "none", border: "none", color: "#ff6b6b", cursor: "pointer" }}>✕</button>
        </div>
      )}

      <div className="page-body" style={{ maxWidth: 640 }}>
        {/* AI Provider */}
        <section style={{ marginBottom: 32 }}>
          <h2 style={{ fontSize: 15, fontWeight: 600, color: "var(--text-primary)", marginBottom: 16, paddingBottom: 8, borderBottom: "1px solid var(--border)" }}>
            {t("settings.aiProvider")}
          </h2>

          <div className="form-group">
            <label className="label">{t("settings.provider")}</label>
            <select className="input" value={form.provider ?? "anthropic"} onChange={(e) => update("provider", e.target.value)}>
              <option value="anthropic">Anthropic (Claude)</option>
              <option value="openai">OpenAI (GPT)</option>
              <option value="deepseek">DeepSeek（深度求索）</option>
              <option value="qwen">阿里百炼（通义千问 Qwen）</option>
              <option value="minimax">MiniMax（稀宇科技）</option>
              <option value="zhipu">智谱 AI（GLM / Z.AI）</option>
              <option value="kimi">Kimi（月之暗面 Moonshot）</option>
              <option value="custom">{t("settings.customApiKey")} (OpenAI {t("common.enable")})</option>
            </select>
          </div>

          <div className="form-group">
            <label className="label">{t("settings.model")}</label>
            <input className="input" value={form.model ?? ""} onChange={(e) => update("model", e.target.value)}
              placeholder={
                form.provider === "anthropic" ? "claude-sonnet-4-5" :
                form.provider === "openai" ? "gpt-4o" :
                form.provider === "deepseek" ? "deepseek-chat" :
                form.provider === "qwen" ? "qwen3-max" :
                form.provider === "minimax" ? "MiniMax-M2.5" :
                form.provider === "zhipu" ? "glm-5" :
                form.provider === "kimi" ? "kimi-k2.5" :
                t("settings.modelPlaceholder")
              }
            />
          </div>

          <div className="form-group">
            <label className="label" style={{ display: "flex", alignItems: "center", gap: 8, cursor: "pointer" }}>
              <input
                type="checkbox"
                checked={!!form.vision_enabled}
                onChange={(e) => update("vision_enabled", e.target.checked)}
                style={{ width: 16, height: 16, cursor: "pointer" }}
              />
              {t("settings.visionEnabled")}
            </label>
            <p className="field-hint" style={{ marginTop: 4 }}>{t("settings.visionEnabledHint")}</p>
          </div>

          {(form.provider === "anthropic" || !form.provider) && (
            <div className="form-group">
              <label className="label">{t("settings.anthropicKey")}</label>
              <div style={{ position: "relative" }}>
                <input className="input" type={showKeys ? "text" : "password"} value={form.anthropic_api_key ?? ""}
                  onChange={(e) => update("anthropic_api_key", e.target.value)} placeholder="sk-ant-..." style={{ paddingRight: 80 }} />
                <button style={{ position: "absolute", right: 8, top: "50%", transform: "translateY(-50%)", background: "none", border: "none", color: "var(--text-muted)", cursor: "pointer", fontSize: 12 }}
                  onClick={() => setShowKeys(!showKeys)}>{showKeys ? t("common.hide") : t("common.show")}</button>
              </div>
            </div>
          )}

          {form.provider === "openai" && (
            <div className="form-group">
              <label className="label">{t("settings.openaiKey")}</label>
              <input className="input" type={showKeys ? "text" : "password"} value={form.openai_api_key ?? ""}
                onChange={(e) => update("openai_api_key", e.target.value)} placeholder="sk-..." />
            </div>
          )}

          {form.provider === "deepseek" && (
            <div className="form-group">
              <label className="label">{t("settings.deepseekKey")}</label>
              <input className="input" type={showKeys ? "text" : "password"} value={form.deepseek_api_key ?? ""}
                onChange={(e) => update("deepseek_api_key", e.target.value)} placeholder="sk-..." />
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>
                <a href="https://platform.deepseek.com" target="_blank" rel="noreferrer" style={{ color: "var(--accent)" }}>{t("settings.deepseekKeyHelp")}</a>
              </p>
            </div>
          )}

          {form.provider === "qwen" && (
            <div className="form-group">
              <label className="label">{t("settings.qwenKey")}</label>
              <input className="input" type={showKeys ? "text" : "password"} value={form.qwen_api_key ?? ""}
                onChange={(e) => update("qwen_api_key", e.target.value)} placeholder="sk-..." />
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>
                Base URL: <code>https://dashscope.aliyuncs.com/compatible-mode/v1</code>
                {" · "}<a href="https://bailian.console.aliyun.com" target="_blank" rel="noreferrer" style={{ color: "var(--accent)" }}>{t("settings.qwenKeyHelp")}</a>
              </p>
            </div>
          )}

          {form.provider === "minimax" && (
            <div className="form-group">
              <label className="label">{t("settings.minimaxKey")}</label>
              <input className="input" type={showKeys ? "text" : "password"} value={form.minimax_api_key ?? ""}
                onChange={(e) => update("minimax_api_key", e.target.value)} placeholder="eyJ..." />
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>
                Base URL: <code>https://api.minimax.io/v1</code>
                {" · "}<a href="https://platform.minimax.io" target="_blank" rel="noreferrer" style={{ color: "var(--accent)" }}>{t("settings.minimaxKeyHelp")}</a>
              </p>
            </div>
          )}

          {form.provider === "zhipu" && (
            <div className="form-group">
              <label className="label">{t("settings.zhipuKey")}</label>
              <input className="input" type={showKeys ? "text" : "password"} value={form.zhipu_api_key ?? ""}
                onChange={(e) => update("zhipu_api_key", e.target.value)} placeholder="API Key..." />
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>
                Base URL: <code>https://api.z.ai/api/paas/v4</code>
                {" · "}<a href="https://z.ai" target="_blank" rel="noreferrer" style={{ color: "var(--accent)" }}>{t("settings.zhipuKeyHelp")}</a>
              </p>
            </div>
          )}

          {form.provider === "kimi" && (
            <div className="form-group">
              <label className="label">{t("settings.kimiKey")}</label>
              <input className="input" type={showKeys ? "text" : "password"} value={form.kimi_api_key ?? ""}
                onChange={(e) => update("kimi_api_key", e.target.value)} placeholder="sk-..." />
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>
                Base URL: <code>https://api.moonshot.cn/v1</code>
                {" · "}<a href="https://platform.moonshot.cn" target="_blank" rel="noreferrer" style={{ color: "var(--accent)" }}>{t("settings.kimiKeyHelp")}</a>
              </p>
            </div>
          )}

          {form.provider === "custom" && (
            <>
              <div className="form-group">
                <label className="label">{t("settings.customApiKey")}</label>
                <input className="input" type={showKeys ? "text" : "password"} value={form.openai_api_key ?? ""}
                  onChange={(e) => update("openai_api_key", e.target.value)} placeholder="API Key" />
              </div>
              <div className="form-group">
                <label className="label">{t("settings.customBaseUrl")}</label>
                <input className="input" value={form.custom_base_url ?? ""} onChange={(e) => update("custom_base_url", e.target.value)}
                  placeholder={t("settings.customBaseUrlPlaceholder")} />
              </div>
            </>
          )}

          <div className="form-group">
            <label className="label">{t("settings.maxTokens")}</label>
            <input className="input" type="number" value={form.max_tokens ?? 4096} onChange={(e) => update("max_tokens", parseInt(e.target.value))} min={256} max={65536} />
            <span className="hint">{t("settings.maxTokensHint")}</span>
          </div>
          <div className="form-group">
            <label className="label">{t("settings.contextWindow")}</label>
            <input
              className="input"
              type="number"
              value={form.context_window ?? 0}
              onChange={(e) => update("context_window", parseInt(e.target.value) || 0)}
              min={0}
              max={2000000}
              step={1000}
            />
            <span className="hint">{t("settings.contextWindowHint")}</span>
          </div>
        </section>

        {/* Workspace */}
        <section style={{ marginBottom: 32 }}>
          <h2 style={{ fontSize: 15, fontWeight: 600, color: "var(--text-primary)", marginBottom: 16, paddingBottom: 8, borderBottom: "1px solid var(--border)" }}>
            {t("settings.workspace")}
          </h2>
          <div className="form-group">
            <label className="label">{t("settings.workspaceRoot")}</label>
            <input
              className="input"
              value={form.workspace_root ?? ""}
              onChange={(e) => update("workspace_root", e.target.value)}
              placeholder={t("settings.workspaceRootPlaceholder")}
              style={!form.allow_outside_workspace && !(form.workspace_root ?? "").trim()
                ? { borderColor: "var(--color-warning, #f59e0b)" }
                : undefined}
            />
            {!form.allow_outside_workspace && !(form.workspace_root ?? "").trim() && (
              <p style={{ fontSize: 12, color: "var(--color-warning, #f59e0b)", marginTop: 4 }}>
                {t("settings.workspaceRootRequired")}
              </p>
            )}
            {form.allow_outside_workspace && (form.workspace_root ?? "").trim() && (
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>{t("settings.workspaceRootHelp")}</p>
            )}
            {!form.allow_outside_workspace && (form.workspace_root ?? "").trim() && (
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>{t("settings.workspaceRootHelp")}</p>
            )}
          </div>
          <div className="form-group" style={{ display: "flex", alignItems: "flex-start", justifyContent: "space-between", gap: 12 }}>
            <div style={{ flex: 1 }}>
              <div style={{ fontWeight: 500, color: "var(--text-primary)" }}>{t("settings.allowOutsideWorkspace")}</div>
              <div style={{ fontSize: 12, color: "var(--text-secondary)", marginTop: 2 }}>{t("settings.allowOutsideWorkspaceDesc")}</div>
              {form.allow_outside_workspace && (
                <div style={{ fontSize: 12, color: "var(--color-warning, #f59e0b)", marginTop: 6, padding: "6px 10px", background: "rgba(245,158,11,0.08)", borderRadius: 6, border: "1px solid rgba(245,158,11,0.3)" }}>
                  {t("settings.allowOutsideWorkspaceWarning")}
                </div>
              )}
            </div>
            <input
              type="checkbox"
              checked={form.allow_outside_workspace ?? false}
              onChange={(e) => update("allow_outside_workspace", e.target.checked)}
              style={{ marginTop: 2, flexShrink: 0 }}
            />
          </div>
        </section>

        {/* Security */}
        <section style={{ marginBottom: 32 }}>
          <h2 style={{ fontSize: 15, fontWeight: 600, color: "var(--text-primary)", marginBottom: 16, paddingBottom: 8, borderBottom: "1px solid var(--border)" }}>
            {t("settings.security")}
          </h2>
          <div className="form-group" style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
            <div>
              <div style={{ fontWeight: 500, color: "var(--text-primary)" }}>{t("settings.confirmShell")}</div>
              <div style={{ fontSize: 12, color: "var(--text-secondary)" }}>{t("settings.confirmShellDesc")}</div>
            </div>
            <input type="checkbox" checked={form.confirm_shell_commands ?? true} onChange={(e) => update("confirm_shell_commands", e.target.checked)} />
          </div>
          <div className="form-group" style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
            <div>
              <div style={{ fontWeight: 500, color: "var(--text-primary)" }}>{t("settings.confirmFileWrite")}</div>
              <div style={{ fontSize: 12, color: "var(--text-secondary)" }}>{t("settings.confirmFileWriteDesc")}</div>
            </div>
            <input type="checkbox" checked={form.confirm_file_writes ?? true} onChange={(e) => update("confirm_file_writes", e.target.checked)} />
          </div>
          <div className="form-group" style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
            <div>
              <div style={{ fontWeight: 500, color: "var(--text-primary)" }}>{t("settings.browserHeadless")}</div>
              <div style={{ fontSize: 12, color: "var(--text-secondary)" }}>{t("settings.browserHeadlessDesc")}</div>
            </div>
            <input type="checkbox" checked={form.browser_headless ?? true} onChange={(e) => update("browser_headless", e.target.checked)} />
          </div>
        </section>

        {/* Agent Config */}
        <section style={{ marginBottom: 32 }}>
          <h2 style={{ fontSize: 15, fontWeight: 600, color: "var(--text-primary)", marginBottom: 16, paddingBottom: 8, borderBottom: "1px solid var(--border)" }}>
            {t("settings.agentConfig")}
          </h2>
          <div className="form-group">
            <label className="label">{t("settings.maxIterations")}</label>
            <input
              className="input"
              type="number"
              min={10}
              max={200}
              value={form.max_iterations ?? 50}
              onChange={(e) => update("max_iterations", Math.min(200, Math.max(10, Number(e.target.value))))}
            />
            <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>{t("settings.maxIterationsDesc")}</p>
          </div>
          <div className="form-group" style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
            <div>
              <div style={{ fontWeight: 500, color: "var(--text-primary)" }}>{t("settings.heartbeatEnabled")}</div>
              <div style={{ fontSize: 12, color: "var(--text-secondary)" }}>{t("settings.heartbeatEnabledDesc")}</div>
            </div>
            <input type="checkbox" checked={form.heartbeat_enabled ?? false} onChange={(e) => update("heartbeat_enabled", e.target.checked)} />
          </div>
          {form.heartbeat_enabled && (
            <>
              <div className="form-group">
                <label className="label">{t("settings.heartbeatInterval")}</label>
                <input
                  className="input"
                  type="number"
                  min={1}
                  max={1440}
                  value={form.heartbeat_interval_mins ?? 30}
                  onChange={(e) => update("heartbeat_interval_mins", Math.max(1, Number(e.target.value)))}
                />
              </div>
              <div className="form-group">
                <label className="label">{t("settings.heartbeatPrompt")}</label>
                <textarea
                  className="input"
                  rows={3}
                  value={form.heartbeat_prompt ?? ""}
                  placeholder={t("settings.heartbeatPromptPlaceholder")}
                  onChange={(e) => update("heartbeat_prompt", e.target.value)}
                  style={{ resize: "vertical", fontFamily: "inherit" }}
                />
              </div>
            </>
          )}
        </section>

        {/* Interface */}
        <section style={{ marginBottom: 32 }}>
          <h2 style={{ fontSize: 15, fontWeight: 600, color: "var(--text-primary)", marginBottom: 16, paddingBottom: 8, borderBottom: "1px solid var(--border)" }}>
            {t("settings.interface")}
          </h2>
          <div className="form-group">
            <label className="label">{t("settings.language")}</label>
            <select className="input" value={form.language ?? "zh"} onChange={(e) => update("language", e.target.value)}>
              <option value="zh">中文</option>
              <option value="en">English</option>
            </select>
          </div>

          <div className="form-group">
            <label className="label">{t("settings.theme")}</label>
            <div style={{ display: "flex", gap: 12, marginTop: 4 }}>
              {/* 紫罗兰主题卡片 */}
              <button
                onClick={() => setTheme("violet")}
                style={{
                  flex: 1,
                  padding: "14px 12px",
                  border: `2px solid ${theme === "violet" ? "#7c6af7" : "transparent"}`,
                  borderRadius: 10,
                  background: theme === "violet" ? "rgba(124,106,247,0.08)" : "#1a1a22",
                  cursor: "pointer",
                  transition: "all 0.2s",
                  outline: "none",
                  position: "relative",
                  overflow: "hidden",
                }}
              >
                {/* 色块预览 */}
                <div style={{ display: "flex", gap: 4, justifyContent: "center", marginBottom: 8 }}>
                  <div style={{ width: 18, height: 18, borderRadius: "50%", background: "#0f0f13", border: "1px solid #333345" }} />
                  <div style={{ width: 18, height: 18, borderRadius: "50%", background: "#7c6af7" }} />
                  <div style={{ width: 18, height: 18, borderRadius: "50%", background: "#9585ff" }} />
                </div>
                <div style={{ fontSize: 13, fontWeight: 600, color: theme === "violet" ? "#9585ff" : "var(--text-secondary)" }}>
                  {t("settings.themeViolet")}
                </div>
                <div style={{ fontSize: 11, color: "var(--text-muted)", marginTop: 2 }}>
                  {t("settings.themeVioletDesc")}
                </div>
                {theme === "violet" && (
                  <div style={{ position: "absolute", top: 6, right: 8, color: "#7c6af7", fontSize: 14, fontWeight: 700 }}>✓</div>
                )}
              </button>

              {/* 黑金主题卡片 */}
              <button
                onClick={() => setTheme("gold")}
                style={{
                  flex: 1,
                  padding: "14px 12px",
                  border: `2px solid ${theme === "gold" ? "#c9a84c" : "transparent"}`,
                  borderRadius: 10,
                  background: theme === "gold" ? "rgba(201,168,76,0.06)" : "#111110",
                  cursor: "pointer",
                  transition: "all 0.2s",
                  outline: "none",
                  position: "relative",
                  overflow: "hidden",
                }}
              >
                <div style={{ display: "flex", gap: 4, justifyContent: "center", marginBottom: 8 }}>
                  <div style={{ width: 18, height: 18, borderRadius: "50%", background: "#0a0a08", border: "1px solid #2a2820" }} />
                  <div style={{ width: 18, height: 18, borderRadius: "50%", background: "#c9a84c" }} />
                  <div style={{ width: 18, height: 18, borderRadius: "50%", background: "#dfc070" }} />
                </div>
                <div style={{ fontSize: 13, fontWeight: 600, color: theme === "gold" ? "#c9a84c" : "var(--text-secondary)" }}>
                  {t("settings.themeGold")}
                </div>
                <div style={{ fontSize: 11, color: "var(--text-muted)", marginTop: 2 }}>
                  {t("settings.themeGoldDesc")}
                </div>
                {theme === "gold" && (
                  <div style={{ position: "absolute", top: 6, right: 8, color: "#c9a84c", fontSize: 14, fontWeight: 700 }}>✓</div>
                )}
              </button>
            </div>
          </div>
        </section>

        {/* IM Gateway */}
        <section style={{ marginBottom: 32 }}>
          <h2 style={{ fontSize: 15, fontWeight: 600, color: "var(--text-primary)", marginBottom: 4, paddingBottom: 8, borderBottom: "1px solid var(--border)" }}>
            {t("settings.imChannels")}
          </h2>
          <p style={{ fontSize: 12, color: "var(--text-muted)", marginBottom: 16 }}>
            {t("settings.imChannelsDesc")}
          </p>

          {gatewayStatus.length > 0 && (
            <div style={{ marginBottom: 16, padding: "8px 12px", background: "var(--bg-secondary)", borderRadius: 6, fontSize: 13 }}>
              {gatewayStatus.map((ch) => (
                <div key={ch.name} style={{ display: "flex", justifyContent: "space-between", padding: "2px 0" }}>
                  <span style={{ color: "var(--text-primary)", fontWeight: 500 }}>{ch.name}</span>
                  {statusBadge(ch.status)}
                </div>
              ))}
            </div>
          )}

          {gatewayMsg && (
            <div style={{ marginBottom: 12, padding: "6px 12px", background: gatewayMsg.includes("失败") || gatewayMsg.includes("failed") || gatewayMsg.includes("Failed") ? "rgba(220,53,69,0.12)" : "rgba(40,167,69,0.12)", borderRadius: 6, fontSize: 13, color: gatewayMsg.includes("失败") || gatewayMsg.includes("failed") || gatewayMsg.includes("Failed") ? "#ff6b6b" : "#28a745" }}>
              {gatewayMsg}
            </div>
          )}

          {/* Feishu */}
          <div style={{ marginBottom: 20, padding: "14px 16px", border: "1px solid var(--border)", borderRadius: 8 }}>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: form.feishu_enabled ? 12 : 0 }}>
              <div>
                <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>{t("settings.feishu")}</span>
                <span style={{ fontSize: 12, color: "var(--text-muted)", marginLeft: 8 }}>{t("settings.feishuDesc")}</span>
              </div>
              <input type="checkbox" checked={form.feishu_enabled} onChange={(e) => update("feishu_enabled", e.target.checked)} />
            </div>
            {form.feishu_enabled && (
              <>
                <div className="form-group">
                  <label className="label">{t("settings.feishuAppId")}</label>
                  <input className="input" value={form.feishu_app_id} onChange={(e) => update("feishu_app_id", e.target.value)} placeholder="cli_xxxxxxxxxxxxxxxx" />
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.feishuAppSecret")}</label>
                  <input className="input" type={showKeys ? "text" : "password"} value={form.feishu_app_secret} onChange={(e) => update("feishu_app_secret", e.target.value)} placeholder="xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx" />
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.feishuDomain")}</label>
                  <select className="input" value={form.feishu_domain} onChange={(e) => update("feishu_domain", e.target.value)}>
                    <option value="feishu">{t("settings.feishuDomainCN")}</option>
                    <option value="lark">{t("settings.feishuDomainIntl")}</option>
                  </select>
                </div>
                <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "4px 0 0" }}>{t("settings.feishuHelp")}</p>
              </>
            )}
          </div>

          {/* WeCom */}
          <div style={{ marginBottom: 20, padding: "14px 16px", border: "1px solid var(--border)", borderRadius: 8 }}>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: form.wecom_enabled ? 12 : 0 }}>
              <div>
                <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>{t("settings.wecom")}</span>
                <span style={{ fontSize: 12, color: "var(--text-muted)", marginLeft: 8 }}>{t("settings.wecomDesc")}</span>
              </div>
              <input type="checkbox" checked={form.wecom_enabled} onChange={(e) => update("wecom_enabled", e.target.checked)} />
            </div>
            {form.wecom_enabled && (
              <>
                <div className="form-group">
                  <label className="label">{t("settings.wecomCorpId")}</label>
                  <input className="input" value={form.wecom_corp_id} onChange={(e) => update("wecom_corp_id", e.target.value)} placeholder="ww xxxxxxxxxxxxxxxx" />
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.wecomAgentId")}</label>
                  <input className="input" value={form.wecom_agent_id} onChange={(e) => update("wecom_agent_id", e.target.value)} placeholder="1000002" />
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.wecomAgentSecret")}</label>
                  <input className="input" type={showKeys ? "text" : "password"} value={form.wecom_agent_secret} onChange={(e) => update("wecom_agent_secret", e.target.value)} placeholder="xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx" />
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.wecomInboxFile")} <span style={{ fontSize: 11, color: "var(--text-muted)" }}>({t("common.optional")})</span></label>
                  <input className="input" value={form.wecom_inbox_file ?? ""} onChange={(e) => update("wecom_inbox_file", e.target.value)} placeholder="C:\pisci\wecom_inbox.jsonl" />
                  <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>{t("settings.wecomInboxFileHelp")}</p>
                </div>
                <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "4px 0 0" }}>{t("settings.wecomHelp")}</p>
              </>
            )}
          </div>

          {/* DingTalk */}
          <div style={{ marginBottom: 20, padding: "14px 16px", border: "1px solid var(--border)", borderRadius: 8 }}>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: form.dingtalk_enabled ? 12 : 0 }}>
              <div>
                <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>{t("settings.dingtalk")}</span>
                <span style={{ fontSize: 12, color: "var(--text-muted)", marginLeft: 8 }}>{t("settings.dingtalkDesc")}</span>
              </div>
              <input type="checkbox" checked={form.dingtalk_enabled} onChange={(e) => update("dingtalk_enabled", e.target.checked)} />
            </div>
            {form.dingtalk_enabled && (
              <>
                <div className="form-group">
                  <label className="label">{t("settings.dingtalkAppKey")}</label>
                  <input className="input" value={form.dingtalk_app_key} onChange={(e) => update("dingtalk_app_key", e.target.value)} placeholder="dingxxxxxxxxxxxxxxxx" />
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.dingtalkAppSecret")}</label>
                  <input className="input" type={showKeys ? "text" : "password"} value={form.dingtalk_app_secret} onChange={(e) => update("dingtalk_app_secret", e.target.value)} placeholder="xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx" />
                </div>
                <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "4px 0 0" }}>{t("settings.dingtalkHelp")}</p>
              </>
            )}
          </div>

          {/* Telegram */}
          <div style={{ marginBottom: 20, padding: "14px 16px", border: "1px solid var(--border)", borderRadius: 8 }}>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: form.telegram_enabled ? 12 : 0 }}>
              <div>
                <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>{t("settings.telegram")}</span>
                <span style={{ fontSize: 12, color: "var(--text-muted)", marginLeft: 8 }}>{t("settings.telegramDesc")}</span>
              </div>
              <input type="checkbox" checked={form.telegram_enabled} onChange={(e) => update("telegram_enabled", e.target.checked)} />
            </div>
            {form.telegram_enabled && (
              <>
                <div className="form-group">
                  <label className="label">{t("settings.telegramToken")}</label>
                  <input className="input" type={showKeys ? "text" : "password"} value={form.telegram_bot_token} onChange={(e) => update("telegram_bot_token", e.target.value)} placeholder="123456789:ABCdefGHIjklMNOpqrSTUvwxYZ" />
                </div>
                <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "4px 0 0" }}>{t("settings.telegramHelp")}</p>
              </>
            )}
          </div>

          {/* Slack */}
          <div style={{ marginBottom: 20, padding: "14px 16px", border: "1px solid var(--border)", borderRadius: 8 }}>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: form.slack_enabled ? 12 : 0 }}>
              <div>
                <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>{t("settings.slack")}</span>
                <span style={{ fontSize: 12, color: "var(--text-muted)", marginLeft: 8 }}>{t("settings.slackDesc")}</span>
              </div>
              <input type="checkbox" checked={form.slack_enabled ?? false} onChange={(e) => update("slack_enabled", e.target.checked)} />
            </div>
            {form.slack_enabled && (
              <>
                <div className="form-group">
                  <label className="label">{t("settings.slackWebhookUrl")}</label>
                  <input className="input" value={form.slack_webhook_url ?? ""} onChange={(e) => update("slack_webhook_url", e.target.value)} placeholder="https://hooks.slack.com/services/T.../B.../..." />
                </div>
                <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "4px 0 0" }}>{t("settings.slackHelp")}</p>
              </>
            )}
          </div>

          {/* Discord */}
          <div style={{ marginBottom: 20, padding: "14px 16px", border: "1px solid var(--border)", borderRadius: 8 }}>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: form.discord_enabled ? 12 : 0 }}>
              <div>
                <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>{t("settings.discord")}</span>
                <span style={{ fontSize: 12, color: "var(--text-muted)", marginLeft: 8 }}>{t("settings.discordDesc")}</span>
              </div>
              <input type="checkbox" checked={form.discord_enabled ?? false} onChange={(e) => update("discord_enabled", e.target.checked)} />
            </div>
            {form.discord_enabled && (
              <>
                <div className="form-group">
                  <label className="label">{t("settings.discordWebhookUrl")}</label>
                  <input className="input" value={form.discord_webhook_url ?? ""} onChange={(e) => update("discord_webhook_url", e.target.value)} placeholder="https://discord.com/api/webhooks/..." />
                </div>
                <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "4px 0 0" }}>{t("settings.discordHelp")}</p>
              </>
            )}
          </div>

          {/* Microsoft Teams */}
          <div style={{ marginBottom: 20, padding: "14px 16px", border: "1px solid var(--border)", borderRadius: 8 }}>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: form.teams_enabled ? 12 : 0 }}>
              <div>
                <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>{t("settings.teams")}</span>
                <span style={{ fontSize: 12, color: "var(--text-muted)", marginLeft: 8 }}>{t("settings.teamsDesc")}</span>
              </div>
              <input type="checkbox" checked={form.teams_enabled ?? false} onChange={(e) => update("teams_enabled", e.target.checked)} />
            </div>
            {form.teams_enabled && (
              <>
                <div className="form-group">
                  <label className="label">{t("settings.teamsWebhookUrl")}</label>
                  <input className="input" value={form.teams_webhook_url ?? ""} onChange={(e) => update("teams_webhook_url", e.target.value)} placeholder="https://yourorg.webhook.office.com/webhookb2/..." />
                </div>
                <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "4px 0 0" }}>{t("settings.teamsHelp")}</p>
              </>
            )}
          </div>

          {/* Matrix */}
          <div style={{ marginBottom: 20, padding: "14px 16px", border: "1px solid var(--border)", borderRadius: 8 }}>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: form.matrix_enabled ? 12 : 0 }}>
              <div>
                <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>{t("settings.matrix")}</span>
                <span style={{ fontSize: 12, color: "var(--text-muted)", marginLeft: 8 }}>{t("settings.matrixDesc")}</span>
              </div>
              <input type="checkbox" checked={form.matrix_enabled ?? false} onChange={(e) => update("matrix_enabled", e.target.checked)} />
            </div>
            {form.matrix_enabled && (
              <>
                <div className="form-group">
                  <label className="label">{t("settings.matrixHomeserver")}</label>
                  <input className="input" value={form.matrix_homeserver ?? ""} onChange={(e) => update("matrix_homeserver", e.target.value)} placeholder="https://matrix.org" />
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.matrixAccessToken")}</label>
                  <input className="input" type={showKeys ? "text" : "password"} value={form.matrix_access_token ?? ""} onChange={(e) => update("matrix_access_token", e.target.value)} placeholder="syt_..." />
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.matrixRoomId")}</label>
                  <input className="input" value={form.matrix_room_id ?? ""} onChange={(e) => update("matrix_room_id", e.target.value)} placeholder="!roomid:matrix.org" />
                </div>
                <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "4px 0 0" }}>{t("settings.matrixHelp")}</p>
              </>
            )}
          </div>

          {/* Generic Webhook */}
          <div style={{ marginBottom: 20, padding: "14px 16px", border: "1px solid var(--border)", borderRadius: 8 }}>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: form.webhook_enabled ? 12 : 0 }}>
              <div>
                <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>{t("settings.webhook")}</span>
                <span style={{ fontSize: 12, color: "var(--text-muted)", marginLeft: 8 }}>{t("settings.webhookDesc")}</span>
              </div>
              <input type="checkbox" checked={form.webhook_enabled ?? false} onChange={(e) => update("webhook_enabled", e.target.checked)} />
            </div>
            {form.webhook_enabled && (
              <>
                <div className="form-group">
                  <label className="label">{t("settings.webhookOutboundUrl")}</label>
                  <input className="input" value={form.webhook_outbound_url ?? ""} onChange={(e) => update("webhook_outbound_url", e.target.value)} placeholder="https://your-service.example.com/webhook" />
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.webhookAuthToken")} <span style={{ fontSize: 11, color: "var(--text-muted)" }}>({t("common.optional")})</span></label>
                  <input className="input" type={showKeys ? "text" : "password"} value={form.webhook_auth_token ?? ""} onChange={(e) => update("webhook_auth_token", e.target.value)} placeholder="Bearer token or API key" />
                </div>
                <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "4px 0 0" }}>{t("settings.webhookHelp")}</p>
              </>
            )}
          </div>

          <div style={{ display: "flex", gap: 8 }}>
            <button className="btn btn-primary" onClick={handleGatewayConnect} disabled={gatewayConnecting || gatewayDisconnecting}>
              {gatewayConnecting ? t("common.connecting") : t("settings.connectChannels")}
            </button>
            <button
              className="btn"
              onClick={handleGatewayDisconnect}
              disabled={
                gatewayDisconnecting ||
                gatewayConnecting ||
                !gatewayStatus.some((ch) => ch.status === "Connected" || ch.status === "Connecting")
              }
              style={{ border: "1px solid var(--border)" }}
            >
              {gatewayDisconnecting ? t("common.disconnecting") : t("settings.disconnectAll")}
            </button>
            <button className="btn" onClick={() => setShowKeys(!showKeys)} style={{ background: "none", border: "1px solid var(--border)", color: "var(--text-muted)", fontSize: 12 }}>
              {showKeys ? t("common.hideKeys") : t("common.showKeys")}
            </button>
          </div>
        </section>

        {/* ── Email ─────────────────────────────────────────────────────── */}
        <section className="settings-section">
          <h3 className="settings-section-title">{t("settings.emailSection")}</h3>
          <p style={{ fontSize: 13, color: "var(--text-muted)", marginBottom: 16 }}>
            {t("settings.emailSectionDesc")}
          </p>

          <div style={{ padding: "14px 16px", border: "1px solid var(--border)", borderRadius: 8 }}>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: form.email_enabled ? 16 : 0 }}>
              <div>
                <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>{t("settings.emailEnabled")}</span>
                <span style={{ fontSize: 12, color: "var(--text-muted)", marginLeft: 8 }}>{t("settings.emailEnabledDesc")}</span>
              </div>
              <input type="checkbox" checked={form.email_enabled} onChange={(e) => update("email_enabled", e.target.checked)} />
            </div>

            {form.email_enabled && (
              <>
                <div style={{ display: "grid", gridTemplateColumns: "1fr auto", gap: 8, marginBottom: 12 }}>
                  <div className="form-group" style={{ marginBottom: 0 }}>
                    <label className="label">{t("settings.smtpHost")}</label>
                    <input className="input" value={form.smtp_host} onChange={(e) => update("smtp_host", e.target.value)} placeholder="smtp.gmail.com" />
                  </div>
                  <div className="form-group" style={{ marginBottom: 0, width: 90 }}>
                    <label className="label">{t("settings.smtpPort")}</label>
                    <input className="input" type="number" value={form.smtp_port} onChange={(e) => update("smtp_port", parseInt(e.target.value) || 587)} placeholder="587" />
                  </div>
                </div>

                <div className="form-group">
                  <label className="label">{t("settings.smtpUsername")}</label>
                  <input className="input" value={form.smtp_username} onChange={(e) => update("smtp_username", e.target.value)} placeholder="you@gmail.com" />
                </div>

                <div className="form-group">
                  <label className="label">{t("settings.smtpPassword")}</label>
                  <input className="input" type={showKeys ? "text" : "password"} value={form.smtp_password} onChange={(e) => update("smtp_password", e.target.value)} placeholder={t("settings.smtpPasswordPlaceholder")} />
                  <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>{t("settings.smtpPasswordHelp")}</p>
                </div>

                <div className="form-group">
                  <label className="label">{t("settings.smtpFromName")} <span style={{ fontSize: 11, color: "var(--text-muted)" }}>({t("common.optional")})</span></label>
                  <input className="input" value={form.smtp_from_name} onChange={(e) => update("smtp_from_name", e.target.value)} placeholder="Pisci Agent" />
                </div>

                <div style={{ borderTop: "1px solid var(--border)", margin: "12px 0" }} />

                <div style={{ display: "grid", gridTemplateColumns: "1fr auto", gap: 8 }}>
                  <div className="form-group" style={{ marginBottom: 0 }}>
                    <label className="label">{t("settings.imapHost")} <span style={{ fontSize: 11, color: "var(--text-muted)" }}>({t("common.optional")})</span></label>
                    <input className="input" value={form.imap_host} onChange={(e) => update("imap_host", e.target.value)} placeholder="imap.gmail.com" />
                  </div>
                  <div className="form-group" style={{ marginBottom: 0, width: 90 }}>
                    <label className="label">{t("settings.imapPort")}</label>
                    <input className="input" type="number" value={form.imap_port} onChange={(e) => update("imap_port", parseInt(e.target.value) || 993)} placeholder="993" />
                  </div>
                </div>
                <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 6 }}>{t("settings.imapHelp")}</p>
              </>
            )}
          </div>
        </section>

        {/* ── SSH Servers ───────────────────────────────────────────────── */}
        <section className="settings-section">
          <h3 className="settings-section-title">{t("settings.sshSection")}</h3>
          <p style={{ fontSize: 13, color: "var(--text-muted)", marginBottom: 16 }}>
            {t("settings.sshSectionDesc")}
          </p>

          {/* Server list */}
          {sshServers.length > 0 && (
            <div style={{ display: "flex", flexDirection: "column", gap: 8, marginBottom: 12 }}>
              {sshServers.map((srv, idx) => (
                <div key={srv.id} style={{ display: "flex", alignItems: "center", gap: 10, padding: "10px 14px", border: "1px solid var(--border)", borderRadius: 8, background: "var(--bg-secondary)" }}>
                  <span style={{ fontSize: 16 }}>🖥️</span>
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ fontWeight: 600, color: "var(--text-primary)", fontSize: 13 }}>
                      {srv.label || srv.id}
                      <span style={{ fontWeight: 400, color: "var(--text-muted)", fontSize: 11, marginLeft: 8 }}>
                        [{srv.id}]
                      </span>
                    </div>
                    <div style={{ fontSize: 11, color: "var(--text-muted)" }}>
                      {srv.username}@{srv.host}:{srv.port}
                      {" · "}
                      {srv.password ? t("settings.sshAuthPassword") : srv.private_key ? t("settings.sshAuthKey") : t("settings.sshAuthNone")}
                    </div>
                  </div>
                  <div style={{ display: "flex", gap: 6 }}>
                    <button className="btn" style={{ fontSize: 11, padding: "3px 10px", border: "1px solid var(--border)" }}
                      onClick={() => { setSshEditIdx(idx); setSshEditForm({ ...srv, password: "", private_key: "" }); setSshShowPassword(false); }}>
                      {t("common.edit")}
                    </button>
                    <button className="btn" style={{ fontSize: 11, padding: "3px 10px", border: "1px solid #dc3545", color: "#dc3545" }}
                      onClick={() => setSshServers(prev => prev.filter((_, i) => i !== idx))}>
                      {t("common.delete")}
                    </button>
                  </div>
                </div>
              ))}
            </div>
          )}

          {/* Add / Edit form */}
          {sshEditIdx !== null ? (
            <div style={{ padding: 16, border: "1px solid var(--border)", borderRadius: 8, background: "var(--bg-secondary)" }}>
              <div style={{ fontWeight: 600, marginBottom: 12, fontSize: 13 }}>
                {sshEditIdx === -1 ? t("settings.sshAddServer") : t("settings.sshEditServer")}
              </div>
              <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 10 }}>
                <div className="form-group" style={{ marginBottom: 0 }}>
                  <label className="label">{t("settings.sshId")} *</label>
                  <input className="input" value={sshEditForm.id} onChange={e => setSshEditForm(f => ({ ...f, id: e.target.value }))} placeholder="prod" />
                </div>
                <div className="form-group" style={{ marginBottom: 0 }}>
                  <label className="label">{t("settings.sshLabel")}</label>
                  <input className="input" value={sshEditForm.label} onChange={e => setSshEditForm(f => ({ ...f, label: e.target.value }))} placeholder={t("settings.sshLabelPlaceholder")} />
                </div>
                <div className="form-group" style={{ marginBottom: 0 }}>
                  <label className="label">{t("settings.sshHost")} *</label>
                  <input className="input" value={sshEditForm.host} onChange={e => setSshEditForm(f => ({ ...f, host: e.target.value }))} placeholder="192.168.1.100" />
                </div>
                <div className="form-group" style={{ marginBottom: 0 }}>
                  <label className="label">{t("settings.sshPort")}</label>
                  <input className="input" type="number" value={sshEditForm.port} onChange={e => setSshEditForm(f => ({ ...f, port: parseInt(e.target.value) || 22 }))} />
                </div>
                <div className="form-group" style={{ marginBottom: 0 }}>
                  <label className="label">{t("settings.sshUsername")} *</label>
                  <input className="input" value={sshEditForm.username} onChange={e => setSshEditForm(f => ({ ...f, username: e.target.value }))} placeholder="root" />
                </div>
                <div className="form-group" style={{ marginBottom: 0 }}>
                  <label className="label">
                    {t("settings.sshPassword")}
                    <span style={{ fontSize: 11, color: "var(--text-muted)", marginLeft: 6 }}>{t("settings.sshPasswordHint")}</span>
                  </label>
                  <div style={{ display: "flex", gap: 6 }}>
                    <input className="input" style={{ flex: 1 }} type={sshShowPassword ? "text" : "password"} value={sshEditForm.password} onChange={e => setSshEditForm(f => ({ ...f, password: e.target.value }))} placeholder={sshEditIdx !== -1 ? t("settings.sshPasswordKeep") : ""} />
                    <button className="btn" style={{ padding: "0 10px", border: "1px solid var(--border)" }} onClick={() => setSshShowPassword(v => !v)}>
                      {sshShowPassword ? "🙈" : "👁️"}
                    </button>
                  </div>
                </div>
              </div>
              <div className="form-group" style={{ marginTop: 10, marginBottom: 0 }}>
                <label className="label">
                  {t("settings.sshPrivateKey")}
                  <span style={{ fontSize: 11, color: "var(--text-muted)", marginLeft: 6 }}>{t("settings.sshPrivateKeyHint")}</span>
                </label>
                <textarea className="input" rows={3} style={{ fontFamily: "monospace", fontSize: 11 }} value={sshEditForm.private_key} onChange={e => setSshEditForm(f => ({ ...f, private_key: e.target.value }))} placeholder="-----BEGIN OPENSSH PRIVATE KEY-----&#10;...&#10;-----END OPENSSH PRIVATE KEY-----" />
              </div>
              <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
                <button className="btn btn-primary" style={{ fontSize: 12 }}
                  onClick={() => {
                    if (!sshEditForm.id.trim() || !sshEditForm.host.trim() || !sshEditForm.username.trim()) return;
                    if (sshEditIdx === -1) {
                      setSshServers(prev => [...prev, sshEditForm]);
                    } else {
                      setSshServers(prev => prev.map((s, i) => i === sshEditIdx ? sshEditForm : s));
                    }
                    setSshEditIdx(null);
                  }}>
                  {t("common.save")}
                </button>
                <button className="btn" style={{ fontSize: 12, border: "1px solid var(--border)" }} onClick={() => setSshEditIdx(null)}>
                  {t("common.cancel")}
                </button>
              </div>
            </div>
          ) : (
            <button className="btn" style={{ fontSize: 12, padding: "6px 14px", border: "1px solid var(--border)" }}
              onClick={() => { setSshEditIdx(-1); setSshEditForm({ id: "", label: "", host: "", port: 22, username: "", password: "", private_key: "" }); setSshShowPassword(false); }}>
              + {t("settings.sshAddServer")}
            </button>
          )}
        </section>

        {/* ── Runtime Environment ───────────────────────────────────────── */}
        <section className="settings-section">
          <h3 className="settings-section-title">{t("settings.runtimeSection")}</h3>
          <p style={{ fontSize: 13, color: "var(--text-muted)", marginBottom: 16 }}>
            {t("settings.runtimeSectionDesc")}
          </p>

          <button
            className="btn"
            onClick={handleCheckRuntimes}
            disabled={runtimesLoading}
            style={{ marginBottom: 16, border: "1px solid var(--border)", fontSize: 13 }}
          >
            {runtimesLoading ? t("common.loading") : t("settings.checkRuntimes")}
          </button>

          {runtimes.length > 0 && (
            <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              {runtimes.map((item) => {
                // Keys must match what the backend uses in runtime_paths HashMap
                const runtimeKey = item.name === "Node.js" ? "node"
                  : item.name === "npm" ? "npm"
                  : item.name === "Python" ? "python"
                  : item.name === "pip" ? "pip"
                  : item.name === "Git" ? "git"
                  : item.name.toLowerCase();
                const isSetting = runtimesSettingKey === runtimeKey;
                return (
                  <div
                    key={item.name}
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 10,
                      padding: "10px 14px",
                      border: "1px solid var(--border)",
                      borderRadius: 8,
                      background: "var(--bg-secondary)",
                    }}
                  >
                    <span style={{ fontSize: 16, flexShrink: 0 }}>
                      {item.available ? "✅" : "❌"}
                    </span>
                    <div style={{ flex: 1, minWidth: 0 }}>
                      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                        <span style={{ fontWeight: 600, color: "var(--text-primary)", fontSize: 13 }}>
                          {item.name}
                        </span>
                        {item.available && item.version && (
                          <span style={{ fontSize: 11, color: "var(--text-muted)" }}>
                            {item.version}
                          </span>
                        )}
                        {!item.available && (
                          <span style={{ fontSize: 11, color: "#dc3545" }}>
                            {t("settings.runtimeNotFound")}
                          </span>
                        )}
                      </div>
                      <div style={{ fontSize: 11, color: "var(--text-muted)", marginTop: 2 }}>
                        {item.hint}
                      </div>
                    </div>
                    <div style={{ display: "flex", gap: 6, flexShrink: 0 }}>
                      <button
                        className="btn"
                        onClick={() => handleSelectRuntimePath(runtimeKey, item.name)}
                        disabled={isSetting}
                        title={t("settings.runtimeSelectPath")}
                        style={{ fontSize: 11, padding: "3px 10px", border: "1px solid var(--border)" }}
                      >
                        {isSetting ? "…" : t("settings.runtimeSelectPath")}
                      </button>
                      {!item.available && (
                        <button
                          className="btn btn-primary"
                          onClick={() => openUrl(item.download_url).catch(() => window.open(item.download_url, "_blank"))}
                          style={{ fontSize: 11, padding: "3px 10px" }}
                        >
                          {t("settings.runtimeDownload")}
                        </button>
                      )}
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </section>
      </div>
    </div>
  );
}
