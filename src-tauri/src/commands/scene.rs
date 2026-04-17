use crate::agent::tool::ToolRegistry;
use crate::browser::SharedBrowserManager;
use crate::skills::loader::SkillLoader;
use crate::store::{Database, Settings};
use crate::tools;
#[allow(unused_imports)]
pub use pisci_core::scene::{
    CollaborationContextMode, EventDigestMode, HistorySliceMode, MemorySliceMode, PoolSnapshotMode,
    RegistryProfile, SceneKind, ScenePolicy,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

pub type SharedSkillLoader = Arc<Mutex<SkillLoader>>;

pub fn load_skill_loader(app: &AppHandle) -> Option<SharedSkillLoader> {
    let app_data_dir = app.path().app_data_dir().ok()?;
    let mut loader = SkillLoader::new(app_data_dir.join("skills"));
    if let Err(error) = loader.load_all() {
        tracing::warn!("Failed to load skills: {}", error);
    }
    Some(Arc::new(Mutex::new(loader)))
}

#[allow(clippy::too_many_arguments)]
pub fn build_registry_for_scene(
    scene: SceneKind,
    browser: SharedBrowserManager,
    user_tools_dir: Option<&Path>,
    db: Option<Arc<Mutex<Database>>>,
    builtin_tool_enabled: Option<&HashMap<String, bool>>,
    app: Option<AppHandle>,
    settings: Option<Arc<Mutex<Settings>>>,
    app_data_dir: Option<PathBuf>,
    skill_loader: Option<SharedSkillLoader>,
) -> ToolRegistry {
    let policy = ScenePolicy::for_kind(scene);
    let mut registry = tools::build_registry(
        browser,
        user_tools_dir,
        db,
        builtin_tool_enabled,
        app,
        settings,
        app_data_dir,
        if policy.allow_skill_loader {
            skill_loader
        } else {
            None
        },
    );

    match policy.registry_profile {
        RegistryProfile::MainChat
        | RegistryProfile::PoolCoordinator
        | RegistryProfile::IMHeadless
        | RegistryProfile::HeartbeatSupervisor => {
            registry.unregister("call_koi");
        }
        RegistryProfile::KoiTask => {}
    }

    if let Some(allowlist) = policy.tool_allowlist() {
        registry.retain(|tool| allowlist.contains(&tool.name()));
    }

    registry
}

#[cfg(test)]
mod tests {
    use super::{CollaborationContextMode, EventDigestMode, SceneKind, ScenePolicy};

    #[test]
    fn heartbeat_scene_policy_is_lightweight_and_disables_proactive_compaction() {
        let policy = ScenePolicy::for_kind(SceneKind::HeartbeatSupervisor);
        assert!(!policy.include_memory);
        assert!(!policy.include_task_state);
        assert!(policy.include_pool_context);
        assert_eq!(policy.auto_compact_threshold_override, Some(0));
    }

    #[test]
    fn collaboration_context_rules_still_come_from_core_policy() {
        assert_eq!(
            ScenePolicy::for_kind(SceneKind::MainChat).collaboration_context_mode(),
            CollaborationContextMode::OnDemand
        );
        assert_eq!(
            ScenePolicy::for_kind(SceneKind::HeartbeatSupervisor).event_digest_mode(),
            EventDigestMode::CoordinationPlusFailures
        );
    }
}
