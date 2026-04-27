use crate::browser::SharedBrowserManager;
use crate::commands::config::mcp::resolve_settings_placeholders_in_mcp_config;
use crate::host::DesktopHostTools;
use crate::skills::loader::SkillLoader;
use crate::store::{Database, Settings};
use pisci_core::scene::RegistryProfile;
pub use pisci_core::scene::{
    HistorySliceMode, MemorySliceMode, PoolSnapshotMode, SceneKind, ScenePolicy,
};
use pisci_kernel::agent::tool::ToolRegistry;
use pisci_kernel::tools::register_mcp_tools;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::UNIX_EPOCH;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

pub type SharedSkillLoader = Arc<Mutex<SkillLoader>>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SkillDirSignature {
    entries: usize,
    latest_modified_ms: u128,
}

#[derive(Clone)]
struct CachedSkillLoader {
    signature: SkillDirSignature,
    loader: SharedSkillLoader,
}

static SKILL_LOADER_CACHE: OnceLock<StdMutex<HashMap<PathBuf, CachedSkillLoader>>> =
    OnceLock::new();

fn skill_loader_cache() -> &'static StdMutex<HashMap<PathBuf, CachedSkillLoader>> {
    SKILL_LOADER_CACHE.get_or_init(|| StdMutex::new(HashMap::new()))
}

fn modified_ms(path: &Path) -> u128 {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

fn skill_dir_signature(skills_dir: &Path) -> SkillDirSignature {
    let mut signature = SkillDirSignature::default();
    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return signature;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_file = path.join("SKILL.md");
        if !skill_file.exists() {
            continue;
        }
        signature.entries += 1;
        signature.latest_modified_ms = signature
            .latest_modified_ms
            .max(modified_ms(&path))
            .max(modified_ms(&skill_file));
    }
    signature
}

pub fn load_skill_loader(app: &AppHandle) -> Option<SharedSkillLoader> {
    let app_data_dir = app.path().app_data_dir().ok()?;
    let skills_dir = app_data_dir.join("skills");
    let signature = skill_dir_signature(&skills_dir);
    if let Ok(cache) = skill_loader_cache().lock() {
        if let Some(cached) = cache.get(&skills_dir) {
            if cached.signature == signature {
                return Some(cached.loader.clone());
            }
        }
    }

    let mut loader = SkillLoader::new(&skills_dir);
    if let Err(error) = loader.load_all() {
        tracing::warn!("Failed to load skills: {}", error);
    }
    let loader = Arc::new(Mutex::new(loader));
    let signature = skill_dir_signature(&skills_dir);
    if let Ok(mut cache) = skill_loader_cache().lock() {
        cache.insert(
            skills_dir,
            CachedSkillLoader {
                signature,
                loader: loader.clone(),
            },
        );
    }
    Some(loader)
}

#[allow(clippy::too_many_arguments)]
pub async fn build_registry_for_scene(
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

    // Snapshot MCP server configs before the Settings mutex is moved into
    // `DesktopHostTools`. Only scenes that opt-in (main-chat / koi task /
    // IM headless) actually touch MCP — everyone else skips the I/O.
    //
    // Per the layered IM architecture (credentials shared, channel and
    // capability independent), MCP servers hosting enterprise APIs
    // (DingTalk / Feishu / WeCom CLI) often need the *same* application
    // credentials the IM channel already has. We expand `${settings:*}`
    // placeholders inside each server's `env` map so users don't need to
    // duplicate bot_id/app_secret values into the MCP config — the
    // single source of truth is Settings → IM → application credentials.
    let mcp_servers = if policy.allow_mcp_tools {
        if let Some(ref settings_arc) = settings {
            let guard = settings_arc.lock().await;
            guard
                .mcp_servers
                .iter()
                .map(|server| resolve_settings_placeholders_in_mcp_config(server, &guard))
                .collect()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

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
            registry.unregister("pool_chat");
        }
        RegistryProfile::KoiTask => {}
    }

    if let Some(allowlist) = policy.tool_allowlist() {
        registry.retain(|tool| allowlist.contains(&tool.name()));
    }

    // MCP tools are registered *after* the allowlist filter — their tool
    // names are user-configured (e.g. `git.status`, `notion.search`) and
    // cannot be enumerated statically, so the allowlist would strip them.
    if !mcp_servers.is_empty() {
        register_mcp_tools(&mut registry, &mcp_servers).await;
    }

    registry
}

#[cfg(test)]
mod tests {
    use super::{SceneKind, ScenePolicy};
    use pisci_core::scene::{CollaborationContextMode, EventDigestMode};

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

    #[test]
    fn pisci_profiles_do_not_expose_pool_chat() {
        for kind in [SceneKind::PoolCoordinator, SceneKind::HeartbeatSupervisor] {
            let allowlist = ScenePolicy::for_kind(kind)
                .tool_allowlist()
                .expect("pisci coordinator profiles use allowlists");
            assert!(!allowlist.contains(&"pool_chat"));
            assert!(allowlist.contains(&"pool_org"));
        }

        let koi_allowlist = ScenePolicy::for_kind(SceneKind::KoiTask)
            .tool_allowlist()
            .expect("koi task profile uses an allowlist");
        assert!(koi_allowlist.contains(&"pool_chat"));
    }
}
