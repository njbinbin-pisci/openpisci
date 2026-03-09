import { useState } from "react";
import { useTranslation } from "react-i18next";
import KoiManager from "./KoiManager";
import ChatPool from "./ChatPool";
import Board from "./Board";
import "./Pond.css";

type PondSubTab = "kois" | "pool" | "board";

export default function Pond() {
  const { t } = useTranslation();
  const [subTab, setSubTab] = useState<PondSubTab>("kois");

  const tabs: { id: PondSubTab; label: string; icon: string }[] = [
    { id: "kois", label: t("pond.tabKois"), icon: "🐡" },
    { id: "pool", label: t("pond.tabPool"), icon: "💬" },
    { id: "board", label: t("pond.tabBoard"), icon: "📋" },
  ];

  return (
    <div className="pond">
      <div className="pond-header">
        <h2 className="pond-title">🏊 {t("pond.title")}</h2>
        <div className="pond-tabs">
          {tabs.map((tab) => (
            <button
              key={tab.id}
              className={`pond-tab ${subTab === tab.id ? "active" : ""}`}
              onClick={() => setSubTab(tab.id)}
            >
              <span className="pond-tab-icon">{tab.icon}</span>
              <span>{tab.label}</span>
            </button>
          ))}
        </div>
      </div>
      <div className="pond-content">
        {subTab === "kois" && <KoiManager />}
        {subTab === "pool" && <ChatPool />}
        {subTab === "board" && <Board />}
      </div>
    </div>
  );
}
