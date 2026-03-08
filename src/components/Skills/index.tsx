import { useEffect, useState, useCallback } from "react";
import { useDispatch, useSelector } from "react-redux";
import { useTranslation } from "react-i18next";
import { RootState, skillsActions } from "../../store";
import { skillsApi, clawHubApi, SkillCatalogItem, ClawHubSkill, SkillCompatibilityCheck } from "../../services/tauri";
import ConfirmDialog from "../ConfirmDialog";

const SOURCE_BADGE: Record<string, { label: string; color: string }> = {
  builtin:   { label: "builtin",   color: "var(--text-muted)" },
  installed: { label: "installed", color: "#28a745" },
  workspace: { label: "workspace", color: "#ffc107" },
  registry:  { label: "registry",  color: "var(--accent)" },
};

export default function Skills() {
  const { t } = useTranslation();
  const dispatch = useDispatch();
  const { skills } = useSelector((s: RootState) => s.skills);
  const [error, setError] = useState<string | null>(null);
  const [successMsg, setSuccessMsg] = useState<string | null>(null);

  // Installation state
  const [installUrl, setInstallUrl] = useState("");
  const [installing, setInstalling] = useState(false);
  const [compatCheck, setCompatCheck] = useState<SkillCompatibilityCheck | null>(null);
  const [compatChecking, setCompatChecking] = useState(false);

  // Catalog (detailed skill info from file system)
  const [catalog, setCatalog] = useState<SkillCatalogItem[]>([]);

  // Uninstall confirmation dialog
  const [uninstallTarget, setUninstallTarget] = useState<string | null>(null);
  const [uninstalling, setUninstalling] = useState(false);

  // ClawHub marketplace
  const [hubTab, setHubTab] = useState<"local" | "hub">("local");
  const [hubQuery, setHubQuery] = useState("");
  const [hubResults, setHubResults] = useState<ClawHubSkill[]>([]);
  const [hubSearching, setHubSearching] = useState(false);
  const [hubError, setHubError] = useState<string | null>(null);
  const [hubInstalling, setHubInstalling] = useState<string | null>(null);

  const loadSkills = useCallback(() => {
    skillsApi.list().then(({ skills }) => {
      dispatch(skillsActions.setSkills(skills));
    }).catch((e) => setError(t("skills.failedLoad", { error: String(e) })));

    skillsApi.catalog().then(setCatalog).catch(() => {});
  }, [dispatch, t]);

  useEffect(() => {
    loadSkills();
  }, [loadSkills]);

  const handleToggle = async (id: string, enabled: boolean) => {
    try {
      await skillsApi.toggle(id, enabled);
      dispatch(skillsActions.toggleSkill({ id, enabled }));
    } catch (e) {
      setError(t("skills.failedToggle", { error: String(e) }));
    }
  };

  const handleCheckCompat = useCallback(async (src: string) => {
    if (!src.trim()) { setCompatCheck(null); return; }
    setCompatChecking(true);
    setCompatCheck(null);
    try {
      const result = await skillsApi.checkCompat(src.trim());
      setCompatCheck(result);
    } catch {
      setCompatCheck(null);
    } finally {
      setCompatChecking(false);
    }
  }, []);

  const handleInstall = async () => {
    const src = installUrl.trim();
    if (!src) return;
    setInstalling(true);
    setError(null);
    setSuccessMsg(null);
    try {
      const skill = await skillsApi.install(src);
      setSuccessMsg(t("skills.installSuccess", { name: skill.name }));
      setInstallUrl("");
      setCompatCheck(null);
      loadSkills();
    } catch (e) {
      setError(t("skills.installFailed", { error: String(e) }));
    } finally {
      setInstalling(false);
    }
  };

  const handleUninstall = (skillName: string) => {
    setUninstallTarget(skillName);
  };

  const doUninstall = async () => {
    if (!uninstallTarget) return;
    const skillName = uninstallTarget;
    setUninstalling(true);
    setError(null);
    try {
      await skillsApi.uninstall(skillName);
      setSuccessMsg(t("skills.uninstallSuccess", { name: skillName }));
      loadSkills();
    } catch (e) {
      setError(t("skills.uninstallFailed", { error: String(e) }));
    } finally {
      setUninstalling(false);
      setUninstallTarget(null);
    }
  };

  const handleUpdate = async (skillName: string, source: string) => {
    if (!source.startsWith("http")) {
      setError("Update requires the original URL. Re-paste the URL in the install box.");
      return;
    }
    setInstallUrl(source);
  };

  const handleHubSearch = useCallback(async () => {
    const q = hubQuery.trim();
    setHubSearching(true);
    setHubError(null);
    try {
      const result = await clawHubApi.search(q, 20);
      if (result.items.length === 0) {
        setHubError(t("skills.hubNoResults"));
        setHubResults([]);
        return;
      }
      // Show results immediately, then enrich with compat info in background
      setHubResults(result.items);

      // Fire-and-forget: pre-check compatibility for each skill that has a skill_url
      result.items.forEach(async (skill, idx) => {
        if (!skill.skill_url) return;
        try {
          const compat = await skillsApi.checkCompat(skill.skill_url);
          setHubResults((prev) => {
            const next = [...prev];
            if (next[idx]?.slug === skill.slug) {
              next[idx] = {
                ...next[idx],
                platform: compat.issues.some((i) => i.includes("平台")) ? ["linux/macos"] : next[idx].platform,
                compatible: compat.compatible,
                compat_issues: compat.issues,
              };
            }
            return next;
          });
        } catch {
          // compat check failed silently — don't block the UI
        }
      });
    } catch (e) {
      setHubError(t("skills.hubSearchFailed", { error: String(e) }));
    } finally {
      setHubSearching(false);
    }
  }, [hubQuery, t]);

  const handleHubInstall = async (skill: ClawHubSkill) => {
    // Block install if we already know it's incompatible
    if (skill.compatible === false) {
      setError(t("skills.installFailed", { error: skill.compat_issues.join("; ") }));
      return;
    }
    setHubInstalling(skill.slug);
    setError(null);
    setSuccessMsg(null);
    try {
      const installed = await clawHubApi.install(skill.slug, skill.version);
      setSuccessMsg(t("skills.installSuccess", { name: installed.name }));
      loadSkills();
    } catch (e) {
      setError(t("skills.installFailed", { error: String(e) }));
    } finally {
      setHubInstalling(null);
    }
  };

  const enabledCount = skills.filter((s) => s.enabled).length;

  const catalogByName = Object.fromEntries(catalog.map((c) => [c.name.toLowerCase(), c]));
  // Hide built-in pseudo skills from user-facing UI; only show user/workspace/registry skills.
  const visibleSkills = skills.filter((skill) => {
    const source = catalogByName[skill.name.toLowerCase()]?.source ?? "builtin";
    return source !== "builtin";
  });

  return (
    <div className="page">
      <div className="page-header">
        <h1 className="page-title">⚡ {t("skills.title")}</h1>
        <span className="badge badge-info">
          {t("skills.enabledCount", { enabled: visibleSkills.filter((s) => s.enabled).length, total: visibleSkills.length })}
        </span>
      </div>

      <div className="page-body">
        {error && (
          <div style={{ padding: "8px 14px", background: "rgba(220,53,69,0.15)", borderLeft: "3px solid #dc3545", color: "#ff6b6b", fontSize: "0.85rem", marginBottom: 12, display: "flex", justifyContent: "space-between" }}>
            <span>{error}</span>
            <button onClick={() => setError(null)} style={{ background: "none", border: "none", color: "#ff6b6b", cursor: "pointer" }}>✕</button>
          </div>
        )}
        {successMsg && (
          <div style={{ padding: "8px 14px", background: "rgba(40,167,69,0.12)", borderLeft: "3px solid #28a745", color: "#28a745", fontSize: "0.85rem", marginBottom: 12, display: "flex", justifyContent: "space-between" }}>
            <span>{successMsg}</span>
            <button onClick={() => setSuccessMsg(null)} style={{ background: "none", border: "none", color: "#28a745", cursor: "pointer" }}>✕</button>
          </div>
        )}

        <ConfirmDialog
          open={!!uninstallTarget}
          title={t("skills.uninstallConfirmTitle", { defaultValue: "确认卸载" })}
          message={t("skills.uninstallConfirm", { name: uninstallTarget ?? "" })}
          confirmLabel={t("skills.uninstallBtn", { defaultValue: "卸载" })}
          cancelLabel={t("common.cancel")}
          loading={uninstalling}
          onConfirm={doUninstall}
          onCancel={() => !uninstalling && setUninstallTarget(null)}
        />

        {/* Tab switcher */}
        <div style={{ display: "flex", gap: 4, marginBottom: 20, borderBottom: "1px solid var(--border)", paddingBottom: 0 }}>
          {(["local", "hub"] as const).map((tab) => (
            <button
              key={tab}
              onClick={() => setHubTab(tab)}
              style={{
                padding: "6px 16px",
                background: "none",
                border: "none",
                borderBottom: hubTab === tab ? "2px solid var(--accent)" : "2px solid transparent",
                color: hubTab === tab ? "var(--accent)" : "var(--text-secondary)",
                cursor: "pointer",
                fontWeight: hubTab === tab ? 600 : 400,
                fontSize: 13,
                marginBottom: -1,
              }}
            >
              {tab === "local" ? `⚡ ${t("skills.tabLocal")}` : `🛒 ${t("skills.tabHub")}`}
            </button>
          ))}
        </div>

        {hubTab === "local" && (
          <>
            {/* Install panel */}
            <div style={{ marginBottom: 24, padding: "14px 16px", border: "1px solid var(--border)", borderRadius: 8, background: "var(--bg-secondary)" }}>
              <div style={{ fontWeight: 600, color: "var(--text-primary)", marginBottom: 8, fontSize: 14 }}>
                ⬇ {t("skills.installTitle")}
              </div>
              <div style={{ display: "flex", gap: 8 }}>
                <input
                  className="input"
                  style={{ flex: 1 }}
                  value={installUrl}
                  onChange={(e) => { setInstallUrl(e.target.value); setCompatCheck(null); }}
                  placeholder={t("skills.installPlaceholder")}
                  onKeyDown={(e) => e.key === "Enter" && handleInstall()}
                  disabled={installing}
                />
                <button
                  className="btn btn-secondary"
                  onClick={() => handleCheckCompat(installUrl)}
                  disabled={compatChecking || installing || !installUrl.trim()}
                  style={{ flexShrink: 0 }}
                  title={t("skills.checkCompatTitle")}
                >
                  {compatChecking ? "…" : t("skills.checkCompatBtn")}
                </button>
                <button
                  className="btn btn-primary"
                  onClick={handleInstall}
                  disabled={installing || !installUrl.trim() || compatCheck?.compatible === false}
                  style={{ flexShrink: 0 }}
                >
                  {installing ? t("skills.installing") : t("skills.installBtn")}
                </button>
              </div>
              {/* Compatibility check result */}
              {compatCheck && (
                <div style={{
                  marginTop: 8, padding: "8px 12px", borderRadius: 6, fontSize: 12,
                  background: compatCheck.compatible ? "rgba(40,167,69,0.1)" : "rgba(220,53,69,0.1)",
                  border: `1px solid ${compatCheck.compatible ? "#28a745" : "#dc3545"}`,
                  color: compatCheck.compatible ? "#28a745" : "#ff6b6b",
                }}>
                  {compatCheck.compatible
                    ? `✓ ${t("skills.compatOk")}`
                    : `✗ ${t("skills.compatFail")}`}
                  {compatCheck.issues.map((issue, i) => (
                    <div key={i} style={{ marginTop: 4, color: "#ff6b6b" }}>• {issue}</div>
                  ))}
                  {compatCheck.warnings.map((w, i) => (
                    <div key={i} style={{ marginTop: 4, color: "#ffc107" }}>⚠ {w}</div>
                  ))}
                </div>
              )}
              <p style={{ fontSize: 11, color: "var(--text-muted)", marginTop: 6 }}>
                {t("skills.installHint")}
              </p>
            </div>

            <p style={{ color: "var(--text-secondary)", marginBottom: 16, fontSize: 13 }}>
              {t("skills.description")}
            </p>

            {visibleSkills.length === 0 ? (
              <div className="empty-state" style={{ padding: "28px 16px" }}>
                <div className="empty-state-title">{t("skills.emptyTitle")}</div>
                <div className="empty-state-desc">{t("skills.emptyDesc")}</div>
              </div>
            ) : (
            <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(320px, 1fr))", gap: 12 }}>
              {visibleSkills.map((skill) => {
                const catalogEntry = catalogByName[skill.name.toLowerCase()];
                const source = catalogEntry?.source ?? "builtin";
                const badge = SOURCE_BADGE[source] ?? SOURCE_BADGE.builtin;
                const canUninstall = source === "installed" || source === "workspace";

                return (
                  <div key={skill.id} className="card skill-card" style={{ opacity: skill.enabled ? 1 : 0.6 }}>
                    <div style={{ display: "flex", alignItems: "flex-start", justifyContent: "space-between", gap: 12 }}>
                      <div style={{ flex: 1, minWidth: 0 }}>
                        <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 4 }}>
                          <span style={{ fontSize: 20 }}>{skill.icon}</span>
                          <span style={{ fontWeight: 600, color: "var(--text-primary)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{skill.name}</span>
                          <span style={{ fontSize: 10, padding: "1px 6px", borderRadius: 10, background: "var(--bg-tertiary)", color: badge.color, flexShrink: 0, border: `1px solid ${badge.color}` }}>
                            {badge.label}
                          </span>
                        </div>
                        <p style={{ fontSize: 12, color: "var(--text-secondary)", margin: 0 }}>{skill.description}</p>
                        {catalogEntry && catalogEntry.tools.length > 0 && (
                          <p style={{ fontSize: 11, color: "var(--text-muted)", margin: "4px 0 0" }}>
                            {t("skills.toolsBadge", { tools: catalogEntry.tools.join(", ") })}
                          </p>
                        )}
                        {catalogEntry && catalogEntry.permissions.length > 0 && (
                          <p style={{ fontSize: 11, color: "#ffc107", margin: "2px 0 0" }}>
                            ⚠ {t("skills.permissionsBadge", { perms: catalogEntry.permissions.join(", ") })}
                          </p>
                        )}
                        {catalogEntry && catalogEntry.platform.length > 0 && (
                          <p style={{ fontSize: 11, color: "var(--text-muted)", margin: "2px 0 0" }}>
                            🖥 {t("skills.platformBadge", { platform: catalogEntry.platform.join(", ") })}
                          </p>
                        )}
                        {catalogEntry && catalogEntry.dependencies.length > 0 && (
                          <p style={{ fontSize: 11, color: "var(--text-muted)", margin: "2px 0 0" }}>
                            📦 {t("skills.depsBadge", { deps: catalogEntry.dependencies.join(", ") })}
                          </p>
                        )}
                      </div>
                      <div style={{ display: "flex", flexDirection: "column", alignItems: "flex-end", gap: 8, flexShrink: 0 }}>
                        <label className="toggle">
                          <input
                            type="checkbox"
                            checked={skill.enabled}
                            onChange={(e) => handleToggle(skill.id, e.target.checked)}
                          />
                          <span className="toggle-slider" />
                        </label>
                        {canUninstall && (
                          <button
                            onClick={() => handleUninstall(skill.name)}
                            style={{ fontSize: 11, background: "none", border: "1px solid var(--border)", borderRadius: 4, padding: "2px 8px", color: "var(--text-muted)", cursor: "pointer" }}
                          >
                            {t("skills.uninstallBtn")}
                          </button>
                        )}
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
            )}
          </>
        )}

        {hubTab === "hub" && (
          <div>
            {/* ClawHub search */}
            <div style={{ marginBottom: 16 }}>
              <div style={{ fontWeight: 600, color: "var(--text-primary)", marginBottom: 8, fontSize: 14 }}>
                🔍 {t("skills.hubSearch")}
              </div>
              <div style={{ display: "flex", gap: 8 }}>
                <input
                  className="input"
                  style={{ flex: 1 }}
                  value={hubQuery}
                  onChange={(e) => setHubQuery(e.target.value)}
                  placeholder={t("skills.hubSearchPlaceholder")}
                  onKeyDown={(e) => e.key === "Enter" && handleHubSearch()}
                  disabled={hubSearching}
                />
                <button
                  className="btn btn-primary"
                  onClick={handleHubSearch}
                  disabled={hubSearching}
                  style={{ flexShrink: 0 }}
                >
                  {hubSearching ? t("common.loading") : t("common.search")}
                </button>
              </div>
              <p style={{ fontSize: 11, color: "var(--text-muted)", marginTop: 6 }}>
                {t("skills.hubHint")}
              </p>
            </div>

            {hubError && (
              <div style={{ padding: "8px 14px", background: "rgba(220,53,69,0.1)", borderLeft: "3px solid #dc3545", color: "#ff6b6b", fontSize: 12, marginBottom: 12 }}>
                {hubError}
              </div>
            )}

            {hubResults.length === 0 && !hubSearching && !hubError && (
              <div className="empty-state" style={{ padding: "28px 16px" }}>
                <div className="empty-state-title">{t("skills.hubEmpty")}</div>
                <div className="empty-state-desc">{t("skills.hubEmptyDesc")}</div>
              </div>
            )}

            <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(320px, 1fr))", gap: 12 }}>
              {hubResults.map((skill) => {
                const isInstalling = hubInstalling === skill.slug;
                const incompatible = skill.compatible === false;
                return (
                  <div
                    key={skill.slug}
                    className="card"
                    style={{
                      display: "flex", flexDirection: "column", gap: 8,
                      opacity: incompatible ? 0.75 : 1,
                      border: incompatible ? "1px solid rgba(220,53,69,0.4)" : undefined,
                    }}
                  >
                    <div style={{ display: "flex", alignItems: "flex-start", justifyContent: "space-between", gap: 8 }}>
                      <div style={{ flex: 1, minWidth: 0 }}>
                        <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 4 }}>
                          <span style={{ fontWeight: 600, color: "var(--text-primary)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                            {skill.name}
                          </span>
                          <span style={{ fontSize: 10, color: "var(--text-muted)", flexShrink: 0 }}>{/^\d/.test(skill.version) ? `v${skill.version}` : skill.version}</span>
                          {/* Compat badge — shown once check completes */}
                          {skill.compatible === true && (
                            <span style={{ fontSize: 10, color: "#28a745", background: "rgba(40,167,69,0.12)", padding: "1px 6px", borderRadius: 8, flexShrink: 0 }}>
                              ✓ {t("skills.compatOk")}
                            </span>
                          )}
                          {incompatible && (
                            <span style={{ fontSize: 10, color: "#ff6b6b", background: "rgba(220,53,69,0.12)", padding: "1px 6px", borderRadius: 8, flexShrink: 0 }}>
                              ✗ {t("skills.compatFail")}
                            </span>
                          )}
                          {skill.compatible === null && skill.skill_url && (
                            <span style={{ fontSize: 10, color: "var(--text-muted)", flexShrink: 0 }}>…</span>
                          )}
                        </div>
                        <p style={{ fontSize: 12, color: "var(--text-secondary)", margin: 0, lineHeight: 1.4 }}>
                          {skill.description || t("skills.noDescription")}
                        </p>
                        {/* Compat issues */}
                        {incompatible && skill.compat_issues.length > 0 && (
                          <div style={{ marginTop: 4, fontSize: 11, color: "#ff6b6b" }}>
                            {skill.compat_issues.map((issue, i) => (
                              <div key={i}>⚠ {issue}</div>
                            ))}
                          </div>
                        )}
                        {/* Platform / deps badges */}
                        {(skill.platform.length > 0 || skill.dependencies.length > 0) && (
                          <div style={{ display: "flex", flexWrap: "wrap", gap: 4, marginTop: 4 }}>
                            {skill.platform.length > 0 && (
                              <span style={{ fontSize: 10, color: "var(--text-muted)", background: "var(--bg-tertiary)", padding: "1px 6px", borderRadius: 8, border: "1px solid var(--border)" }}>
                                🖥 {skill.platform.join("/")}
                              </span>
                            )}
                            {skill.dependencies.map((dep) => (
                              <span key={dep} style={{ fontSize: 10, color: "var(--text-muted)", background: "var(--bg-tertiary)", padding: "1px 6px", borderRadius: 8, border: "1px solid var(--border)" }}>
                                📦 {dep}
                              </span>
                            ))}
                          </div>
                        )}
                      </div>
                      <button
                        className={`btn ${incompatible ? "btn-secondary" : "btn-primary"}`}
                        onClick={() => handleHubInstall(skill)}
                        disabled={isInstalling || incompatible}
                        title={incompatible ? skill.compat_issues.join("; ") : undefined}
                        style={{ flexShrink: 0, fontSize: 12, padding: "4px 12px" }}
                      >
                        {isInstalling ? t("skills.installing") : incompatible ? t("skills.compatFail") : t("skills.installBtn")}
                      </button>
                    </div>
                    <div style={{ display: "flex", alignItems: "center", gap: 12, fontSize: 11, color: "var(--text-muted)" }}>
                      <span>👤 {skill.author}</span>
                      <span>⭐ {skill.stars}</span>
                      {skill.tags.slice(0, 3).map((tag) => (
                        <span key={tag} style={{ padding: "1px 6px", background: "var(--bg-tertiary)", borderRadius: 10, border: "1px solid var(--border)" }}>
                          {tag}
                        </span>
                      ))}
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </div>

      <style>{`
        .toggle { position: relative; display: inline-block; width: 40px; height: 22px; flex-shrink: 0; }
        .toggle input { opacity: 0; width: 0; height: 0; }
        .toggle-slider { position: absolute; cursor: pointer; inset: 0; background: var(--bg-tertiary); border: 1px solid var(--border); border-radius: 100px; transition: 0.2s; }
        .toggle-slider::before { content: ""; position: absolute; width: 16px; height: 16px; left: 2px; top: 2px; background: var(--text-muted); border-radius: 50%; transition: 0.2s; }
        .toggle input:checked + .toggle-slider { background: var(--accent-dim); border-color: var(--accent); }
        .toggle input:checked + .toggle-slider::before { transform: translateX(18px); background: var(--accent); }
      `}</style>
    </div>
  );
}
