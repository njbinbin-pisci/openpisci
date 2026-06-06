import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import FishPage from "../Fish";
import KoiManager from "../Pond/KoiManager";
import "../Tools/Tools.css";
import "./School.css";

export type SchoolSubTab = "fish" | "koi";

interface SchoolPageProps {
  initialSubTab?: SchoolSubTab;
}

export default function SchoolPage({ initialSubTab = "fish" }: SchoolPageProps) {
  const { t } = useTranslation();
  const [activeTab, setActiveTab] = useState<SchoolSubTab>(initialSubTab);

  useEffect(() => {
    setActiveTab(initialSubTab);
  }, [initialSubTab]);

  return (
    <div className="page school-page">
      <div className="page-header">
        <h1 className="page-title">🐟 {t("nav.school")}</h1>
      </div>

      <div className="page-body school-page-body">
        <div className="tools-tabs school-tabs">
          <button
            type="button"
            className={`tools-tab ${activeTab === "fish" ? "active" : ""}`}
            onClick={() => setActiveTab("fish")}
          >
            🐠 {t("school.tabFish")}
          </button>
          <button
            type="button"
            className={`tools-tab ${activeTab === "koi" ? "active" : ""}`}
            onClick={() => setActiveTab("koi")}
          >
            🎏 {t("school.tabKoi")}
          </button>
        </div>

        <div className="school-tab-panel" hidden={activeTab !== "fish"}>
          <FishPage embedded />
        </div>
        <div className="school-tab-panel" hidden={activeTab !== "koi"}>
          <KoiManager />
        </div>
      </div>
    </div>
  );
}
