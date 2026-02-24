import { useEffect, useState } from "react";
import { Provider, useDispatch, useSelector } from "react-redux";
import { store, RootState, settingsActions, sessionsActions } from "./store";
import { settingsApi, sessionsApi } from "./services/tauri";
import Chat from "./components/Chat";
import Memory from "./components/Memory";
import Skills from "./components/Skills";
import Scheduler from "./components/Scheduler";
import Settings from "./components/Settings";
import Onboarding from "./components/Onboarding";
import "./App.css";

type Tab = "chat" | "memory" | "skills" | "scheduler" | "settings";

function AppContent() {
  const dispatch = useDispatch();
  const { isConfigured, showOnboarding } = useSelector((s: RootState) => s.settings);
  const [activeTab, setActiveTab] = useState<Tab>("chat");
  const [initialized, setInitialized] = useState(false);

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
    { id: "chat", label: "Chat", icon: "💬" },
    { id: "memory", label: "Memory", icon: "🧠" },
    { id: "skills", label: "Skills", icon: "⚡" },
    { id: "scheduler", label: "Scheduler", icon: "⏰" },
    { id: "settings", label: "Settings", icon: "⚙️" },
  ];

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="sidebar-header">
          <span className="logo">🐟</span>
          <span className="app-name">Pisci</span>
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
      </aside>
      <main className="main-content">
        {activeTab === "chat" && <Chat />}
        {activeTab === "memory" && <Memory />}
        {activeTab === "skills" && <Skills />}
        {activeTab === "scheduler" && <Scheduler />}
        {activeTab === "settings" && <Settings />}
      </main>
    </div>
  );
}

export default function App() {
  return (
    <Provider store={store}>
      <AppContent />
    </Provider>
  );
}
