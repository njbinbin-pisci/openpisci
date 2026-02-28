import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-shell";
import "./About.css";

const GITHUB_URL = "https://github.com/njbinbin-pisci/openpisci";

const REFERENCES = [
  { name: "OpenClaw", url: "https://github.com/mariozechner/openclaw", desc: "跨平台 AI Agent，pi-agent 架构参考" },
  { name: "OpenFang", url: "https://github.com/RightNow-AI/openfang", desc: "Rust + Tauri Agent OS，Loop Guard 与 Hand 系统参考" },
  { name: "LobsterAI", url: "https://github.com/lobsterai/lobsterai", desc: "Claude Agent SDK 集成参考" },
];

export default function About() {
  const [version, setVersion] = useState<string>("0.1.0");

  useEffect(() => {
    invoke<string>("plugin:app|version").then(setVersion).catch(() => {});
  }, []);

  const openLink = async (url: string) => {
    try {
      await open(url);
    } catch {
      window.open(url, "_blank");
    }
  };

  return (
    <div className="about-page">
      <div className="about-hero">
        <span className="about-logo">🐟</span>
        <h1 className="about-title">OpenPisci</h1>
        <p className="about-tagline">开源 Windows AI 桌面 Agent</p>
        <span className="about-version">v{version}</span>
      </div>

      <div className="about-desc">
        <p>
          OpenPisci 是一款运行在 Windows 桌面的开源 AI Agent，基于 Tauri + Rust 构建。
          大鱼（Pisci）是主 Agent，小鱼（Fish）是用户自定义的专属子 Agent。
        </p>
        <p>
          支持 Windows UI 自动化、浏览器控制、文件操作、定时任务、IM 网关、长期记忆等功能。
        </p>
      </div>

      <div className="about-links">
        <button
          className="about-link-btn about-link-github"
          onClick={() => openLink(GITHUB_URL)}
        >
          <span className="about-link-icon">⭐</span>
          <span>GitHub — njbinbin-pisci/openpisci</span>
          <span className="about-link-arrow">↗</span>
        </button>
      </div>

      <div className="about-section">
        <h3 className="about-section-title">许可证</h3>
        <p className="about-section-content">
          MIT License — 自由使用、修改和分发
        </p>
      </div>

      <div className="about-section">
        <h3 className="about-section-title">技术栈</h3>
        <div className="about-tech-grid">
          {[
            { name: "Tauri 2", desc: "跨平台桌面框架" },
            { name: "Rust", desc: "后端核心逻辑" },
            { name: "React + TypeScript", desc: "前端界面" },
            { name: "SQLite", desc: "本地数据存储" },
            { name: "Anthropic Claude", desc: "主要 LLM 提供商" },
            { name: "Windows UIA", desc: "桌面自动化" },
          ].map((tech) => (
            <div key={tech.name} className="about-tech-item">
              <span className="about-tech-name">{tech.name}</span>
              <span className="about-tech-desc">{tech.desc}</span>
            </div>
          ))}
        </div>
      </div>

      <div className="about-section">
        <h3 className="about-section-title">致谢 & 参考项目</h3>
        <div className="about-refs">
          {REFERENCES.map((ref) => (
            <button
              key={ref.name}
              className="about-ref-item"
              onClick={() => openLink(ref.url)}
            >
              <span className="about-ref-name">{ref.name}</span>
              <span className="about-ref-desc">{ref.desc}</span>
              <span className="about-ref-arrow">↗</span>
            </button>
          ))}
        </div>
      </div>

      <div className="about-footer">
        <p>Built with ❤️ by the OpenPisci community</p>
      </div>
    </div>
  );
}
