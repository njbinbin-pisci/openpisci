import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { FishDefinition, FishSource } from "../../services/tauri";
import "./Fish.css";

function sourceBadge(source: FishSource) {
  switch (source) {
    case "skill":
      return <span className="fish-card-badge badge-skill">技能</span>;
    case "user":
      return <span className="fish-card-badge badge-user">自定义</span>;
    case "builtin":
    default:
      return <span className="fish-card-badge badge-builtin">内置</span>;
  }
}

function FishCard({ fish }: { fish: FishDefinition }) {
  return (
    <div className="fish-card">
      <div className="fish-card-header">
        <span className="fish-card-icon">{fish.icon}</span>
        <div className="fish-card-meta">
          <span className="fish-card-name">{fish.name}</span>
          {sourceBadge(fish.source ?? (fish.builtin ? "builtin" : "user"))}
        </div>
      </div>
      <p className="fish-card-desc">{fish.description}</p>
      <div className="fish-card-tools">
        {fish.tools.slice(0, 4).map((t) => (
          <span key={t} className="fish-tool-tag">{t}</span>
        ))}
        {fish.tools.length > 4 && (
          <span className="fish-tool-tag">+{fish.tools.length - 4}</span>
        )}
      </div>
    </div>
  );
}

export default function FishPage() {
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
    <div className="fish-page">
      <div className="fish-page-header">
        <h2 className="fish-page-title">🐠 小鱼（Fish）</h2>
        <p className="fish-page-subtitle">
          小鱼是主 Agent 的内部专家工具。主 Agent 通过 call_fish 自动调用，无需手动激活。
        </p>
        <button className="fish-btn fish-btn-secondary fish-refresh-btn" onClick={loadFish}>
          刷新
        </button>
      </div>

      {error && (
        <div className="fish-error">
          <span>⚠️ {error}</span>
          <button onClick={() => setError(null)}>✕</button>
        </div>
      )}

      {loading ? (
        <div className="fish-loading">加载小鱼中...</div>
      ) : (
        <>
          {builtinFish.length > 0 && (
            <section className="fish-section">
              <h3 className="fish-section-title">内置小鱼</h3>
              <p className="fish-section-desc">OpenPisci 内置的专属 Agent，主 Agent 可自动调用</p>
              {renderFishGrid(builtinFish)}
            </section>
          )}

          {skillFish.length > 0 && (
            <section className="fish-section">
              <h3 className="fish-section-title">技能小鱼</h3>
              <p className="fish-section-desc">
                从已安装技能自动生成，每条小鱼专注于对应技能领域
              </p>
              {renderFishGrid(skillFish)}
            </section>
          )}

          {userFish.length > 0 && (
            <section className="fish-section">
              <h3 className="fish-section-title">自定义小鱼</h3>
              <p className="fish-section-desc">
                放置 FISH.toml 文件到 <code>{fishDir || "..."}</code> 目录即可加载
              </p>
              {renderFishGrid(userFish)}
            </section>
          )}

          {fishList.length === 0 && (
            <div className="fish-empty">
              <span className="fish-empty-icon">🐠</span>
              <p>暂无小鱼</p>
            </div>
          )}

          <section className="fish-section fish-guide-section">
            <h3 className="fish-section-title">创建自定义小鱼</h3>
            <p className="fish-section-desc">在 <code>{fishDir ? `${fishDir}\\my-fish\\FISH.toml` : ".../fish/my-fish/FISH.toml"}</code> 创建文件：</p>
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
  );
}
