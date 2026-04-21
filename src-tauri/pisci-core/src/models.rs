use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KoiTodo {
    pub id: String,
    pub owner_id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub priority: String,
    pub assigned_by: String,
    pub pool_session_id: Option<String>,
    pub claimed_by: Option<String>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub depends_on: Option<String>,
    pub blocked_reason: Option<String>,
    pub result_message_id: Option<i64>,
    pub source_type: String,
    #[serde(default)]
    pub task_timeout_secs: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolSession {
    pub id: String,
    pub name: String,
    pub org_spec: String,
    pub status: String,
    pub project_dir: Option<String>,
    #[serde(default)]
    pub task_timeout_secs: u32,
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
    pub msg_type: String,
    pub metadata: String,
    pub todo_id: Option<String>,
    pub reply_to_message_id: Option<i64>,
    pub event_type: Option<String>,
    pub created_at: DateTime<Utc>,
}

// -- Koi definitions -------------------------------------------------
// Kept in `pisci-core::models` so that both the kernel's database layer
// and the desktop-side koi runtime can share the same data shapes
// without creating a kernel → desktop dependency.

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
    pub role: String,
    pub icon: String,
    pub color: String,
    pub system_prompt: String,
    pub description: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub llm_provider_id: Option<String>,
    #[serde(default)]
    pub max_iterations: u32,
    #[serde(default)]
    pub task_timeout_secs: u32,
}

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

pub const KOI_ICONS: &[&str] = &[
    "\u{1F419}",
    "\u{1F988}",
    "\u{1F42C}",
    "\u{1F991}",
    "\u{1F433}",
    "\u{1F41F}",
    "\u{1F990}",
    "\u{1F980}",
    "\u{1F916}",
    "\u{1F4CA}",
    "\u{1F3A8}",
    "\u{1F4BB}",
    "\u{1F52C}",
    "\u{1F4DD}",
    "\u{1F6E1}\u{FE0F}",
    "\u{1F310}",
    "\u{1F9E0}",
    "\u{26A1}",
    "\u{1F527}",
    "\u{1F4C1}",
    "\u{1F3AF}",
    "\u{1F3D7}\u{FE0F}",
    "\u{1F50D}",
    "\u{1F4E1}",
];

pub const STARTER_KOI_SPECS: &[StarterKoiSpec] = &[
    StarterKoiSpec {
        name: "Architect",
        role: "\u{67B6}\u{6784}\u{5E08}",
        icon: "\u{1F3D7}\u{FE0F}",
        color: "#7c6af7",
        system_prompt:
            "You are a software architect. Your job is to design clear, practical technical specifications. \
             Be concise and structured. Output designs as numbered plans with explicit trade-offs, interfaces, and handoff points.",
        description: "Architecture, system design, technical specification",
    },
    StarterKoiSpec {
        name: "Coder",
        role: "\u{7A0B}\u{5E8F}\u{5458}",
        icon: "\u{1F4BB}",
        color: "#45b7d1",
        system_prompt:
            "You are a software developer. Given a specification, write clean, working code. \
             Be practical, prioritize correctness, and explain important implementation choices briefly.",
        description: "Implementation, coding, development",
    },
    StarterKoiSpec {
        name: "Reviewer",
        role: "\u{4EE3}\u{7801}\u{5BA1}\u{67E5}\u{5458}",
        icon: "\u{1F50D}",
        color: "#26de81",
        system_prompt:
            "You are a code reviewer. Review designs and code critically but constructively. \
             Point out concrete risks, missing tests, regressions, and the smallest safe improvements.",
        description: "Code review, quality assurance, feedback",
    },
];
