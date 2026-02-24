use crate::store::{db::Skill, AppState};
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct SkillList {
    pub skills: Vec<Skill>,
    pub total: usize,
}

#[tauri::command]
pub async fn list_skills(state: State<'_, AppState>) -> Result<SkillList, String> {
    let db = state.db.lock().await;
    let skills = db.list_skills().map_err(|e| e.to_string())?;
    let total = skills.len();
    Ok(SkillList { skills, total })
}

#[tauri::command]
pub async fn toggle_skill(
    state: State<'_, AppState>,
    skill_id: String,
    enabled: bool,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.set_skill_enabled(&skill_id, enabled)
        .map_err(|e| e.to_string())
}
