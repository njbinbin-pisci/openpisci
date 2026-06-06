import { useState, useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { FishDefinition, FishSource } from "../../services/tauri";
import "./Fish.css";

function sourceBadge(source: FishSource, t: (k: string) => string) {
  switch (source) {
    case "skill":
      return <span className="fish-card-badge badge-skill">{t("fish.badgeSkill")}</span>;
    case "user":
      return <span className="fish-card-badge badge-user">{t("fish.badgeUser")}</span>;
    case "builtin":
    default:
      return <span className="fish-card-badge badge-builtin">{t("fish.badgeBuiltin")}</span>;
  }
}

function FishCard({ fish }: { fish: FishDefinition }) {
  const { t } = useTranslation();
  return (
    <div className="fish-card">
      <div className="fish-card-header">
        <span className="fish-card-icon">{fish.icon}</span>
        <div className="fish-card-meta">
          <span className="fish-card-name">{fish.name}</span>
          {sourceBadge(fish.source ?? (fish.builtin ? "builtin" : "user"), t)}
        </div>
      </div>
      <p className="fish-card-desc">{fish.description}</p>
      <div className="fish-card-tools">
        {fish.tools.slice(0, 4).map((tool) => (
          <span key={tool} className="fish-tool-tag">{tool}</span>
        ))}
        {fish.tools.length > 4 && (
          <span className="fish-tool-tag">+{fish.tools.length - 4}</span>
        )}
      </div>
    </div>
  );
}

export default function FishPage() {
  const { t } = useTranslation();
  const [fishList, setFishList] = useState<FishDefinition[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [fishDir, setFishDir] = useState<string>("");

  const loadFish = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const list = await invoke<FishDefinition[]>("list_fish");
      setFishList(list);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadFish();
    invoke<string>("get_fish_dir").then(setFishDir).catch(() => {});
  }, [loadFish]);

  const builtinFish = fishList.filter((f) => (f.source ?? (f.builtin ? "builtin" : "user")) === "builtin");
  const skillFish = fishList.filter((f) => (f.source ?? "") === "skill");
  const userFish = fishList.filter((f) => (f.source ?? (f.builtin ? "builtin" : "user")) === "user");

  const renderFishGrid = (list: FishDefinition[]) => (
    <div className="fish-grid">
      {list.map((fish) => (
        <FishCard key={fish.id} fish={fish} />
      ))}
    </div>
  );

  return (
    <div className="page fish-page">
      <div className="page-header">
        <h1 className="page-title">🐠 {t("fish.title")}</h1>
        <div className="page-header-actions">
          <button type="button" className="btn-header" onClick={loadFish}>
            ↻ {t("fish.refresh")}
          </button>
        </div>
      </div>

      <div className="page-body fish-page-body">
      {error && (
        <div className="fish-error">
          <span>⚠️ {error}</span>
          <button onClick={() => setError(null)}>✕</button>
        </div>
      )}

      {loading ? (
        <div className="fish-loading">{t("fish.loading")}</div>
      ) : (
        <>
          {builtinFish.length > 0 && (
            <section className="fish-section">
              <h3 className="fish-section-title">{t("fish.sectionBuiltin")}</h3>
              <p className="fish-section-desc">{t("fish.sectionBuiltinDesc")}</p>
              {renderFishGrid(builtinFish)}
            </section>
          )}

          {skillFish.length > 0 && (
            <section className="fish-section">
              <h3 className="fish-section-title">{t("fish.sectionSkill")}</h3>
              <p className="fish-section-desc">{t("fish.sectionSkillDesc")}</p>
              {renderFishGrid(skillFish)}
            </section>
          )}

          {userFish.length > 0 && (
            <section className="fish-section">
              <h3 className="fish-section-title">{t("fish.sectionUser")}</h3>
              <p className="fish-section-desc">
                {t("fish.sectionUserDesc")} <code>{fishDir || "..."}</code>
              </p>
              {renderFishGrid(userFish)}
            </section>
          )}

          {fishList.length === 0 && (
            <div className="fish-empty">
              <span className="fish-empty-icon">🐠</span>
              <p>{t("fish.empty")}</p>
            </div>
          )}

          <section className="fish-section fish-guide-section">
            <h3 className="fish-section-title">{t("fish.sectionGuide")}</h3>
            <p className="fish-section-desc">在 <code>{fishDir ? `${fishDir}/my-fish/FISH.toml` : ".../fish/my-fish/FISH.toml"}</code> 创建文件：</p>
            <pre className="fish-code-example">{`id = "my-fish"
name = "我的小鱼"
description = "专注于某类任务的助手"
icon = "🐡"
tools = ["file_read", "shell", "memory_store"]

[agent]
system_prompt = "你是一条专注于..."
max_iterations = 20
model = "default"`}</pre>
          </section>
        </>
      )}
      </div>
    </div>
  );
}
