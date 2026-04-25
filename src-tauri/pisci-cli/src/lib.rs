//! pisci-cli — headless host adapter for the OpenPisci kernel.
//!
//! This crate implements the [`pisci_core::host`] traits for a pure CLI
//! environment: events are serialised as JSON lines to stdout, confirmation /
//! interactive prompts fall back to deterministic defaults, and no
//! platform-specific tools are registered. It exists so that the
//! `openpisci-headless` binary no longer needs Tauri at runtime.
//!
//! The concrete `CliHost` and its `EventSink` / `Notifier` implementations
//! live in submodules; the binary under `src/bin/openpisci.rs` wires them up
//! with the kernel's headless entry point.

pub mod args;
pub mod host;
pub mod interactive;
pub mod rpc_server;
pub mod runner;
