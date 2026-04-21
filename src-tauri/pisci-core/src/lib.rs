//! pisci-core — OS/UI-neutral primitives shared across the kernel, the CLI
//! host and the desktop host.
//!
//! This crate intentionally depends only on small, portable crates
//! (`chrono`, `serde`, `serde_json`, ...) and never on Tauri or on the
//! kernel. It hosts the cross-cutting types every layer needs to agree on:
//! host traits, shared data models, the heartbeat / project-state rules,
//! scene policies, Koi prompt layers and small enums.

pub mod heartbeat;
pub mod host;
pub mod koi_prompt;
pub mod models;
pub mod project_state;
pub mod scene;
pub mod trial;
