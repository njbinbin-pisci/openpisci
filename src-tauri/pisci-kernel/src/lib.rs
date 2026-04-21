//! pisci-kernel — OS/UI-neutral agent runtime.
//!
//! This crate hosts the parts of OpenPisci that should run identically on
//! Tauri desktop, the `openpisci` CLI and any future host. It owns the agent
//! loop, LLM clients, local storage, memory layer, policy, scheduler and the
//! platform-neutral tool implementations.
//!
//! Platform-specific tools (Windows UIA, Chromium browser, PowerShell / WMI /
//! COM, IM gateways) deliberately stay in their host crate and are injected
//! into the kernel via the [`pisci_core::host`] traits.

pub mod agent;
pub mod headless;
pub mod llm;
pub mod memory;
pub mod policy;
pub mod pool;
pub mod project_context;
pub mod scheduler;
pub mod security;
pub mod store;
pub mod tools;

// Re-export for downstream crates that want `pisci_kernel::core::...`.
pub use pisci_core as core;

/// Version string for kernel builds — handy for `openpisci capabilities`.
pub const KERNEL_VERSION: &str = env!("CARGO_PKG_VERSION");
