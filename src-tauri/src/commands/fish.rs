/// Fish (小鱼) commands — read-only listing of available sub-Agents.
use crate::fish::{FishDefinition, FishRegistry};
use tauri::{AppHandle, Manager};

/// Return the user fish directory path (where custom FISH.toml files should be placed).
#[tauri::command]
pub async fn get_fish_dir(app: AppHandle) -> Result<String, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map(|d| d.join("fish"))
        .map_err(|e| e.to_string())?;
    Ok(dir.to_string_lossy().into_owned())
}

/// List all available Fish (built-in + skill-generated + user-installed).
#[tauri::command]
pub async fn list_fish(
    app: AppHandle,
) -> Result<Vec<FishDefinition>, String> {
    let app_data_dir = app.path().app_data_dir().ok();
    let registry = FishRegistry::load(app_data_dir.as_deref());
    Ok(registry.list().to_vec())
}
