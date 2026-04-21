//! Tauri command surface, grouped by domain.
//!
//! Each subdirectory is a cohesive domain of UI-invokable commands:
//!
//! - [`chat`] — chat sessions, headless agent runs, debug / harness scenarios,
//!   scheduler, IM gateway channels, Fish subagent listing, and
//!   collaboration-trial orchestration.
//! - [`pool`] — multi-agent pool coordination: pool sessions, Koi definitions,
//!   and the Kanban board for Koi todos.
//! - [`config`] — user-facing configuration & registries: settings, skills,
//!   memory, MCP servers, builtin tools, user tools, scene policies, and
//!   audit log.
//! - [`platform`] — host / OS primitives: runtime / VM status, window & tray
//!   management, permission prompts, and interactive UI responses.
//!
//! Only the domain module is declared here; each domain's head file owns its
//! own submodule list (see e.g. [`chat`] declaring `debug`, `gateway`,
//! `scheduler`, `collab_trial`, `fish`).

pub mod chat;
pub mod config;
pub mod platform;
pub mod pool;
