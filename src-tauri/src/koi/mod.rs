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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KoiTodo {
    pub id: String,
    pub owner_id: String,
    pub title: String,
    pub description: String,
    /// "todo" | "in_progress" | "done" | "blocked" | "cancelled"
    pub status: String,
    /// "low" | "medium" | "high" | "urgent"
    pub priority: String,
    pub assigned_by: String,
    pub pool_session_id: Option<String>,
    /// Who claimed/is working on this task (koi_id or empty)
    pub claimed_by: Option<String>,
    pub claimed_at: Option<DateTime<Utc>>,
    /// Comma-separated todo IDs that this task depends on
    pub depends_on: Option<String>,
    /// Reason for blocked status
    pub blocked_reason: Option<String>,
    /// Pool message ID that contains the result
    pub result_message_id: Option<i64>,
    /// "pisci" | "koi" | "user" | "system"
    pub source_type: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolSession {
    pub id: String,
    pub name: String,
    /// Organization spec for this project pool (POOL.md equivalent).
    /// Contains: project goals, Koi role definitions, collaboration rules,
    /// activation conditions, evaluation metrics.
    pub org_spec: String,
    /// "active" | "paused" | "archived"
    pub status: String,
    /// Optional filesystem directory for this project.
    /// When set, a Git repo is initialized and Koi get isolated worktrees.
    pub project_dir: Option<String>,
    pub last_active_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolMessage {
    pub id: i64,
    pub pool_session_id: String,
    pub sender_id: String,
    pub content: String,
    /// "text" | "task_assign" | "status_update" | "result" | "mention" | "task_claimed" | "task_blocked" | "task_done"
    pub msg_type: String,
    pub metadata: String,
    /// Link to the related koi_todo
    pub todo_id: Option<String>,
    /// Reply threading
    pub reply_to_message_id: Option<i64>,
    /// Structured event type for timeline reconstruction
    pub event_type: Option<String>,
    pub created_at: DateTime<Utc>,
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
