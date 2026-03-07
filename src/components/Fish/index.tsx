import { useState, useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { fishApi, FishWithStatus, FishSettingDef } from "../../services/tauri";
import "./Fish.css";

interface ConfigFormProps {
  fish: FishWithStatus;
  onSave: (config: Record<string, string>) => void;
  onCancel: () => void;
  saving: boolean;
}

function ConfigForm({ fish, onSave, onCancel, saving }: ConfigFormProps) {
  const [values, setValues] = useState<Record<string, string>>(() => {
    const init: Record<string, string> = {};
    fish.settings.forEach((s) => {
      init[s.key] = fish.instance?.user_config?.[s.key] ?? s.default ?? "";
    });
    return init;
  });

  const handleChange = (key: string, value: string) => {
    setValues((prev) => ({ ...prev, [key]: value }));
  };

  const renderField = (setting: FishSettingDef) => {
    const val = values[setting.key] ?? "";
    switch (setting.setting_type) {
      case "toggle":
        return (
          <label className="fish-toggle">
            <input
              type="checkbox"
              checked={val === "true"}
              onChange={(e) => handleChange(setting.key, e.target.checked ? "true" : "false")}
            />
            <span className="fish-toggle-slider" />
          </label>
        );
      case "select":
        return (
          <select
            className="fish-select"
            value={val}
            onChange={(e) => handleChange(setting.key, e.target.value)}
          >
            {setting.options.map((opt) => (
              <option key={opt.value} value={opt.value}>{opt.label}</option>
            ))}
          </select>
        );
      case "password":
        return (
          <input
            type="password"
            className="fish-input"
            value={val}
            placeholder={setting.placeholder}
            onChange={(e) => handleChange(setting.key, e.target.value)}
          />
        );
      default:
        return (
          <input
            type="text"
            className="fish-input"
            value={val}
            placeholder={setting.placeholder}
            onChange={(e) => handleChange(setting.key, e.target.value)}
          />
        );
    }
  };

  return (
    <div className="fish-config-form">
      <h4 className="fish-config-title">配置 {fish.name}</h4>
      {fish.settings.length === 0 ? (
        <p className="fish-config-empty">此小鱼无需额外配置</p>
      ) : (
        fish.settings.map((setting) => (
          <div key={setting.key} className="fish-config-field">
            <label className="fish-config-label">{setting.label}</label>
            {renderField(setting)}
          </div>
        ))
      )}
      <div className="fish-config-actions">
        <button className="fish-btn fish-btn-primary" onClick={() => onSave(values)} disabled={saving}>
          {saving ? "激活中..." : "激活小鱼"}
        </button>
        <button className="fish-btn fish-btn-secondary" onClick={onCancel}>取消</button>
      </div>
    </div>
  );
}

interface FishCardProps {
  fish: FishWithStatus;
  onActivate: (fish: FishWithStatus) => void;
  onDeactivate: (fishId: string) => void;
  onGoToChat: (sessionId: string) => void;
  deactivating: boolean;
}

function FishCard({ fish, onActivate, onDeactivate, onGoToChat, deactivating }: FishCardProps) {
  const { t } = useTranslation();
  const isActive = !!fish.instance && fish.instance.status === "active";

  return (
    <div className={`fish-card ${isActive ? "fish-card-active" : ""}`}>
      <div className="fish-card-header">
        <span className="fish-card-icon">{fish.icon}</span>
        <div className="fish-card-meta">
          <span className="fish-card-name">{fish.name}</span>
          <span className={`fish-card-badge ${isActive ? "badge-active" : "badge-inactive"}`}>
            {isActive ? "游泳中" : "待命"}
          </span>
          {fish.builtin && <span className="fish-card-badge badge-builtin">{t("common.builtin")}</span>}
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
      <div className="fish-card-actions">
        {isActive ? (
          <>
            <button
              className="fish-btn fish-btn-primary"
              onClick={() => onGoToChat(fish.instance!.session_id)}
            >
              🐟 游向对话
            </button>
            <button
              className="fish-btn fish-btn-danger"
              onClick={() => onDeactivate(fish.id)}
              disabled={deactivating}
            >
              {deactivating ? "停用中..." : "停用"}
            </button>
          </>
        ) : (
          <button className="fish-btn fish-btn-primary" onClick={() => onActivate(fish)}>
            激活小鱼
          </button>
        )}
      </div>
    </div>
  );
}

interface FishPageProps {
  onGoToChat?: (sessionId: string) => void;
}

export default function FishPage({ onGoToChat }: FishPageProps) {
  const { t } = useTranslation();
  const [fishList, setFishList] = useState<FishWithStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [configuringFish, setConfiguringFish] = useState<FishWithStatus | null>(null);
  const [activating, setActivating] = useState(false);
  const [deactivatingId, setDeactivatingId] = useState<string | null>(null);
  const [fishDir, setFishDir] = useState<string>("");

  const loadFish = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const list = await fishApi.list();
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

  const handleActivate = (fish: FishWithStatus) => {
    setConfiguringFish(fish);
  };

  const handleSaveConfig = async (config: Record<string, string>) => {
    if (!configuringFish) return;
    setActivating(true);
    try {
      const sessionId = await fishApi.activate(configuringFish.id, config);
      setConfiguringFish(null);
      await loadFish();
      if (onGoToChat) onGoToChat(sessionId);
    } catch (e) {
      setError(String(e));
    } finally {
      setActivating(false);
    }
  };

  const handleDeactivate = async (fishId: string) => {
    setDeactivatingId(fishId);
    try {
      await fishApi.deactivate(fishId);
      await loadFish();
    } catch (e) {
      setError(String(e));
    } finally {
      setDeactivatingId(null);
    }
  };

  const handleGoToChat = (sessionId: string) => {
    if (onGoToChat) onGoToChat(sessionId);
  };

  const builtinFish = fishList.filter((f) => f.builtin);
  const userFish = fishList.filter((f) => !f.builtin);

  return (
    <div className="fish-page">
      <div className="fish-page-header">
        <h2 className="fish-page-title">🐠 小鱼（Fish）</h2>
        <p className="fish-page-subtitle">
          小鱼是专属子 Agent，拥有独立人设、工具权限和配置。激活后在独立会话中工作。
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

      {configuringFish && (
        <div className="fish-modal-overlay" onClick={() => setConfiguringFish(null)}>
          <div className="fish-modal" onClick={(e) => e.stopPropagation()}>
            <ConfigForm
              fish={configuringFish}
              onSave={handleSaveConfig}
              onCancel={() => setConfiguringFish(null)}
              saving={activating}
            />
          </div>
        </div>
      )}

      {loading ? (
        <div className="fish-loading">加载小鱼中...</div>
      ) : (
        <>
          {builtinFish.length > 0 && (
            <section className="fish-section">
              <h3 className="fish-section-title">内置小鱼</h3>
              <p className="fish-section-desc">OpenPisci 内置的专属 Agent，开箱即用</p>
              <div className="fish-grid">
                {builtinFish.map((fish) => (
                  <FishCard
                    key={fish.id}
                    fish={fish}
                    onActivate={handleActivate}
                    onDeactivate={handleDeactivate}
                    onGoToChat={handleGoToChat}
                    deactivating={deactivatingId === fish.id}
                  />
                ))}
              </div>
            </section>
          )}

          {userFish.length > 0 && (
            <section className="fish-section">
              <h3 className="fish-section-title">自定义小鱼</h3>
              <p className="fish-section-desc">
                放置 FISH.toml 文件到 <code>{fishDir || "..."}</code> 目录即可加载
              </p>
              <div className="fish-grid">
                {userFish.map((fish) => (
                  <FishCard
                    key={fish.id}
                    fish={fish}
                    onActivate={handleActivate}
                    onDeactivate={handleDeactivate}
                    onGoToChat={handleGoToChat}
                    deactivating={deactivatingId === fish.id}
                  />
                ))}
              </div>
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
model = "default"

[[settings]]
key = "workspace"
label = "工作目录"
setting_type = "text"
default = ""
placeholder = "例如：C:\\\\Users\\\\你的用户名\\\\Documents"`}</pre>
          </section>
        </>
      )}
    </div>
  );
}
