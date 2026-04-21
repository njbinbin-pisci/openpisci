#![recursion_limit = "512"]

// lib.rs — Tauri application library entry point.
// main.rs calls run() from here; this allows Tauri mobile targets to work.

pub mod app;
mod browser;
mod commands;
mod fish;
mod gateway;
pub mod headless_cli;
pub mod host;

pub mod koi;
#[cfg(test)]
mod live_smoke;
mod pisci;
mod skills;
pub mod store;
mod tools;

// -- Re-exports from the kernel ------------------------------------------
//
// These modules physically live in `pisci-kernel` but many call sites in
// the desktop crate still refer to them via `crate::agent::...`,
// `crate::llm::...`, `crate::memory::...`, etc. The `pub use` bindings
// below make those paths resolve transparently so that moving code to the
// kernel does not require touching hundreds of `use` statements across
// the desktop codebase.
pub use pisci_kernel::agent;
pub use pisci_kernel::llm;
pub use pisci_kernel::memory;
pub use pisci_kernel::policy;
pub use pisci_kernel::project_context;
pub use pisci_kernel::scheduler;
pub use pisci_kernel::security;

pub use app::run;
pub use store::AppState;
