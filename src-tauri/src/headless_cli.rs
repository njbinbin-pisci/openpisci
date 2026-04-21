//! Desktop-side glue for the `openpisci` headless CLI.
//!
//! Canonical schemas live in [`pisci_core::host`]; shared argument parsing
//! and response writing live in [`pisci_cli::args`]. This module only owns
//! the pieces that require desktop-specific knowledge:
//!
//!   * [`disabled_tools_for_mode`] — computed from the desktop tool profile.
//!   * [`run_from_env_args`] — the `openpisci` entry point. It now **only**
//!     boots the full Tauri `AppState` for pool-mode turns; pisci-mode
//!     turns dispatch directly into [`pisci_cli::runner::run_pisci_once`]
//!     so the two headless binaries (`openpisci --mode pisci` and
//!     `openpisci-headless run --mode pisci`) share a single kernel code
//!     path.

use crate::tools::{self, RuntimeToolProfile};
use pisci_cli::args::{
    parse_capabilities_mode, parse_mode, parse_run_request, print_usage, write_response,
};
use serde::Serialize;

pub use pisci_core::host::{
    DisabledToolInfo, HeadlessCliMode, HeadlessCliRequest, HeadlessCliResponse,
    HeadlessContextToggles, PoolWaitSummary,
};

/// Map a canonical `HeadlessCliMode` onto the desktop's runtime tool
/// profile so we can answer "which tools are disabled?" without duplicating
/// the enum.
pub(crate) fn tool_profile(mode: HeadlessCliMode) -> RuntimeToolProfile {
    match mode {
        HeadlessCliMode::Pisci => RuntimeToolProfile::HeadlessPisci,
        HeadlessCliMode::Pool => RuntimeToolProfile::HeadlessPool,
    }
}

pub fn disabled_tools_for_mode(mode: HeadlessCliMode) -> Vec<DisabledToolInfo> {
    tools::runtime_disabled_tools(tool_profile(mode))
        .into_iter()
        .map(|tool| DisabledToolInfo {
            name: tool.name.to_string(),
            reason: tool
                .reason
                .unwrap_or("Disabled by runtime profile.")
                .to_string(),
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
struct CapabilityReport {
    os: &'static str,
    mode: &'static str,
    disabled_tools: Vec<DisabledToolInfo>,
}

fn current_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Dispatch loop for the `openpisci` binary (desktop). Inherits the full
/// Tauri `AppState` so it can run pool-mode and koi-delegation turns.
pub fn run_from_env_args() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        return Err("Missing subcommand.".to_string());
    }

    match args[0].as_str() {
        "run" => {
            let request = parse_run_request(&args[1..])?;
            let output = request.output.clone();
            // Fast path: pisci mode goes through the kernel-only runner
            // in `pisci-cli`, bypassing Tauri boot entirely. Only
            // pool-mode turns still need a live AppState + Tauri runtime.
            let response = match request.mode {
                HeadlessCliMode::Pisci => pisci_cli::runner::run_pisci_once(request)?,
                HeadlessCliMode::Pool => crate::desktop_app::run_headless_cli(request)?,
            };
            write_response(output.as_deref(), &response)
        }
        "capabilities" => {
            let mode = parse_capabilities_mode(&args[1..])?;
            let report = CapabilityReport {
                os: current_os(),
                mode: mode.as_str(),
                disabled_tools: disabled_tools_for_mode(mode),
            };
            let json = serde_json::to_string_pretty(&report)
                .map_err(|e| format!("Serialize failed: {e}"))?;
            println!("{json}");
            Ok(())
        }
        "--help" | "-h" | "help" => {
            print_usage();
            Ok(())
        }
        other => {
            // Reuse the shared mode parser to accept legacy `--mode pisci`
            // style invocations that never had a subcommand — fall back to
            // an error message that points at the right usage banner.
            let _ = parse_mode(other);
            print_usage();
            Err(format!("Unknown subcommand '{other}'."))
        }
    }
}
