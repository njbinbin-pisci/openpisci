pub mod bridge;
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
pub use pisci_core::models::{
    KoiDefinition, KoiTodo, PoolMessage, PoolSession, StarterKoiSpec, KOI_COLORS, KOI_ICONS,
    STARTER_KOI_SPECS,
};
