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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KoiDefinition {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub color: String,
    pub system_prompt: String,
    pub description: String,
    /// Runtime status: "idle" | "busy" | "offline"
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolSession {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolMessage {
    pub id: i64,
    pub pool_session_id: String,
    pub sender_id: String,
    pub content: String,
    /// "text" | "task_assign" | "status_update" | "result" | "mention"
    pub msg_type: String,
    pub metadata: String,
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
    "🐙", "🦈", "🐬", "🦑", "🐳", "🐟", "🦐", "🦀",
    "🤖", "📊", "🎨", "💻", "🔬", "📝", "🛡️", "🌐",
    "🧠", "⚡", "🔧", "📁", "🎯", "🏗️", "🔍", "📡",
];
