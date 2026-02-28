use crate::store::{db::Skill, AppState};
use serde::Serialize;
use tauri::Manager;
use tauri::State;
use tracing::{info, warn};

// reqwest is available as a transitive dependency from other tools
use reqwest;

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

#[derive(Debug, Serialize)]
pub struct SkillCatalogItem {
    pub name: String,
    pub description: String,
    pub version: String,
    pub source: String,
    pub tools: Vec<String>,
    pub dependencies: Vec<String>,
    pub permissions: Vec<String>,
}

#[tauri::command]
pub async fn scan_skill_catalog(state: State<'_, AppState>) -> Result<Vec<SkillCatalogItem>, String> {
    let app_dir = state
        .app_handle
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
    let skills_dir = app_dir.join("skills");
    let mut loader = crate::skills::loader::SkillLoader::new(skills_dir);
    loader.load_all().map_err(|e| e.to_string())?;
    let items = loader
        .list_skills()
        .into_iter()
        .map(|s| SkillCatalogItem {
            name: s.name.clone(),
            description: s.description.clone(),
            version: s.version.clone(),
            source: s.source.clone(),
            tools: s.tools.clone(),
            dependencies: s.dependencies.clone(),
            permissions: s.permissions.clone(),
        })
        .collect::<Vec<_>>();
    Ok(items)
}

/// Install a skill from a URL (raw SKILL.md) or local file path.
/// The SKILL.md is downloaded, parsed, and written to the app skills directory.
#[tauri::command]
pub async fn install_skill(
    state: State<'_, AppState>,
    source: String,
) -> Result<SkillCatalogItem, String> {
    let app_dir = state
        .app_handle
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
    let skills_dir = app_dir.join("skills");

    let content = if source.starts_with("http://") || source.starts_with("https://") {
        // Basic URL validation — reject internal/private addresses
        let blocked = ["localhost", "127.0.0.1", "0.0.0.0", "192.168.", "10.", "172."];
        for pat in blocked {
            if source.contains(pat) {
                return Err(format!("Blocked URL: '{}' points to a private/local address", source));
            }
        }
        info!("Downloading skill from URL: {}", source);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .get(&source)
            .header("User-Agent", "Pisci-Desktop/1.0")
            .send()
            .await
            .map_err(|e| format!("Download failed: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {} when downloading: {}", resp.status(), source));
        }
        resp.text().await.map_err(|e| format!("Failed to read response: {}", e))?
    } else {
        // Local file path
        tokio::fs::read_to_string(&source)
            .await
            .map_err(|e| format!("Failed to read local file '{}': {}", source, e))?
    };

    // Parse and validate frontmatter
    let loader = crate::skills::loader::SkillLoader::new(&skills_dir);
    let skill = loader
        .parse_skill_from_content(&content)
        .map_err(|e| format!("Failed to parse SKILL.md: {}", e))?;

    if skill.name.is_empty() || skill.name == "unnamed" {
        return Err("SKILL.md must declare a 'name' field in frontmatter".into());
    }

    // Warn if the skill declares elevated permissions
    if !skill.permissions.is_empty() {
        warn!(
            "Installing skill '{}' with permissions: {:?}",
            skill.name, skill.permissions
        );
    }

    // Sanitise name for use as directory name
    let safe_name: String = skill
        .name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect::<String>()
        .to_lowercase();

    let skill_dir = skills_dir.join(&safe_name);
    tokio::fs::create_dir_all(&skill_dir)
        .await
        .map_err(|e| format!("Failed to create skill directory: {}", e))?;

    let skill_file = skill_dir.join("SKILL.md");
    tokio::fs::write(&skill_file, &content)
        .await
        .map_err(|e| format!("Failed to write SKILL.md: {}", e))?;

    info!("Installed skill '{}' to {:?}", skill.name, skill_dir);

    Ok(SkillCatalogItem {
        name: skill.name,
        description: skill.description,
        version: skill.version,
        source: "installed".to_string(),
        tools: skill.tools,
        dependencies: skill.dependencies,
        permissions: skill.permissions,
    })
}

/// Remove an installed skill by name. Only skills whose source is "installed" or "workspace"
/// can be removed this way; built-in skills are protected.
#[tauri::command]
pub async fn uninstall_skill(
    state: State<'_, AppState>,
    skill_name: String,
) -> Result<(), String> {
    let app_dir = state
        .app_handle
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
    let skills_dir = app_dir.join("skills");

    let safe_name: String = skill_name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect::<String>()
        .to_lowercase();

    let skill_dir = skills_dir.join(&safe_name);

    if !skill_dir.exists() {
        return Err(format!("Skill '{}' not found", skill_name));
    }

    // Safety: must be inside skills_dir
    let canonical_dir = skill_dir.canonicalize().map_err(|e| e.to_string())?;
    let canonical_skills = skills_dir.canonicalize().map_err(|e| e.to_string())?;
    if !canonical_dir.starts_with(&canonical_skills) {
        return Err("Path traversal attempt blocked".into());
    }

    tokio::fs::remove_dir_all(&skill_dir)
        .await
        .map_err(|e| format!("Failed to remove skill: {}", e))?;

    info!("Uninstalled skill '{}'", skill_name);
    Ok(())
}
