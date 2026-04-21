use crate::browser::SharedBrowserManager;
use crate::host::DesktopHostTools;
use crate::skills::loader::SkillLoader;
use crate::store::{Database, Settings};
pub use pisci_core::scene::{
    CollaborationContextMode, HistorySliceMode, MemorySliceMode, PoolSnapshotMode, SceneKind,
    ScenePolicy,
};
use pisci_core::scene::RegistryProfile;
use pisci_kernel::agent::tool::ToolRegistry;
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

    // `fill_pool_defaults()` auto-populates the four kernel pool seams
    // (event_sink / plan_store / pool_event_sink / pool_mention_dispatcher)
    // from the scene's `AppHandle` + `Database`, so the neutral kernel
    // tools (`plan_todo`, `pool_org`, `pool_chat`) light up without each
    // call site repeating the boilerplate.
    let mut registry = DesktopHostTools {
        browser: Some(browser),
        db,
        settings,
        app_handle: app,
        app_data_dir,
        skill_loader: if policy.allow_skill_loader {
            skill_loader
        } else {
            None
        },
        builtin_tool_enabled: builtin_tool_enabled.cloned(),
        user_tools_dir: user_tools_dir.map(PathBuf::from),
        ..DesktopHostTools::default()
    }
    .fill_pool_defaults()
    .build_registry();

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
    use super::{CollaborationContextMode, SceneKind, ScenePolicy};
    use pisci_core::scene::EventDigestMode;

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
