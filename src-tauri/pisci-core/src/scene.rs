#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneKind {
    MainChat,
    PoolCoordinator,
    KoiTask,
    IMHeadless,
    HeartbeatSupervisor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryProfile {
    MainChat,
    PoolCoordinator,
    KoiTask,
    IMHeadless,
    HeartbeatSupervisor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollaborationContextMode {
    Never,
    OnDemand,
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistorySliceMode {
    FullRecent,
    SummaryOnly,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventDigestMode {
    Off,
    CoordinationOnly,
    CoordinationPlusFailures,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySliceMode {
    Off,
    ScopedSearch,
    ScopedPlusRecent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolSnapshotMode {
    Off,
    Compact,
    Full,
}

const POOL_COORDINATOR_TOOLS: &[&str] = &[
    "file_read",
    "file_write",
    "file_edit",
    "file_diff",
    "code_run",
    "file_search",
    "file_list",
    "shell",
    "web_search",
    "browser",
    "memory_store",
    "call_fish",
    "pool_org",
    "pool_chat",
    "vision_context",
    "skill_list",
    "ssh",
    "pdf",
];

const KOI_TASK_TOOLS: &[&str] = &[
    "file_read",
    "file_write",
    "file_edit",
    "file_diff",
    "code_run",
    "file_search",
    "file_list",
    "shell",
    "web_search",
    "browser",
    "memory_store",
    "call_fish",
    "call_koi",
    "pool_org",
    "pool_chat",
    "vision_context",
    "skill_list",
    "ssh",
    "pdf",
];

const HEARTBEAT_SUPERVISOR_TOOLS: &[&str] = &[
    "file_read",
    "file_write",
    "file_edit",
    "file_diff",
    "code_run",
    "file_search",
    "file_list",
    "shell",
    "web_search",
    "browser",
    "pool_org",
    "pool_chat",
    "vision_context",
    "ssh",
    "pdf",
];

#[derive(Debug, Clone, Copy)]
pub struct ScenePolicy {
    pub registry_profile: RegistryProfile,
    pub allow_skill_loader: bool,
    pub include_memory: bool,
    pub include_task_state: bool,
    pub include_pool_roster: bool,
    pub include_pool_context: bool,
    pub include_project_instructions: bool,
    pub injection_budget_ratio: f64,
    pub injection_budget_min_chars: usize,
    pub auto_compact_threshold_override: Option<u32>,
}

fn compute_total_input_budget(context_window: u32, max_tokens: u32) -> usize {
    let window = if context_window > 0 {
        context_window as usize
    } else {
        match max_tokens {
            t if t >= 8192 => 128_000,
            t if t >= 4096 => 64_000,
            _ => 32_000,
        }
    };
    let usable = window.saturating_sub(max_tokens as usize);
    (usable as f64 * 0.85) as usize
}

impl ScenePolicy {
    pub fn for_kind(kind: SceneKind) -> Self {
        match kind {
            SceneKind::MainChat => Self {
                registry_profile: RegistryProfile::MainChat,
                allow_skill_loader: true,
                include_memory: true,
                include_task_state: true,
                include_pool_roster: true,
                include_pool_context: false,
                include_project_instructions: true,
                injection_budget_ratio: 0.15,
                injection_budget_min_chars: 2_000,
                auto_compact_threshold_override: None,
            },
            SceneKind::PoolCoordinator => Self {
                registry_profile: RegistryProfile::PoolCoordinator,
                allow_skill_loader: false,
                include_memory: true,
                include_task_state: true,
                include_pool_roster: false,
                include_pool_context: true,
                include_project_instructions: false,
                injection_budget_ratio: 0.10,
                injection_budget_min_chars: 1_500,
                auto_compact_threshold_override: Some(0),
            },
            SceneKind::KoiTask => Self {
                registry_profile: RegistryProfile::KoiTask,
                allow_skill_loader: true,
                include_memory: true,
                include_task_state: false,
                include_pool_roster: false,
                include_pool_context: true,
                include_project_instructions: false,
                injection_budget_ratio: 0.10,
                injection_budget_min_chars: 1_500,
                auto_compact_threshold_override: Some(0),
            },
            SceneKind::IMHeadless => Self {
                registry_profile: RegistryProfile::IMHeadless,
                allow_skill_loader: false,
                include_memory: true,
                include_task_state: true,
                include_pool_roster: false,
                include_pool_context: false,
                include_project_instructions: false,
                injection_budget_ratio: 0.10,
                injection_budget_min_chars: 1_500,
                auto_compact_threshold_override: Some(0),
            },
            SceneKind::HeartbeatSupervisor => Self {
                registry_profile: RegistryProfile::HeartbeatSupervisor,
                allow_skill_loader: false,
                include_memory: false,
                include_task_state: false,
                include_pool_roster: false,
                include_pool_context: true,
                include_project_instructions: false,
                injection_budget_ratio: 0.06,
                injection_budget_min_chars: 1_200,
                auto_compact_threshold_override: Some(0),
            },
        }
    }

    pub fn compute_injection_budget(self, context_window: u32, max_tokens: u32) -> usize {
        let total_budget = compute_total_input_budget(context_window, max_tokens);
        ((total_budget as f64 * self.injection_budget_ratio) as usize * 4)
            .max(self.injection_budget_min_chars)
    }

    pub fn effective_auto_compact_threshold(self, configured: u32) -> u32 {
        self.auto_compact_threshold_override.unwrap_or(configured)
    }

    pub fn project_instructions_enabled(self, configured: bool) -> bool {
        self.include_project_instructions && configured
    }

    pub fn collaboration_context_mode(self) -> CollaborationContextMode {
        match self.registry_profile {
            RegistryProfile::MainChat => CollaborationContextMode::OnDemand,
            RegistryProfile::IMHeadless => CollaborationContextMode::Never,
            RegistryProfile::PoolCoordinator
            | RegistryProfile::KoiTask
            | RegistryProfile::HeartbeatSupervisor => CollaborationContextMode::Required,
        }
    }

    pub fn tool_allowlist(self) -> Option<&'static [&'static str]> {
        match self.registry_profile {
            RegistryProfile::MainChat | RegistryProfile::IMHeadless => None,
            RegistryProfile::PoolCoordinator => Some(POOL_COORDINATOR_TOOLS),
            RegistryProfile::KoiTask => Some(KOI_TASK_TOOLS),
            RegistryProfile::HeartbeatSupervisor => Some(HEARTBEAT_SUPERVISOR_TOOLS),
        }
    }

    pub fn history_slice_mode(self) -> HistorySliceMode {
        match self.registry_profile {
            RegistryProfile::MainChat | RegistryProfile::IMHeadless => HistorySliceMode::FullRecent,
            RegistryProfile::PoolCoordinator | RegistryProfile::HeartbeatSupervisor => {
                HistorySliceMode::SummaryOnly
            }
            RegistryProfile::KoiTask => HistorySliceMode::None,
        }
    }

    pub fn event_digest_mode(self) -> EventDigestMode {
        match self.registry_profile {
            RegistryProfile::MainChat | RegistryProfile::IMHeadless => EventDigestMode::Off,
            RegistryProfile::PoolCoordinator => EventDigestMode::CoordinationPlusFailures,
            RegistryProfile::KoiTask => EventDigestMode::CoordinationPlusFailures,
            RegistryProfile::HeartbeatSupervisor => EventDigestMode::CoordinationPlusFailures,
        }
    }

    pub fn memory_slice_mode(self) -> MemorySliceMode {
        if !self.include_memory {
            return MemorySliceMode::Off;
        }
        match self.registry_profile {
            RegistryProfile::KoiTask => MemorySliceMode::ScopedPlusRecent,
            RegistryProfile::MainChat
            | RegistryProfile::PoolCoordinator
            | RegistryProfile::IMHeadless
            | RegistryProfile::HeartbeatSupervisor => MemorySliceMode::ScopedSearch,
        }
    }

    pub fn pool_snapshot_mode(self) -> PoolSnapshotMode {
        match self.registry_profile {
            RegistryProfile::IMHeadless => PoolSnapshotMode::Off,
            RegistryProfile::KoiTask
            | RegistryProfile::PoolCoordinator
            | RegistryProfile::HeartbeatSupervisor => PoolSnapshotMode::Compact,
            RegistryProfile::MainChat => PoolSnapshotMode::Full,
        }
    }

    pub fn recent_pool_message_limit(self) -> usize {
        match self.registry_profile {
            RegistryProfile::MainChat => 8,
            RegistryProfile::PoolCoordinator => 10,
            RegistryProfile::KoiTask => 12,
            RegistryProfile::IMHeadless => 0,
            RegistryProfile::HeartbeatSupervisor => 6,
        }
    }

    pub fn recent_pool_message_chars(self) -> usize {
        match self.registry_profile {
            RegistryProfile::MainChat => 180,
            RegistryProfile::PoolCoordinator => 220,
            RegistryProfile::KoiTask => 260,
            RegistryProfile::IMHeadless => 0,
            RegistryProfile::HeartbeatSupervisor => 180,
        }
    }

    pub fn org_spec_preview_chars(self) -> usize {
        match self.registry_profile {
            RegistryProfile::MainChat => 600,
            RegistryProfile::PoolCoordinator => 900,
            RegistryProfile::KoiTask => 1_200,
            RegistryProfile::IMHeadless => 0,
            RegistryProfile::HeartbeatSupervisor => 600,
        }
    }
}
