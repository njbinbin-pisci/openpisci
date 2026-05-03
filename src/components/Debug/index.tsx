import { useState, useCallback, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-shell";
import { useTranslation } from "react-i18next";
import { systemApi, settingsApi, RuntimeCheckItem, Settings, SystemDependencyItem, poolApi, koiApi, PoolMessage, KoiWithStats } from "../../services/tauri";
import "./Debug.css";

// ─── Types (mirror Rust structs) ─────────────────────────────────────────────

interface DebugScenario {
  id: string;
  name: string;
  description: string;
  name_en: string;
  description_en: string;
  prompt: string;
  expected_keywords: string[];
  expected_tools: string[];
  requires_config?: string[] | null;
  /** Which platforms this scenario supports. null means all platforms. */
  platforms?: string[] | null;
}

interface ToolCallRecord {
  tool_name: string;
  input_summary: string;
  result_summary: string;
  is_error: boolean;
  duration_ms: number;
}

interface ScenarioResult {
  scenario_id: string;
  scenario_name: string;
  passed: boolean;
  response_text: string;
  tool_calls: ToolCallRecord[];
  error: string | null;
  duration_ms: number;
  input_tokens: number;
  output_tokens: number;
  missing_keywords: string[];
  missing_tools: string[];
  unexpected_tool_errors: string[];
}

interface SystemInfo {
  os: string;
  provider: string;
  model: string;
  workspace_root: string;
  policy_mode: string;
  max_iterations: number;
  tool_rate_limit: number;
  api_key_configured: boolean;
  vision_enabled: boolean;
  /** Whether a vision model is effectively configured (main model supports vision OR separate vision model is set). */
  vision_configured: boolean;
  /** Whether a separate vision model is in use (not the main LLM). */
  vision_uses_separate_model: boolean;
}

interface SettingsSummary {
  provider: string;
  model: string;
  workspace_root: string;
  policy_mode: string;
  max_tokens: number;
  max_iterations: number;
  confirm_shell: boolean;
  confirm_file_write: boolean;
  enabled_tools: string[];
  disabled_tools: string[];
}

interface DebugReport {
  timestamp: string;
  system_info: SystemInfo;
  settings_summary: SettingsSummary;
  system_dependencies: SystemDependencyItem[];
  available_tools: string[];
  recent_audit: any[];
  recent_errors: string[];
  log_tail: string[];
  scenario_results: ScenarioResult[];
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

function StatusBadge({ passed, running }: { passed?: boolean; running?: boolean }) {
  const { t } = useTranslation();
  if (running) return <span className="dbg-badge dbg-badge-running">{t("debug.running")}</span>;
  if (passed === undefined) return <span className="dbg-badge dbg-badge-idle">{t("debug.idle")}</span>;
  return passed
    ? <span className="dbg-badge dbg-badge-pass">{t("debug.passed")}</span>
    : <span className="dbg-badge dbg-badge-fail">{t("debug.failed")}</span>;
}

function ms(n: number) {
  return n >= 1000 ? `${(n / 1000).toFixed(1)}s` : `${n}ms`;
}

function localizedDependencyRemediation(
  t: (key: string, options?: Record<string, unknown>) => string,
  item: SystemDependencyItem,
): string | null {
  switch (item.key) {
    case "linux-session":
      return t("settings.depRemediation_linux_session");
    case "xdotool":
      return t("settings.depRemediation_xdotool");
    case "wmctrl":
      return t("settings.depRemediation_wmctrl");
    case "xclip":
      return t("settings.depRemediation_xclip");
    case "cliclick":
      return t("settings.depRemediation_cliclick");
    case "osascript":
      return t("settings.depRemediation_osascript");
    case "macos-accessibility":
      return t("settings.depRemediation_macos_accessibility");
    case "powershell":
      return t("settings.depRemediation_powershell");
    case "uia-runtime":
      return t("settings.depRemediation_uia_runtime");
    case "wmi-service":
      return t("settings.depRemediation_wmi_service");
    case "office-installation":
      return t("settings.depRemediation_office_installation");
    default:
      return item.remediation;
  }
}

function isScenarioAvailable(scenario: DebugScenario, settings: Settings | null): boolean {
  if (!scenario.requires_config || scenario.requires_config.length === 0) return true;
  if (!settings) return true; // optimistic while loading
  for (const req of scenario.requires_config) {
    if (req === "ssh_servers" && (!settings.ssh_servers || settings.ssh_servers.length === 0)) {
      return false;
    }
  }
  return true;
}

// ─── Main Component ───────────────────────────────────────────────────────────

export default function DebugPanel() {
  const { t } = useTranslation();
  const [scenarios, setScenarios] = useState<DebugScenario[]>([]);
  const [results, setResults] = useState<Record<string, ScenarioResult>>({});
  const [running, setRunning] = useState<Record<string, boolean>>({});
  const [report, setReport] = useState<DebugReport | null>(null);
  const [logLines, setLogLines] = useState<string[]>([]);
  const [activeTab, setActiveTab] = useState<"scenarios" | "report" | "logs" | "uia" | "multiagent">("scenarios");
  const [loadingReport, setLoadingReport] = useState(false);
  const [loadingLogs, setLoadingLogs] = useState(false);
  const [runningAll, setRunningAll] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [runtimes, setRuntimes] = useState<RuntimeCheckItem[] | null>(null);
  const [loadingRuntimes, setLoadingRuntimes] = useState(false);
  const [appSettings, setAppSettings] = useState<Settings | null>(null);
  const logEndRef = useRef<HTMLDivElement>(null);

  // Load scenario list and settings on mount
  useEffect(() => {
    invoke<DebugScenario[]>("list_debug_scenarios")
      .then((list) => {
        setScenarios(list);
        if (list.length > 0) setSelectedId(list[0].id);
      })
      .catch(console.error);
    settingsApi.get()
      .then(setAppSettings)
      .catch(console.error);
  }, []);

  // Auto-scroll log
  useEffect(() => {
    logEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [logLines]);

  const runScenario = useCallback(async (id: string) => {
    setRunning((r) => ({ ...r, [id]: true }));
    try {
      const result = await invoke<ScenarioResult>("run_debug_scenario", { scenarioId: id });
      setResults((r) => ({ ...r, [id]: result }));
    } catch (e) {
      setResults((r) => ({
        ...r,
        [id]: {
          scenario_id: id,
          scenario_name: id,
          passed: false,
          response_text: "",
          tool_calls: [],
          error: String(e),
          duration_ms: 0,
          input_tokens: 0,
          output_tokens: 0,
          missing_keywords: [],
          missing_tools: [],
          unexpected_tool_errors: [],
        },
      }));
    } finally {
      setRunning((r) => ({ ...r, [id]: false }));
    }
  }, []);

  const runAll = useCallback(async () => {
    setRunningAll(true);
    // Run sequentially to avoid overloading the LLM API; skip unavailable scenarios
    for (const s of scenarios) {
      if (!isScenarioAvailable(s, appSettings)) continue;
      setRunning((r) => ({ ...r, [s.id]: true }));
      try {
        const result = await invoke<ScenarioResult>("run_debug_scenario", { scenarioId: s.id });
        setResults((r) => ({ ...r, [s.id]: result }));
      } catch (e) {
        setResults((r) => ({
          ...r,
          [s.id]: {
            scenario_id: s.id,
            scenario_name: s.name,
            passed: false,
            response_text: "",
            tool_calls: [],
            error: String(e),
            duration_ms: 0,
            input_tokens: 0,
            output_tokens: 0,
            missing_keywords: [],
            missing_tools: [],
            unexpected_tool_errors: [],
          },
        }));
      } finally {
        setRunning((r) => ({ ...r, [s.id]: false }));
      }
    }
    setRunningAll(false);
  }, [scenarios, appSettings]);

  const loadRuntimes = useCallback(async () => {
    setLoadingRuntimes(true);
    try {
      const items = await systemApi.checkRuntimes();
      setRuntimes(items);
    } catch (e) {
      console.error(e);
    } finally {
      setLoadingRuntimes(false);
    }
  }, []);

  const loadReport = useCallback(async () => {
    setLoadingReport(true);
    try {
      const r = await invoke<DebugReport>("get_debug_report");
      setReport(r);
      setActiveTab("report");
      // Also refresh runtime check when loading the report
      loadRuntimes();
    } catch (e) {
      console.error(e);
    } finally {
      setLoadingReport(false);
    }
  }, [loadRuntimes]);

  const loadLogs = useCallback(async () => {
    setLoadingLogs(true);
    try {
      const lines = await invoke<string[]>("get_log_tail", { lines: 200 });
      setLogLines(lines);
      setActiveTab("logs");
    } catch (e) {
      console.error(e);
    } finally {
      setLoadingLogs(false);
    }
  }, []);

  const passCount = Object.values(results).filter((r) => r.passed).length;
  const failCount = Object.values(results).filter((r) => !r.passed).length;
  const totalRun = passCount + failCount;

  return (
    <div className="dbg-root">
      <div className="dbg-header">
        <div className="dbg-title">
          <span className="dbg-icon">🔬</span>
          <span>{t("debug.title")}</span>
        </div>
        <div className="dbg-header-actions">
          {totalRun > 0 && (
            <span className="dbg-summary">
              {t("debug.passCount", { pass: passCount, total: totalRun })}
            </span>
          )}
          <button
            className="dbg-btn dbg-btn-secondary"
            onClick={loadReport}
            disabled={loadingReport}
          >
            {loadingReport ? t("common.loading") : t("debug.diagReport")}
          </button>
          <button
            className="dbg-btn dbg-btn-secondary"
            onClick={loadLogs}
            disabled={loadingLogs}
          >
            {loadingLogs ? t("common.loading") : t("debug.viewLogs")}
          </button>
          <button
            className="dbg-btn dbg-btn-primary"
            onClick={runAll}
            disabled={runningAll}
          >
            {runningAll ? t("debug.running") : t("debug.runAll")}
          </button>
        </div>
      </div>

      <div className="dbg-tabs">
        <button
          className={`dbg-tab ${activeTab === "scenarios" ? "active" : ""}`}
          onClick={() => setActiveTab("scenarios")}
        >
          {t("debug.tabScenarios")}
        </button>
        <button
          className={`dbg-tab ${activeTab === "report" ? "active" : ""}`}
          onClick={() => setActiveTab("report")}
        >
          {t("debug.tabReport")}
        </button>
        <button
          className={`dbg-tab ${activeTab === "logs" ? "active" : ""}`}
          onClick={() => setActiveTab("logs")}
        >
          {t("debug.tabLogs")}
        </button>
        <button
          className={`dbg-tab ${activeTab === "uia" ? "active" : ""}`}
          onClick={() => setActiveTab("uia")}
        >
          {t("debug.tabUia")}
        </button>
        <button
          className={`dbg-tab ${activeTab === "multiagent" ? "active" : ""}`}
          onClick={() => setActiveTab("multiagent")}
        >
          Multi-Agent
        </button>
      </div>

      <div className="dbg-body">
        {activeTab === "scenarios" && (
          <div className="dbg-scenarios-layout">
            {/* Left: scrollable scenario list */}
            <div className="dbg-scenarios-list">
              {scenarios.map((s) => {
                const result = results[s.id];
                const isRunning = running[s.id] ?? false;
                return (
                  <ScenarioListItem
                    key={s.id}
                    scenario={s}
                    result={result}
                    isRunning={isRunning}
                    selected={selectedId === s.id}
                    onSelect={() => setSelectedId(s.id)}
                    available={isScenarioAvailable(s, appSettings)}
                  />
                );
              })}
              {scenarios.length === 0 && (
                <div className="dbg-empty">{t("debug.loadingScenarios")}</div>
              )}
            </div>

            {/* Right: detail pane for selected scenario */}
            {selectedId && scenarios.find((s) => s.id === selectedId) ? (
              <ScenarioDetail
                scenario={scenarios.find((s) => s.id === selectedId)!}
                result={results[selectedId]}
                isRunning={running[selectedId] ?? false}
                onRun={() => runScenario(selectedId)}
                available={isScenarioAvailable(scenarios.find((s) => s.id === selectedId)!, appSettings)}
              />
            ) : (
              <div className="dbg-detail-empty">{t("debug.selectScenario")}</div>
            )}
          </div>
        )}

        {activeTab === "report" && (
          <div className="dbg-report">
            {report ? (
              <ReportView
                report={report}
                runtimes={runtimes}
                loadingRuntimes={loadingRuntimes}
                onRefreshRuntimes={loadRuntimes}
              />
            ) : (
              <div className="dbg-empty">{t("debug.loadReport")}</div>
            )}
          </div>
        )}

        {activeTab === "logs" && (
          <div className="dbg-logs">
            <div className="dbg-logs-toolbar">
              <button className="dbg-btn dbg-btn-secondary" onClick={loadLogs} disabled={loadingLogs}>
                {t("debug.refresh")}
              </button>
              <span className="dbg-logs-count">{t("debug.logLines", { count: logLines.length })}</span>
            </div>
            <div className="dbg-log-content">
              {logLines.map((line, i) => (
                <LogLine key={i} line={line} />
              ))}
              {logLines.length === 0 && (
                <div className="dbg-empty">{t("debug.loadLogs")}</div>
              )}
              <div ref={logEndRef} />
            </div>
          </div>
        )}

        {activeTab === "uia" && (
          <UiaTestPanel />
        )}

        {activeTab === "multiagent" && (
          <MultiAgentTestPanel />
        )}
      </div>
    </div>
  );
}

// ─── Scenario List Item (left sidebar) ───────────────────────────────────────

function ScenarioListItem({
  scenario,
  result,
  isRunning,
  selected,
  onSelect,
  available = true,
}: {
  scenario: DebugScenario;
  result?: ScenarioResult;
  isRunning: boolean;
  selected: boolean;
  onSelect: () => void;
  available?: boolean;
}) {
  const { i18n } = useTranslation();
  const isEn = i18n.language.startsWith("en");
  const displayName = isEn && scenario.name_en ? scenario.name_en : scenario.name;

  return (
    <div
      className={`dbg-scenario-item ${selected ? "selected" : ""} ${!available ? "unavailable" : ""}`}
      onClick={onSelect}
      title={!available ? (isEn ? "Requires configuration — see Settings" : "需要先完成配置，请前往设置") : undefined}
    >
      <span className="dbg-scenario-item-badge">
        {!available
          ? <span className="dbg-badge dbg-badge-idle" style={{ opacity: 0.5 }}>—</span>
          : <StatusBadge passed={result?.passed} running={isRunning} />
        }
      </span>
      <span className="dbg-scenario-item-name" title={displayName}>{displayName}</span>
    </div>
  );
}

// ─── Scenario Detail (right pane) ────────────────────────────────────────────

function ScenarioDetail({
  scenario,
  result,
  isRunning,
  onRun,
  available = true,
}: {
  scenario: DebugScenario;
  result?: ScenarioResult;
  isRunning: boolean;
  onRun: () => void;
  available?: boolean;
}) {
  const { t, i18n } = useTranslation();
  const isEn = i18n.language.startsWith("en");
  const displayName = isEn && scenario.name_en ? scenario.name_en : scenario.name;
  const displayDesc = isEn && scenario.description_en ? scenario.description_en : scenario.description;

  return (
    <div className="dbg-scenario-detail">
      {/* Header */}
      <div className="dbg-detail-header">
        <div className="dbg-detail-title">{displayName}</div>
        <button
          className="dbg-btn dbg-btn-primary"
          onClick={onRun}
          disabled={isRunning || !available}
          title={!available ? (isEn ? "Configure required settings first" : "请先在设置中完成相关配置") : undefined}
        >
          {isRunning ? t("debug.running") : t("debug.run")}
        </button>
      </div>

      {/* Unavailable notice */}
      {!available && (
        <div className="dbg-requires-config">
          {isEn
            ? "⚠ This scenario requires configuration. Go to Settings → SSH Servers to add a server."
            : "⚠ 此场景需要先完成配置。请前往「设置 → SSH 服务器」添加服务器后再运行。"}
        </div>
      )}

      {/* Description */}
      <div className="dbg-detail-desc">{displayDesc}</div>

      {/* Prompt preview */}
      <div>
        <div className="dbg-section-label">{t("debug.prompt")}</div>
        <div className="dbg-detail-prompt">{scenario.prompt}</div>
      </div>

      {/* Expected tools */}
      {scenario.expected_tools.length > 0 && (
        <div className="dbg-detail-meta">
          <span>{t("debug.expectedTools")}</span>
          {scenario.expected_tools.map((tool) => (
            <span key={tool} className={`dbg-tool-tag ${result && result.missing_tools.includes(tool) ? "missing" : ""}`}>
              {tool}
            </span>
          ))}
        </div>
      )}

      {/* Result */}
      {result && (
        <>
          {/* Status + timing */}
          <div className="dbg-detail-meta">
            <StatusBadge passed={result.passed} running={isRunning} />
            <span>{ms(result.duration_ms)}</span>
            <span>{result.input_tokens + result.output_tokens} tokens</span>
          </div>

          {result.error && (
            <div className="dbg-error-box">
              <strong>{t("common.error")}：</strong>{result.error}
            </div>
          )}

          {result.missing_keywords.length > 0 && (
            <div className="dbg-warn-box">
              <strong>{t("debug.missingKeywords")}</strong>{result.missing_keywords.join(", ")}
            </div>
          )}

          {result.unexpected_tool_errors.length > 0 && (
            <div className="dbg-error-box">
              <strong>{t("debug.toolErrors")}</strong>
              <ul>
                {result.unexpected_tool_errors.map((e, i) => (
                  <li key={i}>{e}</li>
                ))}
              </ul>
            </div>
          )}

          {result.tool_calls.length > 0 && (
            <div className="dbg-tool-calls">
              <div className="dbg-section-label">{t("debug.toolCalls")}</div>
              {result.tool_calls.map((tc, i) => (
                <ToolCallRow key={i} record={tc} />
              ))}
            </div>
          )}

          {result.response_text && (
            <div className="dbg-response">
              <div className="dbg-section-label">{t("debug.agentReply")}</div>
              <pre className="dbg-pre">{result.response_text}</pre>
            </div>
          )}
        </>
      )}

      {!result && !isRunning && (
        <div style={{ color: "var(--text-secondary)", fontSize: 13 }}>
          {t("debug.notRunYet")}
        </div>
      )}
    </div>
  );
}

// ─── Tool Call Row ────────────────────────────────────────────────────────────

function ToolCallRow({ record }: { record: ToolCallRecord }) {
  const [expanded, setExpanded] = useState(false);
  return (
    <div className={`dbg-tool-row ${record.is_error ? "dbg-tool-row-error" : ""}`}>
      <button className="dbg-tool-row-header" onClick={() => setExpanded(!expanded)}>
        <span className={`dbg-tool-status ${record.is_error ? "error" : "ok"}`}>
          {record.is_error ? "✕" : "✓"}
        </span>
        <span className="dbg-tool-name">{record.tool_name}</span>
        <span className="dbg-tool-input">{record.input_summary}</span>
        <span className="dbg-tool-dur">{ms(record.duration_ms)}</span>
        <span className="dbg-chevron-sm">{expanded ? "▲" : "▼"}</span>
      </button>
      {expanded && (
        <pre className={`dbg-tool-result ${record.is_error ? "error" : ""}`}>
          {record.result_summary}
        </pre>
      )}
    </div>
  );
}

// ─── Report View ──────────────────────────────────────────────────────────────

function ReportView({
  report,
  runtimes,
  loadingRuntimes,
  onRefreshRuntimes,
}: {
  report: DebugReport;
  runtimes: RuntimeCheckItem[] | null;
  loadingRuntimes: boolean;
  onRefreshRuntimes: () => void;
}) {
  const { t, i18n } = useTranslation();
  const info = report.system_info;
  const settings = report.settings_summary;

  return (
    <div className="dbg-report-content">
      <div className="dbg-report-section">
        <div className="dbg-section-title">{t("debug.reportSystem")}</div>
        <table className="dbg-table">
          <tbody>
            <tr><td>{t("debug.reportOs")}</td><td>{info.os}</td></tr>
            <tr><td>{t("debug.reportProvider")}</td><td>{info.provider}</td></tr>
            <tr><td>{t("debug.reportModel")}</td><td>{info.model}</td></tr>
            <tr>
              <td>{t("debug.reportApiKey")}</td>
              <td>
                <span className={`dbg-badge ${info.api_key_configured ? "dbg-badge-pass" : "dbg-badge-fail"}`}>
                  {info.api_key_configured ? t("debug.reportApiKeyOk") : t("debug.reportApiKeyMissing")}
                </span>
              </td>
            </tr>
            <tr>
              <td>{t("debug.reportWorkspace")}</td>
              <td className="dbg-path">
                {info.workspace_root
                  ? info.workspace_root
                  : <span style={{ color: "var(--text-secondary)", fontStyle: "italic" }}>{t("debug.reportWorkspaceEmpty")}</span>
                }
              </td>
            </tr>
            <tr><td>{t("debug.reportPolicy")}</td><td>{info.policy_mode}</td></tr>
            <tr><td>{t("debug.reportMaxIter")}</td><td>{info.max_iterations}</td></tr>
            <tr><td>{t("debug.reportRateLimit")}</td><td>{info.tool_rate_limit}{t("debug.reportRateLimitUnit")}</td></tr>
          </tbody>
        </table>
      </div>

      {/* Runtime environment check */}
      <div className="dbg-report-section">
        <div className="dbg-section-title" style={{ display: "flex", alignItems: "center", gap: 8 }}>
          {t("debug.reportRuntimes")}
          <button
            className="dbg-btn dbg-btn-secondary"
            style={{ fontSize: 11, padding: "2px 8px", marginLeft: "auto" }}
            onClick={onRefreshRuntimes}
            disabled={loadingRuntimes}
          >
            {loadingRuntimes ? t("common.loading") : t("debug.refresh")}
          </button>
        </div>
        {runtimes === null ? (
          <div style={{ color: "var(--text-secondary)", fontSize: 12, padding: "6px 0" }}>
            {t("debug.runtimesHint")}
          </div>
        ) : (
          <table className="dbg-table">
            <tbody>
              {runtimes.map((item) => (
                <tr key={item.name}>
                  <td style={{ fontWeight: 500 }}>{item.name}</td>
                  <td>
                    {item.available ? (
                      <span className="dbg-badge dbg-badge-pass">
                        {item.version ?? t("debug.runtimeAvailable")}
                      </span>
                    ) : (
                      <span className="dbg-badge dbg-badge-fail">{t("debug.runtimeMissing")}</span>
                    )}
                  </td>
                  <td style={{ color: "var(--text-secondary)", fontSize: 12 }}>{item.hint}</td>
                  <td>
                    {!item.available && (
                      <button
                        className="dbg-btn dbg-btn-secondary"
                        style={{ fontSize: 11, padding: "2px 8px" }}
                        onClick={() => open(item.download_url)}
                      >
                        {t("debug.runtimeDownload")}
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      {report.system_dependencies.length > 0 && (
        <div className="dbg-report-section">
          <div className="dbg-section-title">{t("debug.reportSystemDeps")}</div>
          <div className="dbg-dependency-list">
            {report.system_dependencies.map((item) => {
              const localized = localizedDependencyRemediation(t, item);
              return (
                <div key={item.key} className={`dbg-dependency-item dbg-dependency-${item.status}`}>
                  <div className="dbg-dependency-header">
                    <span className="dbg-dependency-name">{item.name}</span>
                    <span className={`dbg-badge ${item.status === "ok" ? "dbg-badge-pass" : item.status === "missing" ? "dbg-badge-fail" : "dbg-badge-running"}`}>
                      {item.status === "ok" ? t("settings.dependencyStatusOk") : item.status === "missing" ? t("settings.dependencyStatusMissing") : t("settings.dependencyStatusWarning")}
                    </span>
                    <span className="dbg-dependency-meta">
                      {item.required ? t("settings.dependencyRequired") : t("settings.dependencyRecommended")} · {item.feature}
                    </span>
                  </div>
                  {item.details && <div className="dbg-dependency-details">{item.details}</div>}
                  <div className="dbg-dependency-hint">{item.hint}</div>
                  {!item.available && localized && (
                    <div className="dbg-dependency-remediation">{localized}</div>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      )}

      <div className="dbg-report-section">
        <div className="dbg-section-title">{t("debug.reportTools", { count: report.available_tools.length })}</div>
        <div className="dbg-tool-list">
          {report.available_tools.map((tool) => (
            <span key={tool} className="dbg-tool-tag">{tool}</span>
          ))}
        </div>
        {settings.disabled_tools.length > 0 && (
          <div className="dbg-disabled-tools">
            {t("debug.reportDisabled")}{settings.disabled_tools.map((tool) => (
              <span key={tool} className="dbg-tool-tag missing">{tool}</span>
            ))}
          </div>
        )}
      </div>

      <div className="dbg-report-section">
        <div className="dbg-section-title">{t("debug.reportConfirm")}</div>
        <table className="dbg-table">
          <tbody>
            <tr>
              <td>{t("debug.reportConfirmShell")}</td>
              <td>
                <span className={`dbg-badge ${settings.confirm_shell ? "dbg-badge-running" : "dbg-badge-idle"}`}>
                  {settings.confirm_shell ? t("debug.reportConfirmOn") : t("debug.reportConfirmOff")}
                </span>
              </td>
            </tr>
            <tr>
              <td>{t("debug.reportConfirmFile")}</td>
              <td>
                <span className={`dbg-badge ${settings.confirm_file_write ? "dbg-badge-running" : "dbg-badge-idle"}`}>
                  {settings.confirm_file_write ? t("debug.reportConfirmOn") : t("debug.reportConfirmOff")}
                </span>
              </td>
            </tr>
          </tbody>
        </table>
        {(settings.confirm_shell || settings.confirm_file_write) && (
          <div className="dbg-warn-box" style={{ marginTop: 8 }}>
            {t("debug.reportConfirmWarn")}
          </div>
        )}
      </div>

      {report.recent_errors.length > 0 && (
        <div className="dbg-report-section">
          <div className="dbg-section-title">{t("debug.reportErrors", { count: report.recent_errors.length })}</div>
          <div className="dbg-error-list">
            {report.recent_errors.map((e, i) => (
              <div key={i} className="dbg-error-item">{e}</div>
            ))}
          </div>
        </div>
      )}

      {report.recent_audit.length > 0 && (
        <div className="dbg-report-section">
          <div className="dbg-section-title">{t("debug.reportAudit")}</div>
          <div className="dbg-audit-list">
            {report.recent_audit.map((entry: any, i: number) => (
              <div key={i} className={`dbg-audit-item ${entry.is_error ? "error" : ""}`}>
                <span className="dbg-audit-tool">{entry.tool_name}</span>
                <span className="dbg-audit-action">{entry.action}</span>
                {entry.result_summary && (
                  <span className="dbg-audit-result">{entry.result_summary}</span>
                )}
              </div>
            ))}
          </div>
        </div>
      )}

      <div className="dbg-report-ts">
        {t("debug.reportTs", { time: new Date(report.timestamp).toLocaleString(i18n.language) })}
      </div>
    </div>
  );
}

// ─── Log Line ─────────────────────────────────────────────────────────────────

function LogLine({ line }: { line: string }) {
  let cls = "dbg-log-line";
  if (line.includes('"level":"ERROR"') || line.includes("ERROR") || line.includes("error")) {
    cls += " dbg-log-error";
  } else if (line.includes('"level":"WARN"') || line.includes("WARN") || line.includes("warn")) {
    cls += " dbg-log-warn";
  } else if (line.includes("tool_exec") || line.includes("executing tool")) {
    cls += " dbg-log-tool";
  } else if (line.includes("agent loop") || line.includes("LLM")) {
    cls += " dbg-log-agent";
  }

  // Try to parse JSON log line for nicer display
  try {
    const obj = JSON.parse(line);
    const ts = obj.timestamp ? new Date(obj.timestamp).toLocaleTimeString("zh-CN") : "";
    const level = obj.level ?? "";
    const msg = obj.fields?.message ?? obj.message ?? line;
    const span = obj.span?.name ?? "";
    return (
      <div className={cls}>
        <span className="dbg-log-ts">{ts}</span>
        <span className={`dbg-log-level dbg-log-level-${level.toLowerCase()}`}>{level}</span>
        {span && <span className="dbg-log-span">[{span}]</span>}
        <span className="dbg-log-msg">{msg}</span>
      </div>
    );
  } catch {
    return <div className={cls}>{line}</div>;
  }
}

// ─── UIA Precision Test Panel ─────────────────────────────────────────────────

type DragState = "idle" | "success" | "fail";

interface UiaDragTestResult {
  passed: boolean;
  response_text: string;
  error: string | null;
  duration_ms: number;
  tool_calls: ToolCallRecord[];
}

function UiaTestPanel() {
  const { t } = useTranslation();
  const arenaRef = useRef<HTMLDivElement>(null);
  const ballRef = useRef<HTMLDivElement>(null);
  const targetRef = useRef<HTMLDivElement>(null);

  // Ball position (relative to arena, px)
  const [ballPos, setBallPos] = useState({ x: 60, y: 120 });
  const ballPosRef = useRef({ x: 60, y: 120 });
  const [dragState, setDragState] = useState<DragState>("idle");
  const [arenaRect, setArenaRect] = useState<DOMRect | null>(null);

  // Agent test state
  const [testRunning, setTestRunning] = useState(false);
  const [testResult, setTestResult] = useState<UiaDragTestResult | null>(null);
  const [visionConfigured, setVisionConfigured] = useState<boolean | null>(null);

  // Load vision status from report
  useEffect(() => {
    invoke<{ system_info: SystemInfo }>("get_debug_report")
      .then((r) => {
        setVisionConfigured(r.system_info.vision_configured);
      })
      .catch(() => {
        setVisionConfigured(false);
      });
  }, []);

  // Update arena rect whenever layout changes (needed for drag boundary clamping)
  const refreshCoords = useCallback(() => {
    if (arenaRef.current) {
      setArenaRect(arenaRef.current.getBoundingClientRect());
    }
  }, []);

  useEffect(() => {
    refreshCoords();
    window.addEventListener("resize", refreshCoords);
    return () => window.removeEventListener("resize", refreshCoords);
  }, [ballPos, refreshCoords]);

  // Mouse drag support (for manual testing)
  const dragging = useRef(false);
  const dragOffset = useRef({ x: 0, y: 0 });

  const checkDrop = useCallback(() => {
    if (!targetRef.current || !arenaRef.current) return;
    const tr = targetRef.current.getBoundingClientRect();
    const ar = arenaRef.current.getBoundingClientRect();
    // Read from ballPosRef (always up-to-date) to avoid stale closure issue.
    // ballPos state may lag behind when checkDrop is called from setTimeout.
    const pos = ballPosRef.current;
    const bx = pos.x + 20;
    const by = pos.y + 20;
    const tx1 = tr.left - ar.left;
    const ty1 = tr.top - ar.top;
    const tx2 = tx1 + tr.width;
    const ty2 = ty1 + tr.height;
    if (bx >= tx1 && bx <= tx2 && by >= ty1 && by <= ty2) {
      setDragState("success");
    } else {
      setDragState("fail");
    }
  }, []);

  // Arena-level mousedown: used by UIA agent whose click may not land exactly on the ball.
  // If the click is within 60px of the ball center, treat it as a ball grab.
  const onArenaMouseDown = (e: React.MouseEvent) => {
    if (!arenaRect) return;
    const bx = ballPos.x + 20; // ball center x (relative to arena)
    const by = ballPos.y + 20; // ball center y
    const cx = e.clientX - arenaRect.left;
    const cy = e.clientY - arenaRect.top;
    const dist = Math.sqrt((cx - bx) ** 2 + (cy - by) ** 2);
    if (dist <= 60) {
      e.preventDefault();
      dragging.current = true;
      // Offset from ball top-left so the ball follows the cursor naturally
      dragOffset.current = { x: cx - ballPos.x, y: cy - ballPos.y };
      setDragState("idle");
    }
  };

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (!dragging.current || !arenaRect) return;
      const nx = e.clientX - arenaRect.left - dragOffset.current.x;
      const ny = e.clientY - arenaRect.top - dragOffset.current.y;
      const newPos = { x: Math.max(0, Math.min(nx, arenaRect.width - 40)), y: Math.max(0, Math.min(ny, arenaRect.height - 40)) };
      ballPosRef.current = newPos;
      setBallPos(newPos);
    };
    const onUp = () => {
      if (!dragging.current) return;
      dragging.current = false;
      checkDrop();
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => { window.removeEventListener("mousemove", onMove); window.removeEventListener("mouseup", onUp); };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [arenaRect, checkDrop]);

  const reset = () => {
    ballPosRef.current = { x: 60, y: 120 };
    setBallPos({ x: 60, y: 120 });
    setDragState("idle");
    setTestResult(null);
    setTimeout(refreshCoords, 50);
  };

  const runTest = async () => {
    if (testRunning || !visionConfigured) return;
    setTestRunning(true);
    setTestResult(null);
    setDragState("idle");
    try {
      const result = await invoke<UiaDragTestResult>("run_uia_drag_test");
      setTestResult(result);
      setTimeout(() => {
        checkDrop();
      }, 500);
    } catch (e) {
      setTestResult({
        passed: false,
        response_text: "",
        error: String(e),
        duration_ms: 0,
        tool_calls: [],
      });
    } finally {
      setTestRunning(false);
    }
  };

  return (
    <div className="dbg-uia-panel">
      <div className="dbg-uia-header">
        <div>
          <div className="dbg-uia-title">{t("debug.uiaTitle")}</div>
          <div className="dbg-uia-subtitle">{t("debug.uiaSubtitle")}</div>
        </div>
        <div className="dbg-uia-header-actions">
          <button className="dbg-btn dbg-btn-secondary" onClick={reset} disabled={testRunning}>
            {t("debug.uiaReset")}
          </button>
          <button
            className={`dbg-btn ${visionConfigured ? "dbg-btn-primary" : "dbg-btn-disabled"}`}
            onClick={runTest}
            disabled={!visionConfigured || testRunning}
            title={
              !visionConfigured ? t("debug.uiaVisionRequired")
              : undefined
            }
          >
            {testRunning ? t("debug.uiaRunning") : t("debug.uiaRunTest")}
          </button>
        </div>
      </div>

      {/* Vision not available warning */}
      {visionConfigured === false && (
        <div className="dbg-uia-vision-warning">
          <div className="dbg-uia-vision-warning-title">⚠ {t("debug.uiaVisionRequired")}</div>
          <div className="dbg-uia-vision-warning-hint">{t("debug.uiaVisionRequiredHint")}</div>
        </div>
      )}

      {dragState === "success" && (
        <div className="dbg-uia-status dbg-uia-status-success">✓ {t("debug.uiaSuccess")}</div>
      )}
      {dragState === "fail" && testResult && (
        <div className="dbg-uia-status dbg-uia-status-fail">✗ {t("debug.uiaFail")}</div>
      )}

      {/* Arena */}
      <div
        className="dbg-uia-arena"
        ref={arenaRef}
        onMouseDown={onArenaMouseDown}
        onDragStart={(e) => e.preventDefault()}
      >
        {/* Ball */}
        <div
          ref={ballRef}
          className={`dbg-uia-ball ${dragState === "success" ? "dbg-uia-ball-done" : ""}`}
          style={{ left: ballPos.x, top: ballPos.y }}
          onDragStart={(e) => e.preventDefault()}
        >
          <span className="dbg-uia-ball-label">🟠</span>
        </div>

        {/* Target zone */}
        <div ref={targetRef} className="dbg-uia-target">
          <span className="dbg-uia-target-label">{t("debug.uiaTarget")}</span>
        </div>

      </div>

      {/* Running progress bar (below arena, so agent can still see the arena) */}
      {testRunning && (
        <div className="dbg-uia-progress">
          <div className="dbg-uia-progress-bar" />
          <div className="dbg-uia-progress-text">{t("debug.uiaRunning")}</div>
        </div>
      )}

      {/* Agent test result — pass/fail is determined by checkDrop() (visual position),
          NOT by the agent's text reply which can hallucinate success */}
      {testResult && (
        <div className={`dbg-uia-result ${dragState === "success" ? "dbg-uia-result-pass" : "dbg-uia-result-fail"}`}>
          <div className="dbg-uia-result-header">
            <span className="dbg-uia-result-badge">
              {dragState === "success" ? `✓ ${t("debug.uiaTestPassed")}` : `✗ ${t("debug.uiaTestFailed")}`}
            </span>
            <span className="dbg-uia-result-meta">
              {ms(testResult.duration_ms)} · {testResult.tool_calls.length} tools
            </span>
          </div>
          {testResult.error && (
            <div className="dbg-uia-result-error">{testResult.error}</div>
          )}
          {testResult.response_text && (
            <div className="dbg-uia-result-body">
              <div className="dbg-uia-result-label">{t("debug.uiaAgentResult")}</div>
              <pre className="dbg-uia-result-text">{testResult.response_text}</pre>
            </div>
          )}
        </div>
      )}

      {/* Instructions */}
      <div className="dbg-uia-instructions">
        <div className="dbg-uia-instructions-title">{t("debug.uiaHowTo")}</div>
        <ol className="dbg-uia-instructions-list">
          <li>{t("debug.uiaStep1")}</li>
          <li>{t("debug.uiaStep2")}</li>
          <li>{t("debug.uiaStep3")}</li>
          <li>{t("debug.uiaStep4")}</li>
        </ol>
      </div>
    </div>
  );
}

// ─── Multi-Agent Test Panel ───────────────────────────────────────────────────

interface TrialStep {
  name: string;
  koi_name: string;
  task: string;
  success: boolean;
  reply_preview: string;
  reply_preview_key?: string | null;
  reply_preview_params?: Record<string, unknown> | null;
  duration_ms: number;
}

interface TrialStatus {
  phase: string;
  pool_id: string;
  koi_ids: string[];
  steps: TrialStep[];
  completed: boolean;
  error: string | null;
  error_key?: string | null;
  error_params?: Record<string, unknown> | null;
}

function localizeTrialPhase(t: (key: string, options?: any) => string, phase: string): string {
  const key = `debug.multiAgentPhase_${phase}`;
  const translated = t(key);
  return translated === key ? phase : translated;
}

function localizeTrialStepName(t: (key: string, options?: any) => string, name: string): string {
  const key = `debug.multiAgentStep_${name}`;
  const translated = t(key);
  return translated === key ? name : translated;
}

function localizeMessage(
  t: (key: string, options?: any) => string,
  key: string | null | undefined,
  params: Record<string, unknown> | null | undefined,
  fallback: string,
): string {
  if (!key) return fallback;
  const translated = t(key, params ?? {});
  return translated === key ? fallback : translated;
}

function TrialMessageBubble({ msg, kois }: { msg: PoolMessage; kois: KoiWithStats[] }) {
  const sender = kois.find((k) => k.id === msg.sender_id);
  const isPisci = msg.sender_id === "pisci";
  const icon = isPisci ? "🐋" : sender?.icon ?? "🐟";
  const color = isPisci ? "#7c3aed" : sender?.color ?? "#6b7280";
  const name = isPisci ? "Pisci" : sender?.name ?? msg.sender_id;

  const time = (() => {
    const d = new Date(msg.created_at);
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  })();

  const isEvent = ["task_assign", "task_claimed", "task_blocked", "task_done", "status_update"].includes(msg.msg_type);

  if (isEvent) {
    const eventIcons: Record<string, string> = {
      task_assign: "📋", task_claimed: "✋", task_blocked: "🚫",
      task_done: "✅", status_update: "📡",
    };
    return (
      <div className="dbg-trial-event">
        <span className="dbg-trial-event-icon">{eventIcons[msg.msg_type] ?? "•"}</span>
        <span className="dbg-trial-event-sender" style={{ color }}>{icon} {name}</span>
        <span className="dbg-trial-event-text">{msg.content.length > 120 ? msg.content.slice(0, 120) + "…" : msg.content}</span>
        <span className="dbg-trial-event-time">{time}</span>
      </div>
    );
  }

  return (
    <div className="dbg-trial-msg">
      <div className="dbg-trial-msg-bar" style={{ background: color }} />
      <div className="dbg-trial-msg-body">
        <div className="dbg-trial-msg-header">
          <span className="dbg-trial-msg-icon">{icon}</span>
          <span className="dbg-trial-msg-name" style={{ color }}>{name}</span>
          <span className="dbg-trial-msg-time">{time}</span>
        </div>
        <div className="dbg-trial-msg-text">{msg.content}</div>
      </div>
    </div>
  );
}

const PHASE_ICONS: Record<string, string> = {
  setup: "⚙️", pool_ready: "🏊", architect: "🏗️",
  coder: "💻", reviewer: "🔍", completed: "✅", error: "❌", done: "✅",
};

function MultiAgentTestPanel() {
  const { t } = useTranslation();
  const [trialResult, setTrialResult] = useState<TrialStatus | null>(null);
  const [runningTrial, setRunningTrial] = useState(false);

  const [trialMessages, setTrialMessages] = useState<PoolMessage[]>([]);
  const [trialPhase, setTrialPhase] = useState("");
  const [trialPhaseDetail, setTrialPhaseDetail] = useState("");
  const [trialKois, setTrialKois] = useState<KoiWithStats[]>([]);
  const trialScrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (trialScrollRef.current) {
      trialScrollRef.current.scrollTop = trialScrollRef.current.scrollHeight;
    }
  }, [trialMessages.length]);

  useEffect(() => {
    let unlistenProgress: (() => void) | null = null;
    let unlistenMessages: (() => void) | null = null;

    listen<{ phase: string; detail: string; pool_id?: string }>(
      "collab_trial_progress",
      async (e) => {
        setTrialPhase(e.payload.phase);
        setTrialPhaseDetail(e.payload.detail);

        if (e.payload.pool_id && !unlistenMessages) {
          const pid = e.payload.pool_id;
          try {
            const msgs = await poolApi.getMessages({ session_id: pid, limit: 200 });
            setTrialMessages(msgs);
          } catch { /* ignore */ }
          try {
            const fn = await poolApi.onMessage(pid, (msg) => {
              setTrialMessages((prev) => {
                if (prev.some((m) => m.id === msg.id)) return prev;
                return [...prev, msg];
              });
            });
            unlistenMessages = fn;
          } catch { /* ignore */ }
        }

        if (e.payload.pool_id && trialKois.length === 0) {
          try {
            const list = await koiApi.list();
            setTrialKois(list);
          } catch { /* ignore */ }
        }
      },
    ).then((fn) => { unlistenProgress = fn; });

    return () => {
      unlistenProgress?.();
      unlistenMessages?.();
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const handleRunTrial = async () => {
    setRunningTrial(true);
    setTrialResult(null);
    setTrialMessages([]);
    setTrialPhase("setup");
    setTrialPhaseDetail("");
    try {
      const list = await koiApi.list();
      setTrialKois(list);
    } catch { /* ignore */ }
    try {
      const result = await invoke<TrialStatus>("run_collaboration_trial");
      setTrialResult(result);
      if (result.pool_id) {
        try {
          const msgs = await poolApi.getMessages({ session_id: result.pool_id, limit: 200 });
          setTrialMessages(msgs);
        } catch { /* ignore */ }
      }
    } catch (e: any) {
      setTrialResult({
        phase: "error", pool_id: "", koi_ids: [],
        steps: [], completed: false, error: `${e}`,
      });
    } finally {
      setRunningTrial(false);
    }
  };

  const showChat = runningTrial || trialMessages.length > 0;

  return (
    <div className="dbg-multiagent">
      <div className="dbg-multiagent-section">
        <h3>{t("debug.multiAgentTrialTitle")}</h3>
        <p className="dbg-multiagent-desc">
          {t("debug.multiAgentTrialDesc")}
        </p>
        <button
          className="dbg-btn dbg-btn-primary"
          onClick={handleRunTrial}
          disabled={runningTrial}
        >
          {runningTrial ? t("debug.multiAgentRunningTrial") : t("debug.multiAgentRunTrial")}
        </button>

        {showChat && (
          <div className="dbg-trial-chat">
            <div className="dbg-trial-chat-header">
              <span className="dbg-trial-chat-title">
                💬 {t("debug.trialChatTitle")}
              </span>
              {runningTrial && trialPhase && (
                <span className="dbg-trial-phase">
                  {PHASE_ICONS[trialPhase] ?? "⏳"} {localizeTrialPhase(t, trialPhase)}
                  {trialPhaseDetail && <span className="dbg-trial-phase-detail"> — {trialPhaseDetail}</span>}
                </span>
              )}
              {trialResult && !runningTrial && (
                <span className={`dbg-trial-verdict ${trialResult.completed ? "dbg-trial-verdict-pass" : "dbg-trial-verdict-fail"}`}>
                  {trialResult.completed ? "✅ " + t("debug.multiAgentAllPassed") : "⚠️ " + t("debug.multiAgentIncomplete")}
                </span>
              )}
            </div>

            <div className="dbg-trial-chat-scroll" ref={trialScrollRef}>
              {trialMessages.length === 0 && runningTrial && (
                <div className="dbg-trial-chat-empty">
                  <span className="dbg-trial-chat-empty-icon">⏳</span>
                  <span>{t("debug.trialChatWaiting")}</span>
                </div>
              )}
              {trialMessages.map((msg) => (
                <TrialMessageBubble key={msg.id} msg={msg} kois={trialKois} />
              ))}
              {runningTrial && trialMessages.length > 0 && (
                <div className="dbg-trial-typing">
                  <span className="dbg-trial-typing-dots">
                    <span /><span /><span />
                  </span>
                  {trialPhase && <span className="dbg-trial-typing-label">{PHASE_ICONS[trialPhase] ?? "⏳"} {localizeTrialPhase(t, trialPhase)}…</span>}
                </div>
              )}
            </div>

            {trialResult && trialResult.steps.length > 0 && (
              <div className="dbg-trial-chat-footer">
                {trialResult.steps.map((s, i) => (
                  <div key={i} className={`dbg-multiagent-row ${s.success ? "dbg-row-pass" : "dbg-row-fail"}`}>
                    <span className="dbg-multiagent-status">{s.success ? "✅" : "❌"}</span>
                    <span className="dbg-multiagent-name">{s.koi_name}: {localizeTrialStepName(t, s.name)}</span>
                    <span className="dbg-multiagent-time">{ms(s.duration_ms)}</span>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}

        {trialResult && trialResult.error && (
          <div className="dbg-multiagent-error" style={{ marginTop: 8 }}>
            {localizeMessage(t, trialResult.error_key, trialResult.error_params, trialResult.error)}
          </div>
        )}
      </div>
    </div>
  );
}
