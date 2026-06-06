import { useTranslation } from "react-i18next";
import Collab from "./Collab";
import "./Pond.css";

export default function Pond() {
  const { t } = useTranslation();

  return (
    <div className="pond">
      <div className="feature-topbar">
        <h1 className="feature-topbar-title">
          <span aria-hidden>🏊</span>
          {t("pond.title")}
        </h1>
      </div>
      <div className="feature-topbar-body">
        <Collab />
      </div>
    </div>
  );
}
