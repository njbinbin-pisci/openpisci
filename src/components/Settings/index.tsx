import { useState, useEffect, useCallback } from "react";
import { useDispatch, useSelector } from "react-redux";
import { useTranslation } from "react-i18next";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { open as openUrl } from "@tauri-apps/plugin-shell";
import { RootState, settingsActions } from "../../store";
import {
  settingsApi,
  gatewayApi,
  systemApi,
  wechatApi,
  enterpriseCapabilityApi,
  Settings as SettingsData,
  ChannelInfo,
  RuntimeCheckItem,
  SshServerConfig,
  LlmProviderConfig,
  EnterpriseCapabilityStatus,
  EnterpriseCapabilityTestResult,
} from "../../services/tauri";
import { setLanguage } from "../../i18n";

type EnterprisePlatformId = "feishu" | "wecom" | "dingtalk";

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
  wecom_bot_id: "",
  wecom_bot_secret: "",
  wecom_enabled: false,
  wechat_enabled: false,
  wechat_gateway_token: "",
  wechat_gateway_port: 18789,
  wechat_bot_token: "",
  wechat_base_url: "",
  wechat_bot_id: "",
  dingtalk_app_key: "",
  dingtalk_app_secret: "",
  dingtalk_robot_code: "",
  dingtalk_corp_id: "",
  dingtalk_agent_id: "",
  dingtalk_mcp_url: "",
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
  auto_compact_input_tokens_threshold: 200000,
  compaction_micro_percent: 60,
  compaction_auto_percent: 80,
  compaction_full_percent: 95,
  max_tool_result_tokens: 8000,
  summary_model: "",
  project_instruction_budget_chars: 8000,
  enable_project_instructions: true,
  llm_read_timeout_secs: 120,
  koi_timeout_secs: 600,
  heartbeat_enabled: false,
  heartbeat_interval_mins: 30,
  heartbeat_prompt: "检查是否有待处理任务，如无则回复 HEARTBEAT_OK",
  vision_enabled: false,
  enable_streaming: false,
  ssh_servers: [],
  allow_multiple_instances: false,
};

interface SettingsProps {
  theme: 'violet' | 'gold';
  setTheme: (t: 'violet' | 'gold') => void;
  onOpenTools?: () => void;
}

export default function Settings({ theme, setTheme, onOpenTools }: SettingsProps) {
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
  const [defaultWorkspace, setDefaultWorkspace] = useState<string>("");
  const [capabilityStatus, setCapabilityStatus] = useState<Partial<Record<EnterprisePlatformId, EnterpriseCapabilityStatus>>>({});
  const [capabilityLoading, setCapabilityLoading] = useState<Partial<Record<EnterprisePlatformId, boolean>>>({});
  const [capabilityTesting, setCapabilityTesting] = useState<Partial<Record<EnterprisePlatformId, boolean>>>({});
  const [capabilityTest, setCapabilityTest] = useState<Partial<Record<EnterprisePlatformId, EnterpriseCapabilityTestResult | null>>>({});
  const [capabilityMsg, setCapabilityMsg] = useState<Partial<Record<EnterprisePlatformId, string | null>>>({});

  // WeChat binding flow
  const [wechatQr, setWechatQr] = useState<string | null>(null);
  const [wechatBindState, setWechatBindState] = useState<"idle" | "loading" | "scan" | "scaned" | "success" | "error">("idle");
  const [wechatBindError, setWechatBindError] = useState<string | null>(null);

  const handleWechatBind = async () => {
    setWechatBindState("loading");
    setWechatQr(null);
    setWechatBindError(null);
    try {
      const result = await wechatApi.startLogin();
      if (result.connected) {
        setWechatBindState("success");
      } else if (result.qr_data_url && result.qrcode_token) {
        setWechatQr(result.qr_data_url);
        setWechatBindState("scan");
        // Start polling for scan status
        pollWechatStatus(result.qrcode_token);
      } else {
        setWechatBindState("error");
        setWechatBindError(result.message);
      }
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : String(e);
      setWechatBindState("error");
      setWechatBindError(msg);
    }
  };

  const pollWechatStatus = async (token: string) => {
    const maxAttempts = 150; // ~5 minutes at 2s intervals
    for (let i = 0; i < maxAttempts; i++) {
      await new Promise(r => setTimeout(r, 2000));
      try {
        const status = await wechatApi.pollLogin(token);
        if (status.connected) {
          setWechatBindState("success");
          setWechatQr(null);
          // Auto-connect gateway so the WeChat listener starts immediately after binding.
          try {
            const r = await gatewayApi.connect();
            setGatewayStatus(r.channels);
          } catch {
            // Non-fatal: user can still click "Connect" manually.
          }
          return;
        }
        if (status.message === "scaned") {
          setWechatBindState("scaned");
        }
        if (status.message === "expired") {
          setWechatBindState("error");
          setWechatBindError(t("settings.wechatQrExpired"));
          return;
        }
      } catch {
        // network hiccup, keep polling
      }
    }
    setWechatBindState("error");
    setWechatBindError(t("settings.wechatBindFailed"));
  };

  // SSH Servers
  const [sshServers, setSshServers] = useState<SshServerConfig[]>([]);
  const [sshEditIdx, setSshEditIdx] = useState<number | null>(null);
  const [sshEditForm, setSshEditForm] = useState<SshServerConfig>({ id: "", label: "", host: "", port: 22, username: "", password: "", private_key: "" });
  const [sshShowPassword, setSshShowPassword] = useState(false);

  // Named LLM Providers
  const EMPTY_LLM_PROVIDER: LlmProviderConfig = { id: "", label: "", provider: "anthropic", model: "", api_key: "", base_url: "", max_tokens: 0 };
  const [llmProviders, setLlmProviders] = useState<LlmProviderConfig[]>([]);
  const [llmEditIdx, setLlmEditIdx] = useState<number | null>(null);
  const [llmEditForm, setLlmEditForm] = useState<LlmProviderConfig>(EMPTY_LLM_PROVIDER);
  const [llmShowKey, setLlmShowKey] = useState(false);

  // Load default workspace path from backend on mount
  useEffect(() => {
    settingsApi.getDefaultWorkspace().then(setDefaultWorkspace).catch(() => {});
  }, []);

  useEffect(() => {
    if (settings) {
      const merged = { ...DEFAULT_SETTINGS, ...settings };
      // If workspace_root is empty, fill with default
      if (!merged.workspace_root?.trim() && defaultWorkspace) {
        merged.workspace_root = defaultWorkspace;
      }
      setForm(merged);
      setSshServers(settings.ssh_servers ?? []);
      setLlmProviders(settings.llm_providers ?? []);
    }
  }, [settings, defaultWorkspace]);

  // Refresh gateway status on mount and whenever settings change (catches post-restart state)
  useEffect(() => {
    gatewayApi.list().then((r) => setGatewayStatus(r.channels)).catch(() => setGatewayStatus([]));
  }, [settings]);

  const refreshCapabilityStatus = useCallback(async (platform: EnterprisePlatformId) => {
    try {
      const status = await enterpriseCapabilityApi.status(platform);
      setCapabilityStatus((prev) => ({ ...prev, [platform]: status }));
    } catch (e) {
      setCapabilityMsg((prev) => ({ ...prev, [platform]: t("settings.enterpriseCapabilityStatusFailed", { error: String(e) }) }));
    }
  }, [t]);

  useEffect(() => {
    (["feishu", "wecom", "dingtalk"] as EnterprisePlatformId[]).forEach((platform) => {
      refreshCapabilityStatus(platform);
    });
  }, [refreshCapabilityStatus, settings]);

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

  const handleEnableCapability = async (platform: EnterprisePlatformId) => {
    setCapabilityLoading((prev) => ({ ...prev, [platform]: true }));
    setCapabilityMsg((prev) => ({ ...prev, [platform]: null }));
    setCapabilityTest((prev) => ({ ...prev, [platform]: null }));
    try {
      const status = await enterpriseCapabilityApi.enable(platform);
      setCapabilityStatus((prev) => ({ ...prev, [platform]: status }));
      setCapabilityMsg((prev) => ({ ...prev, [platform]: t("settings.enterpriseCapabilityEnabled") }));
    } catch (e) {
      setCapabilityMsg((prev) => ({ ...prev, [platform]: t("settings.enterpriseCapabilityEnableFailed", { error: String(e) }) }));
    } finally {
      setCapabilityLoading((prev) => ({ ...prev, [platform]: false }));
    }
  };

  const handleTestCapability = async (platform: EnterprisePlatformId) => {
    setCapabilityTesting((prev) => ({ ...prev, [platform]: true }));
    setCapabilityMsg((prev) => ({ ...prev, [platform]: null }));
    try {
      const result = await enterpriseCapabilityApi.test(platform);
      setCapabilityStatus((prev) => ({ ...prev, [platform]: result.status }));
      setCapabilityTest((prev) => ({ ...prev, [platform]: result }));
      setCapabilityMsg((prev) => ({ ...prev, [platform]: result.success
        ? t("settings.enterpriseCapabilityTestSuccess", { count: result.tools.length })
        : t("settings.enterpriseCapabilityTestFailed", { error: result.error ?? "unknown" })
      }));
    } catch (e) {
      setCapabilityMsg((prev) => ({ ...prev, [platform]: t("settings.enterpriseCapabilityTestFailed", { error: String(e) }) }));
    } finally {
      setCapabilityTesting((prev) => ({ ...prev, [platform]: false }));
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

  const statusBadge = (s: ChannelInfo["status"]) => {
    if (s === "Connected") return <span style={{ color: "#28a745", fontWeight: 600 }}>● {t("common.connected")}</span>;
    if (s === "Connecting") return <span style={{ color: "#ffc107", fontWeight: 600 }}>● {t("common.connecting")}</span>;
    if (s === "Disconnected") return <span style={{ color: "var(--text-muted)" }}>● {t("common.disconnected")}</span>;
    if (typeof s === "object" && "Error" in s) return <span style={{ color: "#dc3545" }}>● {t("common.error")}: {s.Error}</span>;
    return null;
  };

  const handleSave = async () => {
    // workspace_root is always required — fill with default if blank
    if (!(form.workspace_root ?? "").trim()) {
      if (defaultWorkspace) {
        update("workspace_root", defaultWorkspace);
      } else {
        setSaveError(t("settings.workspaceRootRequired"));
        return;
      }
    }
    const micro = Number(form.compaction_micro_percent ?? 60);
    const auto = Number(form.compaction_auto_percent ?? 80);
    const full = Number(form.compaction_full_percent ?? 95);
    if (!(micro >= 0 && micro < auto && auto < full && full < 100)) {
      setSaveError(t("settings.compactionThresholdOrderError"));
      return;
    }
    setSaving(true);
    setSaveError(null);
    try {
      const updated = await settingsApi.save({
        ...form,
        compaction_micro_percent: micro,
        compaction_auto_percent: auto,
        compaction_full_percent: full,
        max_tool_result_tokens: Math.max(1000, Number(form.max_tool_result_tokens) || 8000),
        summary_model: (form.summary_model ?? "").trim() || null,
        ssh_servers: sshServers,
        llm_providers: llmProviders,
      });
      dispatch(settingsActions.setSettings(updated));
      dispatch(settingsActions.setConfigured(updated.is_configured ?? !!(updated.anthropic_api_key || updated.openai_api_key || updated.deepseek_api_key || updated.qwen_api_key)));
      // 立即切换语言
      if (updated.language) setLanguage(updated.language as "zh" | "en");
      (["feishu", "wecom", "dingtalk"] as EnterprisePlatformId[]).forEach((platform) => {
        refreshCapabilityStatus(platform);
      });
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

  const capabilityStatusMessage = (platform: EnterprisePlatformId, status?: EnterpriseCapabilityStatus | null) => {
    if (!status) return t("settings.enterpriseCapabilityLoading");
    if (!status.configured) {
      return platform === "dingtalk"
        ? t("settings.dingtalkMcpUrlMissing")
        : t("settings.enterpriseCapabilityStatusNeedsConfig");
    }
    if (platform === "wecom") return t("settings.wecomEnterpriseStatusReady");
    if (!status.mcp_configured) return t("settings.enterpriseCapabilityStatusReadyToEnable");
    if (!status.mcp_enabled) return t("settings.enterpriseCapabilityStatusDisabled");
    return t("settings.enterpriseCapabilityStatusActive");
  };

  const capabilityDiagnostics = (platform: EnterprisePlatformId, test?: EnterpriseCapabilityTestResult | null) => {
    if (!test) return [];
    if (test.success) {
      if (platform === "wecom") return [t("settings.wecomEnterpriseTestSuccessHint")];
      if (platform === "dingtalk") return [t("settings.dingtalkEnterpriseTestSuccessHint")];
      return [t("settings.feishuEnterpriseTestSuccessHint")];
    }
    if (platform === "wecom") return [t("settings.wecomEnterpriseTestFailedHint")];
    if (platform === "dingtalk") return [t("settings.dingtalkEnterpriseTestFailedHint")];
    return [t("settings.feishuEnterpriseTestFailedHint")];
  };

  const capabilityMissingCredentialsMessage = (platform: EnterprisePlatformId, status: EnterpriseCapabilityStatus) => {
    if (!status.missing_credentials.length) return null;
    if (platform === "wecom") return t("settings.wecomEnterpriseMissingCreds");
    if (platform === "dingtalk") return t("settings.dingtalkMcpUrlMissing");
    if (platform === "feishu") return t("settings.feishuEnterpriseMissingCreds");
    return t("settings.enterpriseCapabilityMissingCreds", { fields: status.missing_credentials.join(", ") });
  };

  const renderEnterpriseCapabilityPanel = (
    platform: EnterprisePlatformId,
    title: string,
    description: string,
    options: { showOpenTools?: boolean; showEnable?: boolean } = {}
  ) => {
    const status = capabilityStatus[platform];
    const loading = !!capabilityLoading[platform];
    const testing = !!capabilityTesting[platform];
    const test = capabilityTest[platform];
    const msg = capabilityMsg[platform];
    const diagnostics = capabilityDiagnostics(platform, test);
    const showEnable = options.showEnable !== false;
    return (
      <>
        <div style={{ fontSize: 11, fontWeight: 600, color: "var(--text-muted)", textTransform: "uppercase", letterSpacing: 0.5, marginTop: 12, marginBottom: 4 }}>
          {t("settings.imCapabilityLabel")}
        </div>
        <div style={{ padding: "10px 12px", border: "1px solid var(--border)", borderRadius: 8, background: "var(--bg-secondary)" }}>
          <div style={{ display: "flex", justifyContent: "space-between", gap: 12, alignItems: "flex-start" }}>
            <div>
              <div style={{ fontSize: 13, fontWeight: 600, color: "var(--text-primary)" }}>{title}</div>
              <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "4px 0 0", lineHeight: 1.6 }}>{description}</p>
            </div>
            <span style={{
              fontSize: 11,
              padding: "2px 8px",
              borderRadius: 999,
              color: status?.enabled ? "#28a745" : status?.configured ? "#ffc107" : "#dc3545",
              background: "rgba(255,255,255,0.04)",
              whiteSpace: "nowrap",
            }}>
              {status?.enabled
                ? t("settings.enterpriseCapabilityStatusEnabled")
                : status?.configured
                  ? t("settings.enterpriseCapabilityStatusReady")
                  : t("settings.enterpriseCapabilityStatusMissing")}
            </span>
          </div>
          <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "8px 0 0", lineHeight: 1.6 }}>
            {capabilityStatusMessage(platform, status)}
          </p>
          {status?.missing_credentials.length ? (
            <p style={{ fontSize: 12, color: "#dc3545", margin: "6px 0 0", lineHeight: 1.5 }}>
              {capabilityMissingCredentialsMessage(platform, status)}
            </p>
          ) : null}
          {msg && (
            <div style={{
              marginTop: 8,
              fontSize: 12,
              lineHeight: 1.5,
              padding: "6px 8px",
              borderRadius: 6,
              color: test?.success ? "#28a745" : msg.includes("失败") || msg.toLowerCase().includes("failed") ? "#dc3545" : "var(--text-primary)",
              background: test?.success ? "rgba(40,167,69,0.12)" : "rgba(255,255,255,0.04)",
            }}>
              {msg}
            </div>
          )}
          {diagnostics.length ? (
            <ul style={{ margin: "8px 0 0 18px", padding: 0, fontSize: 12, color: "var(--text-muted)", lineHeight: 1.6 }}>
              {diagnostics.map((item, idx) => (
                <li key={idx}>{item}</li>
              ))}
            </ul>
          ) : null}
          {test?.tools?.length ? (
            <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "8px 0 0", lineHeight: 1.5 }}>
              {t("settings.enterpriseCapabilityToolsPreview", {
                count: test.tools.length,
                tools: test.tools.slice(0, 5).map(tool => tool.name).join(", "),
              })}
            </p>
          ) : null}
          <div style={{ display: "flex", gap: 8, marginTop: 10, flexWrap: "wrap" }}>
            {showEnable && (
              <button className="btn btn-secondary" type="button" onClick={() => handleEnableCapability(platform)} disabled={loading || !status?.configured}>
                {loading ? t("common.loading") : t("settings.enterpriseCapabilityEnable")}
              </button>
            )}
            <button className="btn btn-secondary" type="button" onClick={() => handleTestCapability(platform)} disabled={testing || !status?.configured}>
              {testing ? t("settings.enterpriseCapabilityTesting") : t("settings.enterpriseCapabilityTest")}
            </button>
            {options.showOpenTools && onOpenTools && (
              <button className="btn btn-secondary" type="button" onClick={onOpenTools}>
                {t("settings.enterpriseCapabilityOpenTools")}
              </button>
            )}
          </div>
        </div>
      </>
    );
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

          <div className="form-group">
            <label className="label" style={{ display: "flex", alignItems: "center", gap: 8, cursor: "pointer" }}>
              <input
                type="checkbox"
                checked={!!form.enable_streaming}
                onChange={(e) => update("enable_streaming", e.target.checked)}
                style={{ width: 16, height: 16, cursor: "pointer" }}
              />
              {t("settings.enableStreaming")}
            </label>
            <p className="field-hint" style={{ marginTop: 4 }}>{t("settings.enableStreamingHint")}</p>
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
                <p className="field-hint" style={{ marginTop: 4 }}>{t("settings.customApiKeyHint")}</p>
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

          {/* Named LLM Providers — lives inside AI Provider section */}
          <div style={{ marginTop: 24, paddingTop: 20, borderTop: "1px solid var(--border)" }}>
            <div style={{ fontWeight: 600, fontSize: 13, color: "var(--text-primary)", marginBottom: 6 }}>
              🔑 多供应商配置
            </div>
            <p style={{ fontSize: 12, color: "var(--text-muted)", marginBottom: 12 }}>
              配置多个命名 LLM 供应商，每个 Koi 可在编辑界面独立选择。未选择时使用上方全局默认配置。
            </p>

            {llmProviders.length > 0 && (
              <div style={{ display: "flex", flexDirection: "column", gap: 8, marginBottom: 12 }}>
                {llmProviders.map((p, idx) => (
                  <div key={p.id} style={{ display: "flex", alignItems: "center", gap: 10, padding: "10px 14px", border: "1px solid var(--border)", borderRadius: 8, background: "var(--bg-secondary)" }}>
                    <span style={{ fontSize: 16 }}>🔑</span>
                    <div style={{ flex: 1, minWidth: 0 }}>
                      <div style={{ fontWeight: 600, color: "var(--text-primary)", fontSize: 13 }}>
                        {p.label || p.id}
                        <span style={{ fontWeight: 400, color: "var(--text-muted)", fontSize: 11, marginLeft: 8 }}>[{p.id}]</span>
                      </div>
                      <div style={{ fontSize: 11, color: "var(--text-muted)" }}>
                        {p.provider} · {p.model || "（未设置模型）"}
                        {p.base_url ? ` · ${p.base_url}` : ""}
                      </div>
                    </div>
                    <div style={{ display: "flex", gap: 6 }}>
                      <button className="btn" style={{ fontSize: 11, padding: "3px 10px", border: "1px solid var(--border)" }}
                        onClick={() => { setLlmEditIdx(idx); setLlmEditForm({ ...p, api_key: "" }); setLlmShowKey(false); }}>
                        编辑
                      </button>
                      <button className="btn" style={{ fontSize: 11, padding: "3px 10px", border: "1px solid #dc3545", color: "#dc3545" }}
                        onClick={() => setLlmProviders(prev => prev.filter((_, i) => i !== idx))}>
                        删除
                      </button>
                    </div>
                  </div>
                ))}
              </div>
            )}

            {llmEditIdx !== null ? (
              <div style={{ padding: 16, border: "1px solid var(--border)", borderRadius: 8, background: "var(--bg-secondary)" }}>
                <div style={{ fontWeight: 600, marginBottom: 12, fontSize: 13 }}>
                  {llmEditIdx === -1 ? "添加供应商" : "编辑供应商"}
                </div>
                <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 10 }}>
                  <div className="form-group" style={{ marginBottom: 0 }}>
                    <label className="label">ID（唯一标识）*</label>
                    <input className="input" value={llmEditForm.id} onChange={e => setLlmEditForm(f => ({ ...f, id: e.target.value }))}
                      placeholder="my-gpt4" disabled={llmEditIdx !== -1} />
                  </div>
                  <div className="form-group" style={{ marginBottom: 0 }}>
                    <label className="label">显示名称</label>
                    <input className="input" value={llmEditForm.label} onChange={e => setLlmEditForm(f => ({ ...f, label: e.target.value }))}
                      placeholder="工作用 GPT-4" />
                  </div>
                  <div className="form-group" style={{ marginBottom: 0 }}>
                    <label className="label">供应商类型 *</label>
                    <select className="input" value={llmEditForm.provider} onChange={e => setLlmEditForm(f => ({ ...f, provider: e.target.value }))}>
                      <option value="anthropic">Anthropic (Claude)</option>
                      <option value="openai">OpenAI (GPT)</option>
                      <option value="deepseek">DeepSeek</option>
                      <option value="qwen">阿里百炼（Qwen）</option>
                      <option value="minimax">MiniMax</option>
                      <option value="zhipu">智谱 AI（GLM）</option>
                      <option value="kimi">Kimi（Moonshot）</option>
                      <option value="custom">自定义（OpenAI 兼容）</option>
                    </select>
                  </div>
                  <div className="form-group" style={{ marginBottom: 0 }}>
                    <label className="label">模型名称 *</label>
                    <input className="input" value={llmEditForm.model} onChange={e => setLlmEditForm(f => ({ ...f, model: e.target.value }))}
                      placeholder={
                        llmEditForm.provider === "anthropic" ? "claude-opus-4-5" :
                        llmEditForm.provider === "openai" ? "gpt-4o" :
                        llmEditForm.provider === "deepseek" ? "deepseek-chat" :
                        llmEditForm.provider === "qwen" ? "qwen3-max" :
                        llmEditForm.provider === "minimax" ? "MiniMax-M2.5" :
                        llmEditForm.provider === "zhipu" ? "glm-5" :
                        llmEditForm.provider === "kimi" ? "kimi-k2.5" :
                        "model-name"
                      } />
                  </div>
                  <div className="form-group" style={{ marginBottom: 0, gridColumn: "1 / -1" }}>
                    <label className="label">
                      API Key
                      {llmEditIdx !== -1 && <span style={{ fontSize: 11, color: "var(--text-muted)", marginLeft: 6 }}>留空表示不修改</span>}
                    </label>
                    <div style={{ display: "flex", gap: 6 }}>
                      <input className="input" style={{ flex: 1 }} type={llmShowKey ? "text" : "password"}
                        value={llmEditForm.api_key} onChange={e => setLlmEditForm(f => ({ ...f, api_key: e.target.value }))}
                        placeholder={llmEditIdx !== -1 ? "（保持不变）" : "sk-..."} />
                      <button className="btn" style={{ padding: "0 10px", border: "1px solid var(--border)" }}
                        onClick={() => setLlmShowKey(v => !v)}>
                        {llmShowKey ? "🙈" : "👁️"}
                      </button>
                    </div>
                  </div>
                  {llmEditForm.provider === "custom" && (
                    <div className="form-group" style={{ marginBottom: 0, gridColumn: "1 / -1" }}>
                      <label className="label">Base URL *</label>
                      <input className="input" value={llmEditForm.base_url} onChange={e => setLlmEditForm(f => ({ ...f, base_url: e.target.value }))}
                        placeholder="https://api.example.com/v1" />
                    </div>
                  )}
                  <div className="form-group" style={{ marginBottom: 0 }}>
                    <label className="label">最大输出 Token（0 = 继承全局）</label>
                    <input className="input" type="number" value={llmEditForm.max_tokens}
                      onChange={e => setLlmEditForm(f => ({ ...f, max_tokens: parseInt(e.target.value) || 0 }))} />
                  </div>
                </div>
                <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
                  <button className="btn btn-primary" style={{ fontSize: 12 }}
                    onClick={() => {
                      if (!llmEditForm.id.trim() || !llmEditForm.model.trim()) return;
                      if (llmEditIdx === -1) {
                        if (llmProviders.some(p => p.id === llmEditForm.id.trim())) return;
                        setLlmProviders(prev => [...prev, llmEditForm]);
                      } else {
                        setLlmProviders(prev => prev.map((p, i) => i === llmEditIdx ? { ...llmEditForm } : p));
                      }
                      setLlmEditIdx(null);
                    }}>
                    保存
                  </button>
                  <button className="btn" style={{ fontSize: 12, border: "1px solid var(--border)" }}
                    onClick={() => setLlmEditIdx(null)}>
                    取消
                  </button>
                </div>
              </div>
            ) : (
              <button className="btn" style={{ fontSize: 12, padding: "6px 14px", border: "1px solid var(--border)" }}
                onClick={() => { setLlmEditIdx(-1); setLlmEditForm(EMPTY_LLM_PROVIDER); setLlmShowKey(false); }}>
                + 添加供应商
              </button>
            )}
          </div>
        </section>

        {/* Workspace */}
        <section style={{ marginBottom: 32 }}>
          <h2 style={{ fontSize: 15, fontWeight: 600, color: "var(--text-primary)", marginBottom: 16, paddingBottom: 8, borderBottom: "1px solid var(--border)" }}>
            {t("settings.workspace")}
          </h2>
          <div className="form-group">
            <label className="label">{t("settings.workspaceRoot")} <span style={{ color: "var(--color-error, #ef4444)", marginLeft: 2 }}>*</span></label>
            <input
              className="input"
              value={form.workspace_root ?? ""}
              onChange={(e) => update("workspace_root", e.target.value)}
              placeholder={defaultWorkspace || t("settings.workspaceRootPlaceholder")}
              style={!(form.workspace_root ?? "").trim()
                ? { borderColor: "var(--color-warning, #f59e0b)" }
                : undefined}
            />
            {!(form.workspace_root ?? "").trim() && (
              <p style={{ fontSize: 12, color: "var(--color-warning, #f59e0b)", marginTop: 4 }}>
                {t("settings.workspaceRootRequired")}
              </p>
            )}
            {(form.workspace_root ?? "").trim() && (
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>
                {form.allow_outside_workspace
                  ? t("settings.workspaceRootHelpOutside")
                  : t("settings.workspaceRootHelp")}
              </p>
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
          <div className="form-group" style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
            <div>
              <div style={{ fontWeight: 500, color: "var(--text-primary)" }}>允许多开</div>
              <div style={{ fontSize: 12, color: "var(--text-secondary)" }}>
                开启后可同时运行多个 OpenPisci 实例；关闭时再次启动会激活已有窗口（重启后生效）
              </div>
            </div>
            <input type="checkbox" checked={form.allow_multiple_instances ?? false} onChange={(e) => update("allow_multiple_instances", e.target.checked)} />
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
          <div className="form-group">
            <label className="label">{t("settings.autoCompactThreshold")}</label>
            <input
              className="input"
              type="number"
              min={0}
              max={10000000}
              value={form.auto_compact_input_tokens_threshold ?? 200000}
              onChange={(e) =>
                update(
                  "auto_compact_input_tokens_threshold",
                  Math.max(0, Number(e.target.value) || 0)
                )
              }
            />
            <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>
              {t("settings.autoCompactThresholdDesc")}
            </p>
          </div>
          <div
            style={{
              display: "grid",
              gridTemplateColumns: "1fr 1fr 1fr",
              gap: 12,
              padding: 14,
              border: "1px solid var(--border)",
              borderRadius: 10,
              background: "var(--bg-secondary)",
              marginBottom: 12,
            }}
          >
            <div style={{ gridColumn: "1 / -1", marginBottom: 4 }}>
              <div style={{ fontWeight: 500, color: "var(--text-primary)" }}>
                {t("settings.compactionTiers")}
              </div>
              <div style={{ fontSize: 12, color: "var(--text-secondary)", marginTop: 4 }}>
                {t("settings.compactionTiersDesc")}
              </div>
            </div>
            <div className="form-group" style={{ marginBottom: 0 }}>
              <label className="label">{t("settings.compactionMicroPct")}</label>
              <input
                className="input"
                type="number"
                min={0}
                max={99}
                value={form.compaction_micro_percent ?? 60}
                onChange={(e) =>
                  update("compaction_micro_percent", Math.min(99, Math.max(0, Number(e.target.value) || 0)))
                }
              />
            </div>
            <div className="form-group" style={{ marginBottom: 0 }}>
              <label className="label">{t("settings.compactionAutoPct")}</label>
              <input
                className="input"
                type="number"
                min={1}
                max={99}
                value={form.compaction_auto_percent ?? 80}
                onChange={(e) =>
                  update("compaction_auto_percent", Math.min(99, Math.max(1, Number(e.target.value) || 1)))
                }
              />
            </div>
            <div className="form-group" style={{ marginBottom: 0 }}>
              <label className="label">{t("settings.compactionFullPct")}</label>
              <input
                className="input"
                type="number"
                min={1}
                max={99}
                value={form.compaction_full_percent ?? 95}
                onChange={(e) =>
                  update("compaction_full_percent", Math.min(99, Math.max(1, Number(e.target.value) || 1)))
                }
              />
            </div>
          </div>
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 12 }}>
            <div className="form-group">
              <label className="label">{t("settings.maxToolResultTokens")}</label>
              <input
                className="input"
                type="number"
                min={1000}
                max={200000}
                value={form.max_tool_result_tokens ?? 8000}
                onChange={(e) =>
                  update("max_tool_result_tokens", Math.max(1000, Number(e.target.value) || 1000))
                }
              />
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>
                {t("settings.maxToolResultTokensDesc")}
              </p>
            </div>
            <div className="form-group">
              <label className="label">{t("settings.summaryModel")}</label>
              <input
                className="input"
                value={form.summary_model ?? ""}
                onChange={(e) => update("summary_model", e.target.value)}
                placeholder={t("settings.summaryModelPlaceholder")}
              />
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>
                {t("settings.summaryModelDesc")}
              </p>
            </div>
          </div>
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 12 }}>
            <div className="form-group">
              <label className="label">{t("settings.llmReadTimeout")}</label>
              <input
                className="input"
                type="number"
                min={30}
                max={600}
                value={form.llm_read_timeout_secs ?? 120}
                onChange={(e) => update("llm_read_timeout_secs", Math.min(600, Math.max(30, Number(e.target.value))))}
              />
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>{t("settings.llmReadTimeoutDesc")}</p>
            </div>
            <div className="form-group">
              <label className="label">{t("settings.koiTimeout")}</label>
              <input
                className="input"
                type="number"
                min={60}
                max={7200}
                value={form.koi_timeout_secs ?? 600}
                onChange={(e) => update("koi_timeout_secs", Math.min(7200, Math.max(60, Number(e.target.value))))}
              />
              <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>{t("settings.koiTimeoutDesc")}</p>
            </div>
          </div>
          <div className="form-group" style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
            <div>
              <div style={{ fontWeight: 500, color: "var(--text-primary)" }}>{t("settings.enableProjectInstructions")}</div>
              <div style={{ fontSize: 12, color: "var(--text-secondary)" }}>{t("settings.enableProjectInstructionsDesc")}</div>
            </div>
            <input
              type="checkbox"
              checked={form.enable_project_instructions ?? true}
              onChange={(e) => update("enable_project_instructions", e.target.checked)}
            />
          </div>
          <div className="form-group">
            <label className="label">{t("settings.projectInstructionBudget")}</label>
            <input
              className="input"
              type="number"
              min={512}
              max={200000}
              value={form.project_instruction_budget_chars ?? 8000}
              onChange={(e) =>
                update(
                  "project_instruction_budget_chars",
                  Math.max(512, Number(e.target.value) || 512)
                )
              }
            />
            <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>
              {t("settings.projectInstructionBudgetDesc")}
            </p>
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
          <p style={{ fontSize: 12, color: "var(--text-muted)", marginBottom: 8 }}>
            {t("settings.imChannelsDesc")}
          </p>
          <p style={{ fontSize: 12, color: "var(--text-muted)", marginBottom: 16, padding: "8px 10px", background: "var(--bg-secondary)", borderRadius: 6, lineHeight: 1.6 }}>
            {t("settings.imLayerExplain")}
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
                <div style={{ fontSize: 11, fontWeight: 600, color: "var(--text-muted)", textTransform: "uppercase", letterSpacing: 0.5, marginTop: 4, marginBottom: 6 }}>
                  {t("settings.imCredsLabel")}
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.feishuAppId")}</label>
                  <input className="input" value={form.feishu_app_id} onChange={(e) => update("feishu_app_id", e.target.value)} placeholder="cli_xxxxxxxxxxxxxxxx" />
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.feishuAppSecret")}</label>
                  <input className="input" type={showKeys ? "text" : "password"} value={form.feishu_app_secret} onChange={(e) => update("feishu_app_secret", e.target.value)} placeholder="xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx" />
                </div>
                <div style={{ fontSize: 11, fontWeight: 600, color: "var(--text-muted)", textTransform: "uppercase", letterSpacing: 0.5, marginTop: 12, marginBottom: 6 }}>
                  {t("settings.imChannelLabel")}
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.feishuDomain")}</label>
                  <select className="input" value={form.feishu_domain} onChange={(e) => update("feishu_domain", e.target.value)}>
                    <option value="feishu">{t("settings.feishuDomainCN")}</option>
                    <option value="lark">{t("settings.feishuDomainIntl")}</option>
                  </select>
                </div>
                <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "4px 0 0" }}>{t("settings.feishuHelp")}</p>
                {renderEnterpriseCapabilityPanel(
                  "feishu",
                  t("settings.feishuEnterpriseTitle"),
                  t("settings.feishuEnterpriseDesc"),
                  { showOpenTools: true }
                )}
              </>
            )}
          </div>

          {/* WeChat (OpenClaw-compat) */}
          <div style={{ marginBottom: 20, padding: "14px 16px", border: "1px solid var(--border)", borderRadius: 8 }}>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: form.wechat_enabled ? 12 : 0 }}>
              <div>
                <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>{t("settings.wechat")}</span>
                <span style={{ fontSize: 12, color: "var(--text-muted)", marginLeft: 8 }}>{t("settings.wechatDesc")}</span>
              </div>
              <input type="checkbox" checked={form.wechat_enabled} onChange={(e) => update("wechat_enabled", e.target.checked)} />
            </div>
            {form.wechat_enabled && (
              <>
                <div className="form-group">
                  <label className="label">{t("settings.wechatGatewayToken")} <span style={{ fontSize: 11, color: "var(--text-muted)" }}>({t("common.optional")})</span></label>
                  <input className="input" type={showKeys ? "text" : "password"} value={form.wechat_gateway_token} onChange={(e) => update("wechat_gateway_token", e.target.value)} placeholder="" />
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.wechatGatewayPort")}</label>
                  <input className="input" type="number" value={form.wechat_gateway_port} onChange={(e) => update("wechat_gateway_port", parseInt(e.target.value) || 18789)} placeholder="18789" />
                </div>

                {/* Bind button and QR display */}
                <div style={{ marginTop: 12 }}>
                  <button
                    className="btn btn-secondary"
                    style={{ fontSize: 13 }}
                    disabled={wechatBindState === "loading"}
                    onClick={handleWechatBind}
                  >
                    {wechatBindState === "loading"
                      ? t("settings.wechatBindLoading")
                      : t("settings.wechatBindBtn")}
                  </button>

                  {(wechatBindState === "scan" || wechatBindState === "scaned") && wechatQr && (
                    <div style={{ marginTop: 12, textAlign: "center" }}>
                      <p style={{ fontSize: 12, color: "var(--text-muted)", marginBottom: 8 }}>
                        {wechatBindState === "scaned"
                          ? t("settings.wechatScaned")
                          : t("settings.wechatBindScanPrompt")}
                      </p>
                      <img
                        src={wechatQr}
                        alt="WeChat QR"
                        style={{ width: 200, height: 200, border: "1px solid var(--border)", borderRadius: 8, opacity: wechatBindState === "scaned" ? 0.4 : 1 }}
                      />
                    </div>
                  )}

                  {wechatBindState === "success" && (
                    <p style={{ fontSize: 13, color: "var(--success, #22c55e)", marginTop: 8 }}>
                      ✓ {t("settings.wechatBindSuccess")}
                    </p>
                  )}

                  {wechatBindState === "error" && wechatBindError && (
                    <p style={{ fontSize: 12, color: "var(--error, #ef4444)", marginTop: 8, whiteSpace: "pre-line" }}>
                      {wechatBindError}
                    </p>
                  )}
                </div>

                <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "12px 0 0", whiteSpace: "pre-line" }}>{t("settings.wechatHelp")}</p>
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
                <div style={{ fontSize: 11, fontWeight: 600, color: "var(--text-muted)", textTransform: "uppercase", letterSpacing: 0.5, marginTop: 4, marginBottom: 6 }}>
                  {t("settings.imCredsLabel")}
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.wecomBotId")}</label>
                  <input className="input" value={form.wecom_bot_id ?? ""} onChange={(e) => update("wecom_bot_id", e.target.value)} placeholder="BOTID" />
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.wecomBotSecret")}</label>
                  <input className="input" type={showKeys ? "text" : "password"} value={form.wecom_bot_secret ?? ""} onChange={(e) => update("wecom_bot_secret", e.target.value)} placeholder="SECRET" />
                </div>
                <div style={{ fontSize: 11, fontWeight: 600, color: "var(--text-muted)", textTransform: "uppercase", letterSpacing: 0.5, marginTop: 12, marginBottom: 4 }}>
                  {t("settings.imChannelLabel")}
                </div>
                <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "4px 0 0", whiteSpace: "pre-line" }}>{t("settings.wecomHelp")}</p>
                {renderEnterpriseCapabilityPanel(
                  "wecom",
                  t("settings.wecomEnterpriseTitle"),
                  t("settings.wecomEnterpriseDesc"),
                  { showEnable: false }
                )}
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
                <div style={{ fontSize: 11, fontWeight: 600, color: "var(--text-muted)", textTransform: "uppercase", letterSpacing: 0.5, marginTop: 4, marginBottom: 6 }}>
                  {t("settings.imCredsLabel")}
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.dingtalkAppKey")}</label>
                  <input className="input" value={form.dingtalk_app_key} onChange={(e) => update("dingtalk_app_key", e.target.value)} placeholder="dingxxxxxxxxxxxxxxxx" />
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.dingtalkAppSecret")}</label>
                  <input className="input" type={showKeys ? "text" : "password"} value={form.dingtalk_app_secret} onChange={(e) => update("dingtalk_app_secret", e.target.value)} placeholder="xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx" />
                </div>
                <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 12 }}>
                  <div className="form-group">
                    <label className="label">{t("settings.dingtalkCorpId")} <span style={{ fontSize: 11, color: "var(--text-muted)" }}>({t("common.optional")})</span></label>
                    <input className="input" value={form.dingtalk_corp_id ?? ""} onChange={(e) => update("dingtalk_corp_id", e.target.value)} placeholder="dingxxxxxxxxxxxx" />
                  </div>
                  <div className="form-group">
                    <label className="label">{t("settings.dingtalkAgentId")} <span style={{ fontSize: 11, color: "var(--text-muted)" }}>({t("common.optional")})</span></label>
                    <input className="input" value={form.dingtalk_agent_id ?? ""} onChange={(e) => update("dingtalk_agent_id", e.target.value)} placeholder="123456789" />
                  </div>
                </div>
                <div style={{ fontSize: 11, fontWeight: 600, color: "var(--text-muted)", textTransform: "uppercase", letterSpacing: 0.5, marginTop: 12, marginBottom: 6 }}>
                  {t("settings.imChannelLabel")}
                </div>
                <div className="form-group">
                  <label className="label">{t("settings.dingtalkRobotCode")}</label>
                  <input className="input" value={form.dingtalk_robot_code ?? ""} onChange={(e) => update("dingtalk_robot_code", e.target.value)} placeholder="dingxxxxxxxxxxxxxxxx" />
                </div>
                <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "4px 0 0", whiteSpace: "pre-line" }}>{t("settings.dingtalkHelp")}</p>
                <div className="form-group" style={{ marginTop: 12 }}>
                  <label className="label">{t("settings.dingtalkMcpUrl")}</label>
                  <input className="input" value={form.dingtalk_mcp_url ?? ""} onChange={(e) => update("dingtalk_mcp_url", e.target.value)} placeholder="https://mcp-gw.dingtalk.com/..." />
                  <p style={{ fontSize: 12, color: "var(--text-muted)", margin: "6px 0 0", lineHeight: 1.6 }}>
                    {t("settings.dingtalkMcpUrlHelp")}
                  </p>
                  <button className="btn btn-secondary" type="button" style={{ marginTop: 8, fontSize: 12 }} onClick={() => openUrl("https://mcp.dingtalk.com/")}>
                    {t("settings.dingtalkOpenMcpMarketplace")}
                  </button>
                </div>
                {renderEnterpriseCapabilityPanel(
                  "dingtalk",
                  t("settings.dingtalkEnterpriseTitle"),
                  t("settings.dingtalkEnterpriseDesc"),
                  { showOpenTools: true }
                )}
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
