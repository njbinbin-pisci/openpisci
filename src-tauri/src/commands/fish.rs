/// Fish (小鱼) commands — manage user-defined sub-Agents.
use crate::fish::{FishDefinition, FishInstance, FishRegistry};
use crate::store::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tauri::{AppHandle, Manager, State};

#[derive(Debug, Serialize, Deserialize)]
pub struct FishWithStatus {
    #[serde(flatten)]
    pub definition: FishDefinition,
    /// None if not activated
    pub instance: Option<FishInstance>,
}

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

/// List all available Fish (built-in + user-installed), with their activation status.
#[tauri::command]
pub async fn list_fish(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<FishWithStatus>, String> {
    let user_fish_dir = app
        .path()
        .app_data_dir()
        .map(|d| d.join("fish"))
        .ok();

    let registry = FishRegistry::load(user_fish_dir.as_deref());

    let db = state.db.lock().await;
    let instances: HashMap<String, FishInstance> = db
        .list_fish_instances()
        .unwrap_or_default()
        .into_iter()
        .map(|i| (i.fish_id.clone(), i))
        .collect();

    let result = registry
        .list()
        .iter()
        .map(|def| FishWithStatus {
            definition: def.clone(),
            instance: instances.get(&def.id).cloned(),
        })
        .collect();

    Ok(result)
}

/// Activate a Fish: create a dedicated session and persist the instance.
#[tauri::command]
pub async fn activate_fish(
    app: AppHandle,
    state: State<'_, AppState>,
    fish_id: String,
    user_config: HashMap<String, String>,
) -> Result<String, String> {
    let user_fish_dir = app
        .path()
        .app_data_dir()
        .map(|d| d.join("fish"))
        .ok();

    let registry = FishRegistry::load(user_fish_dir.as_deref());
    let def = registry
        .get(&fish_id)
        .ok_or_else(|| format!("Fish '{}' not found", fish_id))?;

    let session_title = def.name.clone();
    let session_id = format!("fish_{}", fish_id);

    let db = state.db.lock().await;

    // Create or reuse the dedicated session
    let _ = db.ensure_im_session(&session_id, &session_title, &format!("fish_{}", fish_id));

    // Persist the fish instance
    let config_json = serde_json::to_string(&user_config).unwrap_or_else(|_| "{}".to_string());
    db.upsert_fish_instance(&fish_id, &session_id, "active", &config_json)
        .map_err(|e| e.to_string())?;

    tracing::info!("Fish '{}' activated with session '{}'", fish_id, session_id);
    Ok(session_id)
}

/// Deactivate a Fish (removes the instance record, session is preserved for history).
#[tauri::command]
pub async fn deactivate_fish(
    state: State<'_, AppState>,
    fish_id: String,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.delete_fish_instance(&fish_id).map_err(|e| e.to_string())?;
    tracing::info!("Fish '{}' deactivated", fish_id);
    Ok(())
}

/// Get the current status of a Fish instance.
#[tauri::command]
pub async fn get_fish_status(
    state: State<'_, AppState>,
    fish_id: String,
) -> Result<Option<FishInstance>, String> {
    let db = state.db.lock().await;
    db.get_fish_instance(&fish_id).map_err(|e| e.to_string())
}

/// Send a message to a Fish's dedicated session.
/// This is a thin wrapper that sets the correct system prompt for the Fish
/// before delegating to the standard chat_send flow.
#[tauri::command]
pub async fn fish_chat_send(
    app: AppHandle,
    state: State<'_, AppState>,
    fish_id: String,
    content: String,
) -> Result<(), String> {
    let user_fish_dir = app
        .path()
        .app_data_dir()
        .map(|d| d.join("fish"))
        .ok();

    let registry = FishRegistry::load(user_fish_dir.as_deref());
    let def = registry
        .get(&fish_id)
        .ok_or_else(|| format!("Fish '{}' not found", fish_id))?;

    let session_id = format!("fish_{}", fish_id);

    // Get user config for this fish — also enforces that the fish must be activated first
    let user_config = {
        let db = state.db.lock().await;
        match db.get_fish_instance(&fish_id) {
            Ok(Some(instance)) => instance.user_config,
            Ok(None) => {
                return Err(format!(
                    "Fish '{}' is not activated. Please activate it in the Fish settings page first.",
                    fish_id
                ));
            }
            Err(e) => return Err(e.to_string()),
        }
    };

    // Build fish-specific system prompt
    let fish_system_prompt = crate::fish::build_fish_system_prompt(def, &user_config);

    // Use the standard agent loop with fish-specific system prompt override
    crate::commands::chat::fish_chat_send_impl(
        app,
        state,
        session_id,
        content,
        Some(fish_system_prompt),
        def.agent.max_iterations,
        def.tools.clone(),
    ).await
}
