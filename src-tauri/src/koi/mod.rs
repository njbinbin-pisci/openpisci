pub mod event_bus;
pub mod runtime;

/// Koi (锦鲤) — Persistent independent Agent system for OpenPisci.
///
/// Each Koi is a fully capable Agent with its own identity (name, icon, color),
/// system prompt, independent memory, and todo list. Unlike Fish (ephemeral
/// sub-Agents), Koi agents persist across sessions and can collaborate with
/// each other via @mentions in the Chat Pool.
///
/// Hierarchy:
///   Pisci (main Agent) → Koi (persistent, independent) → Fish (ephemeral)
use chrono::{DateTime, Utc};
pub use pisci_core::models::{KoiTodo, PoolMessage, PoolSession};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy)]
pub struct StarterKoiSpec {
    pub name: &'static str,
    pub role: &'static str,
    pub icon: &'static str,
    pub color: &'static str,
    pub system_prompt: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KoiDefinition {
    pub id: String,
    pub name: String,
    /// Free-form role label such as "架构师", "程序员", or "测试负责人".
    pub role: String,
    pub icon: String,
    pub color: String,
    pub system_prompt: String,
    pub description: String,
    /// Runtime status: "idle" | "busy" | "offline"
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Optional named LLM provider id. When set, this Koi uses the matching
    /// `LlmProviderConfig` from `Settings.llm_providers` instead of the global defaults.
    #[serde(default)]
    pub llm_provider_id: Option<String>,
    /// Maximum number of AgentLoop iterations for this Koi.
    /// 0 means use the system default (30).
    #[serde(default)]
    pub max_iterations: u32,
    /// Per-Koi default execution timeout for a single todo, in seconds.
    /// 0 means inherit the project or system default.
    #[serde(default)]
    pub task_timeout_secs: u32,
}

/// Preset color palette for Koi creation UI.
pub const KOI_COLORS: &[(&str, &str)] = &[
    ("#7c6af7", "Violet"),
    ("#4ecdc4", "Teal"),
    ("#45b7d1", "Sky"),
    ("#f7b731", "Gold"),
    ("#fc5c65", "Coral"),
    ("#26de81", "Emerald"),
    ("#a55eea", "Purple"),
    ("#fd9644", "Orange"),
    ("#778ca3", "Steel"),
    ("#eb3b5a", "Rose"),
    ("#20bf6b", "Green"),
    ("#2d98da", "Blue"),
];

/// Preset icons for Koi creation UI.
pub const KOI_ICONS: &[&str] = &[
    "🐙", "🦈", "🐬", "🦑", "🐳", "🐟", "🦐", "🦀", "🤖", "📊", "🎨", "💻", "🔬", "📝", "🛡️", "🌐",
    "🧠", "⚡", "🔧", "📁", "🎯", "🏗️", "🔍", "📡",
];

/// Default persistent Koi that are auto-created on the first packaged app launch.
pub const STARTER_KOI_SPECS: &[StarterKoiSpec] = &[
    StarterKoiSpec {
        name: "Architect",
        role: "架构师",
        icon: "🏗️",
        color: "#7c6af7",
        system_prompt:
            "You are a software architect. Your job is to design clear, practical technical specifications. \
             Be concise and structured. Output designs as numbered plans with explicit trade-offs, interfaces, and handoff points.",
        description: "Architecture, system design, technical specification",
    },
    StarterKoiSpec {
        name: "Coder",
        role: "程序员",
        icon: "💻",
        color: "#45b7d1",
        system_prompt:
            "You are a software developer. Given a specification, write clean, working code. \
             Be practical, prioritize correctness, and explain important implementation choices briefly.",
        description: "Implementation, coding, development",
    },
    StarterKoiSpec {
        name: "Reviewer",
        role: "代码审查员",
        icon: "🔍",
        color: "#26de81",
        system_prompt:
            "You are a code reviewer. Review designs and code critically but constructively. \
             Point out concrete risks, missing tests, regressions, and the smallest safe improvements.",
        description: "Code review, quality assurance, feedback",
    },
];
