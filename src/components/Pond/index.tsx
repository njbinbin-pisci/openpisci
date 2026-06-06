import { useTranslation } from "react-i18next";
import Collab from "./Collab";
import "./Pond.css";

interface PondProps {
  onNavigateToSchoolKoi?: () => void;
}

export default function Pond({ onNavigateToSchoolKoi }: PondProps) {
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
        <Collab onNavigateToSchoolKoi={onNavigateToSchoolKoi} />
      </div>
    </div>
  );
}
