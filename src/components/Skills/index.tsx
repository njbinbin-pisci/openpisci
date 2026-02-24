import { useEffect } from "react";
import { useDispatch, useSelector } from "react-redux";
import { RootState, skillsActions } from "../../store";
import { skillsApi } from "../../services/tauri";

export default function Skills() {
  const dispatch = useDispatch();
  const { skills } = useSelector((s: RootState) => s.skills);

  useEffect(() => {
    skillsApi.list().then(({ skills }) => {
      dispatch(skillsActions.setSkills(skills));
    });
  }, [dispatch]);

  const handleToggle = async (id: string, enabled: boolean) => {
    await skillsApi.toggle(id, enabled);
    dispatch(skillsActions.toggleSkill({ id, enabled }));
  };

  const enabledCount = skills.filter((s) => s.enabled).length;

  return (
    <div className="page">
      <div className="page-header">
        <h1 className="page-title">⚡ Skills</h1>
        <span className="badge badge-info">{enabledCount}/{skills.length} enabled</span>
      </div>

      <div className="page-body">
        <p style={{ color: "var(--text-secondary)", marginBottom: 20, fontSize: 13 }}>
          Enable or disable tools that Pisci can use. Disabled tools won't be available to the AI.
        </p>

        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))", gap: 12 }}>
          {skills.map((skill) => (
            <div key={skill.id} className="card skill-card" style={{ opacity: skill.enabled ? 1 : 0.6 }}>
              <div style={{ display: "flex", alignItems: "flex-start", justifyContent: "space-between", gap: 12 }}>
                <div style={{ flex: 1 }}>
                  <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6 }}>
                    <span style={{ fontSize: 20 }}>{skill.icon}</span>
                    <span style={{ fontWeight: 600, color: "var(--text-primary)" }}>{skill.name}</span>
                  </div>
                  <p style={{ fontSize: 13, color: "var(--text-secondary)" }}>{skill.description}</p>
                </div>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={skill.enabled}
                    onChange={(e) => handleToggle(skill.id, e.target.checked)}
                  />
                  <span className="toggle-slider" />
                </label>
              </div>
            </div>
          ))}
        </div>
      </div>

      <style>{`
        .toggle {
          position: relative;
          display: inline-block;
          width: 40px;
          height: 22px;
          flex-shrink: 0;
        }
        .toggle input { opacity: 0; width: 0; height: 0; }
        .toggle-slider {
          position: absolute;
          cursor: pointer;
          inset: 0;
          background: var(--bg-tertiary);
          border: 1px solid var(--border);
          border-radius: 100px;
          transition: 0.2s;
        }
        .toggle-slider::before {
          content: "";
          position: absolute;
          width: 16px;
          height: 16px;
          left: 2px;
          top: 2px;
          background: var(--text-muted);
          border-radius: 50%;
          transition: 0.2s;
        }
        .toggle input:checked + .toggle-slider {
          background: var(--accent-dim);
          border-color: var(--accent);
        }
        .toggle input:checked + .toggle-slider::before {
          transform: translateX(18px);
          background: var(--accent);
        }
      `}</style>
    </div>
  );
}
