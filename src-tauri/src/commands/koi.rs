/// Koi (锦鲤) commands — CRUD for persistent independent Agents.
use crate::koi::{KoiDefinition, KOI_COLORS, KOI_ICONS};
use crate::store::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Serialize)]
pub struct KoiWithStats {
    #[serde(flatten)]
    pub koi: KoiDefinition,
    pub memory_count: i64,
    pub todo_count: i64,
    pub active_todo_count: i64,
}

#[tauri::command]
pub async fn list_kois(state: State<'_, AppState>) -> Result<Vec<KoiWithStats>, String> {
    let db = state.db.lock().await;
    let kois = db.list_kois().map_err(|e| e.to_string())?;
    let mut result = Vec::with_capacity(kois.len());
    for koi in kois {
        let memory_count = db.count_memories_for_owner(&koi.id).unwrap_or(0);
        let todos = db.list_koi_todos(Some(&koi.id)).unwrap_or_default();
        let todo_count = todos.len() as i64;
        let active_todo_count = todos.iter().filter(|t| t.status == "todo" || t.status == "in_progress").count() as i64;
        result.push(KoiWithStats { koi, memory_count, todo_count, active_todo_count });
    }
    Ok(result)
}

#[tauri::command]
pub async fn get_koi(state: State<'_, AppState>, id: String) -> Result<Option<KoiDefinition>, String> {
    let db = state.db.lock().await;
    db.get_koi(&id).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
pub struct CreateKoiInput {
    pub name: String,
    pub icon: String,
    pub color: String,
    pub system_prompt: String,
    pub description: String,
}

#[tauri::command]
pub async fn create_koi(state: State<'_, AppState>, input: CreateKoiInput) -> Result<KoiDefinition, String> {
    let db = state.db.lock().await;
    db.create_koi(&input.name, &input.icon, &input.color, &input.system_prompt, &input.description)
        .map_err(|e| e.to_string())
}

#[derive(Deserialize)]
pub struct UpdateKoiInput {
    pub id: String,
    pub name: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub system_prompt: Option<String>,
    pub description: Option<String>,
}

#[tauri::command]
pub async fn update_koi(state: State<'_, AppState>, input: UpdateKoiInput) -> Result<(), String> {
    let db = state.db.lock().await;
    db.update_koi(
        &input.id,
        input.name.as_deref(),
        input.icon.as_deref(),
        input.color.as_deref(),
        input.system_prompt.as_deref(),
        input.description.as_deref(),
    ).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_koi(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let db = state.db.lock().await;
    db.delete_koi(&id).map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct KoiPalette {
    pub colors: Vec<(String, String)>,
    pub icons: Vec<String>,
}

#[tauri::command]
pub async fn get_koi_palette() -> Result<KoiPalette, String> {
    Ok(KoiPalette {
        colors: KOI_COLORS.iter().map(|(c, n)| (c.to_string(), n.to_string())).collect(),
        icons: KOI_ICONS.iter().map(|s| s.to_string()).collect(),
    })
}
