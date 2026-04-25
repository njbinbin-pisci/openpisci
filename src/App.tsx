import { useEffect, useState } from "react";
import { Provider, useDispatch, useSelector } from "react-redux";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { store, RootState, settingsActions, sessionsActions, chatActions } from "./store";
import { settingsApi, sessionsApi, windowApi } from "./services/tauri";
import { isInternalSession } from "./utils/session";
import { setLanguage } from "./i18n";
import Chat from "./components/Chat";
import Memory from "./components/Memory";
import Tools from "./components/Tools";
import FishPage from "./components/Fish";
import Pond from "./components/Pond";
import Skills from "./components/Skills";
import Scheduler from "./components/Scheduler";
import Settings from "./components/Settings";
import AuditLog from "./components/AuditLog";
import About from "./components/About";
import Onboarding from "./components/Onboarding";
import OverlayApp from "./components/Overlay";
import DebugPanel from "./components/Debug";
import Toaster from "./components/Toaster";
import "./theme.css";
import "./App.css";

type Tab = "chat" | "memory" | "tools" | "fish" | "pond" | "skills" | "scheduler" | "audit" | "settings" | "about" | "debug";

// Detect if we are running in the overlay window
const IS_OVERLAY = new URLSearchParams(window.location.search).get("overlay") === "1";

function AppContent() {
  const dispatch = useDispatch();
  const { t } = useTranslation();
  const { showOnboarding, settings } = useSelector((s: RootState) => s.settings);
  const [activeTab, setActiveTab] = useState<Tab>("chat");
  const [initialized, setInitialized] = useState(false);
  const [theme, setTheme] = useState<'violet' | 'gold'>(() => {
    return (localStorage.getItem('pisci-theme') as 'violet' | 'gold') || 'violet';
  });

  useEffect(() => {
    document.documentElement.setAttribute('data-theme', theme);
    localStorage.setItem('pisci-theme', theme);
    console.log('[Theme] data-theme set to:', theme, '| html attr:', document.documentElement.getAttribute('data-theme'));
    // Sync window border/title bar color with theme (Windows 11+)
    if (!IS_OVERLAY) {
      const apply = () => windowApi.setThemeBorder(theme).catch(() => {});
      apply();
      const tid = setTimeout(apply, 800); // Retry after window ready
      return () => clearTimeout(tid);
    }
  }, [theme]);

  useEffect(() => {
    const unlisten = listen<string>("app_theme_changed", (event) => {
      const next = event.payload === "gold" ? "gold" : "violet";
      setTheme(next);
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

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

        // Load sessions — skip internal sessions (heartbeat, pisci_inbox, etc.)
        // when choosing the initial active session so the user always lands on
        // a real chat session, not an invisible internal one.
        const { sessions } = await sessionsApi.list(100);
        dispatch(sessionsActions.setSessions(sessions));
        const firstVisible = sessions.find((s) => !isInternalSession(s));
        if (firstVisible) {
          dispatch(sessionsActions.setActiveSession(firstVisible.id));
        } else if (sessions.length > 0) {
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

  // im_session_updated: inbound user message arrived and was pre-written to DB.
  // Reload messages immediately so the user sees their own message right away.
  // Mark session as running to show the processing indicator.
  // Also refresh the session list so IM sessions (which are created on demand)
  // appear in the sidebar and can be selected.
  useEffect(() => {
    const unlisten = listen<string>("im_session_updated", async (event) => {
      const sid = event.payload;
      if (!sid) return;
      console.log('[IM] im_session_updated sid=', sid);
      try {
        const [messages, { sessions: fresh }] = await Promise.all([
          sessionsApi.getMessages(sid),
          sessionsApi.list(100),
        ]);
        console.log('[IM] im_session_updated: loaded', messages.length, 'messages');
        // Update session list so the IM session appears in the sidebar.
        // setSessions does NOT change activeSessionId, so the user's current
        // session selection is preserved.
        dispatch(sessionsActions.setSessions(fresh));
        dispatch(chatActions.setMessages({ sessionId: sid, messages }));
        dispatch(chatActions.setRunning({ sessionId: sid, running: true }));
      } catch (e) {
        console.error("[IM] im_session_updated error:", e);
      }
    });
    return () => { unlisten.then((fn) => fn()); };
  }, [dispatch]);

  // im_session_done: agent finished AND persisted all messages to DB.
  // This is emitted AFTER persist_agent_turn completes, so getMessages will see the full reply.
  useEffect(() => {
    const unlisten = listen<string>("im_session_done", async (event) => {
      const sid = event.payload;
      if (!sid) return;
      console.log('[IM] im_session_done sid=', sid);
      try {
        const messages = await sessionsApi.getMessages(sid, 200);
        console.log('[IM] im_session_done: loaded', messages.length, 'messages');
        // Use setMessagesWithFrozen so the frozenBubble (merged streaming text) is preserved
        // as a single bubble, rather than being replaced by the raw multi-row DB data.
        dispatch(chatActions.setMessagesWithFrozen({ sessionId: sid, messages }));
      } catch (e) {
        console.error("[IM] im_session_done error:", e);
      }
      dispatch(chatActions.setRunning({ sessionId: sid, running: false }));
      dispatch(chatActions.clearStreaming(sid));
    });
    return () => { unlisten.then((fn) => fn()); };
  }, [dispatch]);

  // settings_changed: emitted by app_control tool when Agent modifies settings
  // (SSH servers, API keys, tool toggles, etc.) — re-fetch and sync Redux store
  // so the Settings page reflects changes without requiring a manual restart.
  useEffect(() => {
    const unlisten = listen("settings_changed", async () => {
      try {
        const updated = await settingsApi.get();
        dispatch(settingsActions.setSettings(updated));
      } catch (e) {
        console.error("[settings_changed] failed to reload settings:", e);
      }
    });
    return () => { unlisten.then((fn) => fn()); };
  }, [dispatch]);

  if (!initialized) {
    return (
      <>
        <div className="loading-screen">
          <div className="loading-spinner" />
          <p>Loading Pisci...</p>
        </div>
        <Toaster />
      </>
    );
  }

  if (showOnboarding) {
    return (
      <>
        <Onboarding onComplete={() => dispatch(settingsActions.setShowOnboarding(false))} />
        <Toaster />
      </>
    );
  }

  const tabs: { id: Tab; label: string; icon: string }[] = [
    { id: "chat", label: t("nav.chat"), icon: "💬" },
    { id: "pond", label: t("nav.pond"), icon: "🏊" },
    { id: "tools", label: t("nav.tools"), icon: "🔧" },
    { id: "skills", label: t("nav.skills"), icon: "⚡" },
    { id: "fish", label: t("nav.fish"), icon: "🐠" },
    { id: "scheduler", label: t("nav.scheduler"), icon: "⏰" },
    { id: "memory", label: t("nav.memory"), icon: "💡" },
    { id: "audit", label: t("nav.audit"), icon: "🔍" },
    { id: "settings", label: t("nav.settings"), icon: "⚙️" },
    { id: "about", label: t("nav.about"), icon: "ℹ️" },
  ];

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="sidebar-header">
          <img src="/pisci.png" className="logo" alt="Pisci" />
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
            className={`nav-item ${activeTab === "debug" ? "active" : ""}`}
            title={t("nav.debug")}
            onClick={() => setActiveTab("debug")}
          >
            <span className="nav-icon">🔬</span>
            <span className="nav-label">{t("nav.debug")}</span>
          </button>
          <button
            className="nav-item minimal-mode-btn"
            title={t("nav.minimalMode")}
            onClick={() => windowApi.enterMinimalMode()}
          >
            <span className="nav-icon">⚪</span>
            <span className="nav-label">{t("nav.minimalMode")}</span>
          </button>
        </div>
      </aside>
      <main className="main-content">
        {activeTab === "chat" && <Chat />}
        {activeTab === "memory" && <Memory />}
        {activeTab === "tools" && <Tools />}
        {activeTab === "pond" && <Pond />}
        {activeTab === "fish" && <FishPage />}
        {activeTab === "skills" && <Skills />}
        {activeTab === "scheduler" && <Scheduler />}
        {activeTab === "audit" && <AuditLog />}
        {activeTab === "settings" && <Settings theme={theme} setTheme={setTheme} onOpenTools={() => setActiveTab("tools")} />}
        {activeTab === "about" && <About />}
        {activeTab === "debug" && <DebugPanel />}
      </main>
      <Toaster />
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
