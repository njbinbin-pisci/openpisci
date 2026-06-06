import { Suspense, lazy, useEffect, useState } from "react";
import { Provider, useDispatch, useSelector } from "react-redux";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { store, RootState, settingsActions, sessionsActions, chatActions } from "./store";
import { settingsApi, sessionsApi, windowApi } from "./services/tauri";
import { isInternalSession } from "./utils/session";
import { setLanguage } from "./i18n";
import Chat from "./components/Chat";
import Toaster from "./components/Toaster";
import "./theme.css";
import "./App.css";

const Memory = lazy(() => import("./components/Memory"));
const Tools = lazy(() => import("./components/Tools"));
const FishPage = lazy(() => import("./components/Fish"));
const Pond = lazy(() => import("./components/Pond"));
const Skills = lazy(() => import("./components/Skills"));
const Scheduler = lazy(() => import("./components/Scheduler"));
const Settings = lazy(() => import("./components/Settings"));
const AuditLog = lazy(() => import("./components/AuditLog"));
const About = lazy(() => import("./components/About"));
const Onboarding = lazy(() => import("./components/Onboarding"));
const OverlayApp = lazy(() => import("./components/Overlay"));
const DebugPanel = lazy(() => import("./components/Debug"));

type Tab = "chat" | "memory" | "tools" | "fish" | "pond" | "skills" | "scheduler" | "audit" | "settings" | "about" | "debug";

// Detect if we are running in the overlay window
const IS_OVERLAY = new URLSearchParams(window.location.search).get("overlay") === "1";

function AppContent() {
  const dispatch = useDispatch();
  const { t } = useTranslation();
  const { showOnboarding, settings } = useSelector((s: RootState) => s.settings);
  const pendingMainChatNav = useSelector((s: RootState) => s.sessions.pendingMainChatNav);
  const [activeTab, setActiveTab] = useState<Tab>("chat");
  /** Tabs that have been opened at least once — stay mounted to preserve state. */
  const [mountedTabs, setMountedTabs] = useState<Set<Tab>>(() => new Set(["chat"]));
  const [initialized, setInitialized] = useState(false);
  const [theme, setTheme] = useState<'violet' | 'gold'>(() => {
    return (localStorage.getItem('piscis-theme') as 'violet' | 'gold') || 'violet';
  });

  useEffect(() => {
    setMountedTabs((prev) => {
      if (prev.has(activeTab)) return prev;
      const next = new Set(prev);
      next.add(activeTab);
      return next;
    });
  }, [activeTab]);

  // Pond IDE assistant → jump to main Chat (鱼池CLI tab).
  useEffect(() => {
    if (pendingMainChatNav) setActiveTab("chat");
  }, [pendingMainChatNav]);

  useEffect(() => {
    document.documentElement.setAttribute('data-theme', theme);
    localStorage.setItem('piscis-theme', theme);
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

        // Load sessions — skip internal sessions (heartbeat, piscis_inbox, etc.)
        // when choosing the initial active session so the user always lands on
        // a real chat session, not an invisible internal one.
        const { sessions } = await sessionsApi.list(100);
        dispatch(sessionsActions.setSessions(sessions));
        const firstVisible = sessions.find((s) => !isInternalSession(s));
        dispatch(sessionsActions.setActiveSession(firstVisible?.id ?? null));
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
        // Clear any stale streaming state / tool steps / frozen bubble from a previous run
        // so the UI doesn't show overlapping output from the old agent.
        // frozenBubble MUST be cleared here because the IM agent event listener
        // (Chat/index.tsx) is only subscribed when this session is the active one.
        // If the user is viewing a different session, freezeStreaming never fires
        // for this IM session, so the stale frozenBubble from the previous turn
        // would be reused by im_session_done's setMessagesWithFrozen, causing
        // a stale collapsed bubble to appear in the middle of the message list.
        dispatch(chatActions.clearFrozenBubble(sid));
        dispatch(chatActions.clearStreaming(sid));
        dispatch(chatActions.clearToolSteps(sid));
        dispatch(chatActions.clearContextUsage(sid));
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
          <p>Loading OpenPiscis...</p>
        </div>
        <Toaster />
      </>
    );
  }

  if (showOnboarding) {
    return (
      <>
        <Suspense fallback={<div className="loading-screen"><div className="loading-spinner" /><p>Loading OpenPiscis...</p></div>}>
          <Onboarding onComplete={() => dispatch(settingsActions.setShowOnboarding(false))} />
        </Suspense>
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
          <img src="/piscis.png" className="logo" alt="OpenPiscis" />
          <span className="app-name">OpenPiscis</span>
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
        <Suspense fallback={<div className="loading-screen"><div className="loading-spinner" /><p>Loading OpenPiscis...</p></div>}>
          {mountedTabs.has("chat") && (
            <div className="tab-panel" hidden={activeTab !== "chat"}>
              <Chat />
            </div>
          )}
          {mountedTabs.has("memory") && (
            <div className="tab-panel" hidden={activeTab !== "memory"}><Memory /></div>
          )}
          {mountedTabs.has("tools") && (
            <div className="tab-panel" hidden={activeTab !== "tools"}><Tools /></div>
          )}
          {mountedTabs.has("pond") && (
            <div className="tab-panel" hidden={activeTab !== "pond"}><Pond /></div>
          )}
          {mountedTabs.has("fish") && (
            <div className="tab-panel" hidden={activeTab !== "fish"}><FishPage /></div>
          )}
          {mountedTabs.has("skills") && (
            <div className="tab-panel" hidden={activeTab !== "skills"}><Skills /></div>
          )}
          {mountedTabs.has("scheduler") && (
            <div className="tab-panel" hidden={activeTab !== "scheduler"}><Scheduler /></div>
          )}
          {mountedTabs.has("audit") && (
            <div className="tab-panel" hidden={activeTab !== "audit"}><AuditLog /></div>
          )}
          {mountedTabs.has("settings") && (
            <div className="tab-panel" hidden={activeTab !== "settings"}>
              <Settings theme={theme} setTheme={setTheme} onOpenTools={() => setActiveTab("tools")} />
            </div>
          )}
          {mountedTabs.has("about") && (
            <div className="tab-panel" hidden={activeTab !== "about"}><About /></div>
          )}
          {mountedTabs.has("debug") && (
            <div className="tab-panel" hidden={activeTab !== "debug"}><DebugPanel /></div>
          )}
        </Suspense>
      </main>
      <Toaster />
    </div>
  );
}

export default function App() {
  if (IS_OVERLAY) {
    return (
      <Suspense fallback={<div className="loading-screen"><div className="loading-spinner" /><p>Loading OpenPiscis...</p></div>}>
        <OverlayApp />
      </Suspense>
    );
  }
  return (
    <Provider store={store}>
      <AppContent />
    </Provider>
  );
}
