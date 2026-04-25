//! Config-domain Tauri commands — user-facing configuration & registries.
//!
//! This module aggregates everything the user tunes from Settings or that
//! reads back the application's static registries:
//!
//! - [`settings`] — global settings (LLM, theme, workspace, …)
//! - [`skills`] — skill catalog & install/uninstall, Clawhub bridge
//! - [`memory`] — long-term memory listing / add / delete
//! - [`mcp`] — MCP server list / save / test
//! - [`tools`] — builtin tool listing + manual heartbeat trigger
//! - [`user_tools`] — user-authored tool plugins (list / install / config)
//! - [`scene`] — scene-policy helpers + `ScenePolicy` re-exports
//! - [`audit`] — audit-log listing & clear
//!
//! None of these deal with chat turn execution or pool coordination; those
//! live in [`crate::commands::chat`] and [`crate::commands::pool`].

pub mod audit;
pub mod enterprise_capability;
pub mod mcp;
pub mod memory;
pub mod scene;
pub mod settings;
pub mod skills;
pub mod tools;
pub mod user_tools;
