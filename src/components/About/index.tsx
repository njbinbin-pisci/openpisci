import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-shell";
import { useTranslation } from "react-i18next";
import "./About.css";

const GITHUB_URL = "https://github.com/njbinbin-pisci/openpisci";
const WEBSITE_URL = "http://www.dimnuo.com";

export default function About() {
  const { t } = useTranslation();
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

  const references = [
    { name: "OpenClaw", url: "https://github.com/mariozechner/openclaw", desc: t("about.refOpenClaw") },
    { name: "OpenFang", url: "https://github.com/RightNow-AI/openfang", desc: t("about.refOpenFang") },
    { name: "LobsterAI", url: "https://github.com/lobsterai/lobsterai", desc: t("about.refLobsterAI") },
  ];

  return (
    <div className="about-page">
      <div className="about-hero">
        <img src="/pisci.png" className="about-logo" alt="Pisci" />
        <h1 className="about-title">OpenPisci</h1>
        <p className="about-tagline">{t("about.tagline")}</p>
        <span className="about-version">v{version}</span>
      </div>

      <div className="about-desc">
        <p>{t("about.desc1")}</p>
        <p>{t("about.desc2")}</p>
        <button
          className="about-link-btn about-link-github about-link-inline"
          onClick={() => openLink(GITHUB_URL)}
        >
          <span className="about-link-icon">⭐</span>
          <span>GitHub — njbinbin-pisci/openpisci</span>
          <span className="about-link-arrow">↗</span>
        </button>
      </div>

      <div className="about-team-card">
        <p className="about-team-desc">{t("about.teamDesc")}</p>
        <button
          className="about-link-btn about-link-website about-link-inline"
          onClick={() => openLink(WEBSITE_URL)}
        >
          <span className="about-link-icon">🌐</span>
          <span>{t("about.website")} — www.dimnuo.com</span>
          <span className="about-link-arrow">↗</span>
        </button>
      </div>

      <div className="about-section about-disclaimer">
        <h3 className="about-section-title about-disclaimer-title">⚠️ {t("about.disclaimer")}</h3>
        <p className="about-section-content about-disclaimer-content">
          {t("about.disclaimerContent")}
        </p>
      </div>

      <div className="about-section">
        <h3 className="about-section-title">{t("about.licenseTitle")}</h3>
        <p className="about-section-content">
          {t("about.licenseContent")}
        </p>
      </div>

      <div className="about-section">
        <h3 className="about-section-title">{t("about.techTitle")}</h3>
        <div className="about-tech-grid">
          {[
            { name: "Tauri 2", desc: t("about.techTauri2") },
            { name: "Rust", desc: t("about.techRust") },
            { name: "React + TypeScript", desc: t("about.techReactTs") },
            { name: "SQLite", desc: t("about.techSQLite") },
            { name: "Anthropic Claude", desc: t("about.techClaude") },
            { name: "Windows UIA", desc: t("about.techWindowsUIA") },
          ].map((tech) => (
            <div key={tech.name} className="about-tech-item">
              <span className="about-tech-name">{tech.name}</span>
              <span className="about-tech-desc">{tech.desc}</span>
            </div>
          ))}
        </div>
      </div>

      <div className="about-section">
        <h3 className="about-section-title">{t("about.referencesTitle")}</h3>
        <div className="about-refs">
          {references.map((ref) => (
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
