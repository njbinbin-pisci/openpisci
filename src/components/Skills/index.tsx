import { useEffect, useState, useCallback } from "react";
import { useDispatch, useSelector } from "react-redux";
import { useTranslation } from "react-i18next";
import { RootState, skillsActions } from "../../store";
import { skillsApi, SkillCatalogItem } from "../../services/tauri";

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

  // Catalog (detailed skill info from file system)
  const [catalog, setCatalog] = useState<SkillCatalogItem[]>([]);

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
      loadSkills();
    } catch (e) {
      setError(t("skills.installFailed", { error: String(e) }));
    } finally {
      setInstalling(false);
    }
  };

  const handleUninstall = async (skillName: string) => {
    if (!window.confirm(t("skills.uninstallConfirm", { name: skillName }))) return;
    setError(null);
    try {
      await skillsApi.uninstall(skillName);
      setSuccessMsg(t("skills.uninstallSuccess", { name: skillName }));
      loadSkills();
    } catch (e) {
      setError(t("skills.uninstallFailed", { error: String(e) }));
    }
  };

  const handleUpdate = async (skillName: string, source: string) => {
    if (!source.startsWith("http")) {
      setError("Update requires the original URL. Re-paste the URL in the install box.");
      return;
    }
    setInstallUrl(source);
  };

  const enabledCount = skills.filter((s) => s.enabled).length;

  const catalogByName = Object.fromEntries(catalog.map((c) => [c.name.toLowerCase(), c]));

  return (
    <div className="page">
      <div className="page-header">
        <h1 className="page-title">⚡ {t("skills.title")}</h1>
        <span className="badge badge-info">
          {t("skills.enabledCount", { enabled: enabledCount, total: skills.length })}
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
              onChange={(e) => setInstallUrl(e.target.value)}
              placeholder={t("skills.installPlaceholder")}
              onKeyDown={(e) => e.key === "Enter" && handleInstall()}
              disabled={installing}
            />
            <button
              className="btn btn-primary"
              onClick={handleInstall}
              disabled={installing || !installUrl.trim()}
              style={{ flexShrink: 0 }}
            >
              {installing ? t("skills.installing") : t("skills.installBtn")}
            </button>
          </div>
          <p style={{ fontSize: 11, color: "var(--text-muted)", marginTop: 6 }}>
            支持 GitHub raw URL / 直链 SKILL.md / 本地文件路径
          </p>
        </div>

        <p style={{ color: "var(--text-secondary)", marginBottom: 16, fontSize: 13 }}>
          {t("skills.description")}
        </p>

        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(300px, 1fr))", gap: 12 }}>
          {skills.map((skill) => {
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
