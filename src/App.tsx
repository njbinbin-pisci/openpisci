import { useEffect, useState } from "react";
import { Provider, useDispatch, useSelector } from "react-redux";
import { useTranslation } from "react-i18next";
import { store, RootState, settingsActions, sessionsActions } from "./store";
import { settingsApi, sessionsApi, windowApi } from "./services/tauri";
import { setLanguage } from "./i18n";
import Chat from "./components/Chat";
import Memory from "./components/Memory";
import Tools from "./components/Tools";
import FishPage from "./components/Fish";
import Skills from "./components/Skills";
import Scheduler from "./components/Scheduler";
import Settings from "./components/Settings";
import AuditLog from "./components/AuditLog";
import About from "./components/About";
import Onboarding from "./components/Onboarding";
import OverlayApp from "./components/Overlay";
import "./theme.css";
import "./App.css";

type Tab = "chat" | "memory" | "tools" | "fish" | "skills" | "scheduler" | "audit" | "settings" | "about";

// Detect if we are running in the overlay window
const IS_OVERLAY = new URLSearchParams(window.location.search).get("overlay") === "1";

function AppContent() {
  const dispatch = useDispatch();
  const { t } = useTranslation();
  const { isConfigured, showOnboarding, settings } = useSelector((s: RootState) => s.settings);
  const [activeTab, setActiveTab] = useState<Tab>("chat");
  const [initialized, setInitialized] = useState(false);
  const [theme, setTheme] = useState<'violet' | 'gold'>(() => {
    return (localStorage.getItem('pisci-theme') as 'violet' | 'gold') || 'violet';
  });

  useEffect(() => {
    document.documentElement.setAttribute('data-theme', theme);
    localStorage.setItem('pisci-theme', theme);
    // Sync window border/title bar color with theme (Windows 11+)
    if (!IS_OVERLAY) {
      const apply = () => windowApi.setThemeBorder(theme).catch(() => {});
      apply();
      const tid = setTimeout(apply, 800); // Retry after window ready
      return () => clearTimeout(tid);
    }
  }, [theme]);

  // 当 settings.language 变化时同步 i18n
  useEffect(() => {
    if (settings?.language) {
      setLanguage(settings.language as "zh" | "en");
    }
  }, [settings?.language]);

  useEffect(() => {
    async function init() {
      try {
        const [settings, configured] = await Promise.all([
          settingsApi.get(),
          settingsApi.isConfigured(),
        ]);
        dispatch(settingsActions.setSettings(settings));
        dispatch(settingsActions.setConfigured(configured));
        if (!configured) {
          dispatch(settingsActions.setShowOnboarding(true));
        }

        // Load sessions
        const { sessions } = await sessionsApi.list();
        dispatch(sessionsActions.setSessions(sessions));
        if (sessions.length > 0) {
          dispatch(sessionsActions.setActiveSession(sessions[0].id));
        }
      } catch (e) {
        console.error("Init error:", e);
      } finally {
        setInitialized(true);
      }
    }
    init();
  }, [dispatch]);

  if (!initialized) {
    return (
      <div className="loading-screen">
        <div className="loading-spinner" />
        <p>Loading Pisci...</p>
      </div>
    );
  }

  if (showOnboarding) {
    return <Onboarding onComplete={() => dispatch(settingsActions.setShowOnboarding(false))} />;
  }

  const tabs: { id: Tab; label: string; icon: string }[] = [
    { id: "chat", label: t("nav.chat"), icon: "💬" },
    { id: "memory", label: t("nav.memory"), icon: "💡" },
    { id: "tools", label: t("nav.tools"), icon: "🔧" },
    { id: "fish", label: t("nav.fish"), icon: "🐠" },
    { id: "skills", label: t("nav.skills"), icon: "⚡" },
    { id: "scheduler", label: t("nav.scheduler"), icon: "⏰" },
    { id: "audit", label: t("nav.audit"), icon: "🔍" },
    { id: "settings", label: t("nav.settings"), icon: "⚙️" },
    { id: "about", label: t("nav.about"), icon: "ℹ️" },
  ];

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="sidebar-header">
          <span className="logo">🐟</span>
          <span className="app-name">OpenPisci</span>
        </div>
        <nav className="sidebar-nav">
          {tabs.map((tab) => (
            <button
              key={tab.id}
              className={`nav-item ${activeTab === tab.id ? "active" : ""}`}
              onClick={() => setActiveTab(tab.id)}
              title={tab.label}
            >
              <span className="nav-icon">{tab.icon}</span>
              <span className="nav-label">{tab.label}</span>
            </button>
          ))}
        </nav>
        <div className="sidebar-footer">
          <button
            className="nav-item minimal-mode-btn"
            title="极简模式（悬浮球）"
            onClick={() => windowApi.enterMinimalMode()}
          >
            <span className="nav-icon">⚪</span>
            <span className="nav-label">极简模式</span>
          </button>
          <button
            className={`nav-item theme-toggle`}
            title={theme === "violet" ? "切换金色主题" : "切换紫色主题"}
            onClick={() => setTheme(theme === "violet" ? "gold" : "violet")}
          >
            <span className="nav-icon">{theme === "violet" ? "🌙" : "☀️"}</span>
            <span className="nav-label">{theme === "violet" ? "金色" : "紫色"}</span>
          </button>
        </div>
      </aside>
      <main className="main-content">
        {activeTab === "chat" && <Chat />}
        {activeTab === "memory" && <Memory />}
        {activeTab === "tools" && <Tools />}
        {activeTab === "fish" && (
          <FishPage
            onGoToChat={(sessionId) => {
              // Switch to chat tab and select the fish session
              dispatch(sessionsActions.setActiveSession(sessionId));
              setActiveTab("chat");
            }}
          />
        )}
        {activeTab === "skills" && <Skills />}
        {activeTab === "scheduler" && <Scheduler />}
        {activeTab === "audit" && <AuditLog />}
        {activeTab === "settings" && <Settings theme={theme} setTheme={setTheme} />}
        {activeTab === "about" && <About />}
      </main>
    </div>
  );
}

export default function App() {
  if (IS_OVERLAY) {
    return <OverlayApp />;
  }
  return (
    <Provider store={store}>
      <AppContent />
    </Provider>
  );
}
