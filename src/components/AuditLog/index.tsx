import { useEffect, useState, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { auditApi, AuditEntry } from "../../services/tauri";
import ConfirmDialog from "../ConfirmDialog";

const TOOL_COLORS: Record<string, string> = {
  shell: "#e67e22",
  file_write: "#e74c3c",
  file_read: "#3498db",
  web_search: "#9b59b6",
  browser: "#1abc9c",
  uia: "#f39c12",
  screen_capture: "#2ecc71",
  powershell_query: "#e67e22",
  wmi: "#95a5a6",
  com: "#34495e",
  office: "#27ae60",
};

function toolColor(name: string) {
  return TOOL_COLORS[name] ?? "#7f8c8d";
}

export default function AuditLog() {
  const { t } = useTranslation();
  const [entries, setEntries] = useState<AuditEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [filterTool, setFilterTool] = useState("");
  const [showErrors, setShowErrors] = useState(false);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [clearing, setClearing] = useState(false);
  const [confirmClearOpen, setConfirmClearOpen] = useState(false);
  const [page, setPage] = useState(0);
  const PAGE_SIZE = 50;

  const load = useCallback(async (reset = false) => {
    setLoading(true);
    setError(null);
    try {
      const offset = reset ? 0 : page * PAGE_SIZE;
      const data = await auditApi.list({
        tool_name: filterTool || undefined,
        limit: PAGE_SIZE,
        offset,
      });
      if (reset) {
        setEntries(data);
        setPage(0);
      } else {
        setEntries(data);
      }
    } catch (e) {
      setError(t("audit.failedLoad", { error: String(e) }));
    } finally {
      setLoading(false);
    }
  }, [filterTool, page, t]);

  useEffect(() => { load(true); }, [filterTool]);

  const handleClearConfirmed = async () => {
    setClearing(true);
    try {
      await auditApi.clear();
      setEntries([]);
    } catch (e) {
      setError(t("audit.failedClear", { error: String(e) }));
    } finally {
      setClearing(false);
      setConfirmClearOpen(false);
    }
  };

  const toggleExpand = (id: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const displayed = showErrors ? entries.filter((e) => e.is_error) : entries;
  const allTools = Array.from(new Set(entries.map((e) => e.tool_name))).sort();

  return (
    <div className="page">
      <div className="page-header">
        <h1 className="page-title">🔍 {t("audit.title")}</h1>
        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <button className="btn" onClick={() => load(true)} disabled={loading}
            style={{ background: "var(--bg-secondary)", border: "1px solid var(--border)", color: "var(--text-secondary)", fontSize: 13 }}>
            {loading ? t("common.loading") : t("common.refresh")}
          </button>
          <button className="btn" onClick={() => setConfirmClearOpen(true)} disabled={clearing}
            style={{ background: "rgba(220,53,69,0.12)", border: "1px solid rgba(220,53,69,0.3)", color: "#ff6b6b", fontSize: 13 }}>
            {clearing ? t("audit.clearing") : t("audit.clearLog")}
          </button>
        </div>
      </div>

      {error && (
        <div style={{ margin: "0 0 12px", padding: "8px 14px", background: "rgba(220,53,69,0.12)", borderLeft: "3px solid #dc3545", color: "#ff6b6b", fontSize: 13, display: "flex", justifyContent: "space-between" }}>
          <span>{error}</span>
          <button onClick={() => setError(null)} style={{ background: "none", border: "none", color: "#ff6b6b", cursor: "pointer" }}>✕</button>
        </div>
      )}

      <div style={{ display: "flex", gap: 10, marginBottom: 16, flexWrap: "wrap", alignItems: "center" }}>
        <select className="input" value={filterTool} onChange={(e) => setFilterTool(e.target.value)}
          style={{ width: "auto", minWidth: 140, fontSize: 13 }}>
          <option value="">{t("audit.allTools")}</option>
          {allTools.map((tool) => <option key={tool} value={tool}>{tool}</option>)}
        </select>
        <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 13, color: "var(--text-secondary)", cursor: "pointer" }}>
          <input type="checkbox" checked={showErrors} onChange={(e) => setShowErrors(e.target.checked)} />
          {t("audit.errorsOnly")}
        </label>
        <span style={{ fontSize: 12, color: "var(--text-muted)", marginLeft: "auto" }}>
          {t("audit.totalRecords", { count: displayed.length })}
        </span>
      </div>

      <div className="page-body" style={{ padding: 0 }}>
        {displayed.length === 0 && !loading && (
          <div style={{ textAlign: "center", padding: "60px 0", color: "var(--text-muted)" }}>
            <div style={{ fontSize: 32, marginBottom: 8 }}>📋</div>
            <div>{t("audit.noRecords")}</div>
            <div style={{ fontSize: 12, marginTop: 4 }}>{t("audit.noRecordsDesc")}</div>
          </div>
        )}

        {displayed.map((entry) => {
          const isExp = expanded.has(entry.id);
          const ts = new Date(entry.timestamp).toLocaleString();
          return (
            <div key={entry.id}
              style={{ borderBottom: "1px solid var(--border)", padding: "10px 16px", background: entry.is_error ? "rgba(220,53,69,0.04)" : "transparent", cursor: "pointer" }}
              onClick={() => toggleExpand(entry.id)}
            >
              <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                <span style={{
                  fontSize: 11, fontWeight: 600, padding: "2px 7px", borderRadius: 4,
                  background: toolColor(entry.tool_name) + "22",
                  color: toolColor(entry.tool_name),
                  border: `1px solid ${toolColor(entry.tool_name)}44`,
                  minWidth: 80, textAlign: "center",
                }}>
                  {entry.tool_name}
                </span>
                <span style={{ fontSize: 13, color: "var(--text-primary)", fontWeight: 500, flex: 1 }}>
                  {entry.action}
                </span>
                {entry.is_error
                  ? <span style={{ fontSize: 11, color: "#ff6b6b", fontWeight: 600 }}>{t("audit.statusError")}</span>
                  : <span style={{ fontSize: 11, color: "#28a745" }}>{t("audit.statusSuccess")}</span>
                }
                <span style={{ fontSize: 11, color: "var(--text-muted)", whiteSpace: "nowrap" }}>{ts}</span>
                <span style={{ color: "var(--text-muted)", fontSize: 12 }}>{isExp ? "▲" : "▼"}</span>
              </div>

              {isExp && (
                <div style={{ marginTop: 10, fontSize: 12, display: "flex", flexDirection: "column", gap: 6 }}>
                  {entry.input_summary && (
                    <div>
                      <span style={{ color: "var(--text-muted)", fontWeight: 600 }}>{t("audit.inputLabel")}</span>
                      <code style={{ color: "var(--text-secondary)", background: "var(--bg-secondary)", padding: "2px 6px", borderRadius: 3, wordBreak: "break-all" }}>
                        {entry.input_summary}
                      </code>
                    </div>
                  )}
                  {entry.result_summary && (
                    <div>
                      <span style={{ color: "var(--text-muted)", fontWeight: 600 }}>{t("audit.resultLabel")}</span>
                      <code style={{ color: entry.is_error ? "#ff6b6b" : "var(--text-secondary)", background: "var(--bg-secondary)", padding: "2px 6px", borderRadius: 3, wordBreak: "break-all" }}>
                        {entry.result_summary}
                      </code>
                    </div>
                  )}
                  <div style={{ color: "var(--text-muted)", fontSize: 11 }}>
                    {t("audit.session")}{entry.session_id.slice(0, 8)}...
                  </div>
                </div>
              )}
            </div>
          );
        })}

        {displayed.length === PAGE_SIZE && (
          <div style={{ textAlign: "center", padding: "12px 0" }}>
            <button className="btn" onClick={() => { setPage((p) => p + 1); load(); }}
              style={{ background: "var(--bg-secondary)", border: "1px solid var(--border)", color: "var(--text-secondary)", fontSize: 13 }}>
              {t("common.loadMore")}
            </button>
          </div>
        )}
      </div>

      <ConfirmDialog
        open={confirmClearOpen}
        title={t("audit.clearLog")}
        message={t("audit.confirmClear")}
        confirmLabel={t("audit.clearLog")}
        cancelLabel={t("common.cancel")}
        loading={clearing}
        onConfirm={handleClearConfirmed}
        onCancel={() => !clearing && setConfirmClearOpen(false)}
      />
    </div>
  );
}
