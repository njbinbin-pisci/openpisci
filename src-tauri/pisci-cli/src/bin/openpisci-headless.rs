//! `openpisci-headless` — fully host-agnostic CLI entry point.
//!
//! Wires a [`CliHost`] onto the pisci kernel and supports three subcommands:
//!
//!   * `capabilities [--mode pisci|pool]` — JSON report of OS, selected
//!     mode, and disabled tools. Does not talk to an LLM.
//!   * `version` — prints the kernel version.
//!   * `run --prompt <text> [--workspace <dir>] ...` — runs a single
//!     pisci-mode agent turn entirely inside `pisci-kernel`, with neutral
//!     tools only. `pool` mode is rejected here and must use the desktop
//!     `openpisci` binary.
//!
//! All events are serialised as NDJSON on stdout via [`CliEventSink`]; the
//! final response is either printed to stdout as pretty JSON or written to
//! `--output <file>`.

use pisci_cli::args::{parse_capabilities_mode, parse_run_request, print_usage, write_response};
use pisci_cli::runner::{resolve_app_data_dir, run_pisci_once};
use pisci_core::host::{DisabledToolInfo, HeadlessCliMode};
use serde_json::json;

fn current_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Names of tools that the CLI host deliberately does NOT register. Kept
/// here (not in the kernel) because "which desktop tools are missing" is a
/// host-policy decision rather than a kernel fact.
fn headless_disabled_tools(mode: HeadlessCliMode) -> Vec<DisabledToolInfo> {
    let common = [
        (
            "browser",
            "Disabled in openpisci-headless: no Chrome-for-Testing manager in the CLI host.",
        ),
        (
            "chat_ui",
            "Disabled in openpisci-headless: no interactive desktop chat UI.",
        ),
        (
            "plan_todo",
            "Disabled in openpisci-headless: plan state is not broadcast to a UI.",
        ),
        (
            "call_fish",
            "Disabled in openpisci-headless: fish sub-agents require an AppHandle.",
        ),
        (
            "call_koi",
            "Disabled in openpisci-headless: koi delegation requires AppState.",
        ),
        (
            "pool_org",
            "Disabled in openpisci-headless: pool orchestration is desktop-only.",
        ),
        (
            "pool_chat",
            "Disabled in openpisci-headless: pool orchestration is desktop-only.",
        ),
        (
            "app_control",
            "Disabled in openpisci-headless: desktop scheduler / settings UI not loaded.",
        ),
        (
            "skill_list",
            "Disabled in openpisci-headless: no skill loader wired into the CLI host.",
        ),
    ];

    let mut out: Vec<_> = common
        .iter()
        .map(|(n, r)| DisabledToolInfo {
            name: (*n).to_string(),
            reason: (*r).to_string(),
        })
        .collect();

    if !cfg!(target_os = "windows") {
        for (n, r) in [
            ("powershell_query", "Windows-only tool."),
            ("wmi", "Windows-only tool."),
            ("office", "Windows-only tool."),
            ("uia", "Windows-only tool."),
            ("screen_capture", "Windows-only tool."),
            ("com", "Windows-only tool."),
            ("com_invoke", "Windows-only tool."),
        ] {
            out.push(DisabledToolInfo {
                name: n.to_string(),
                reason: r.to_string(),
            });
        }
    }

    if matches!(mode, HeadlessCliMode::Pool) {
        out.push(DisabledToolInfo {
            name: "<run>".to_string(),
            reason:
                "openpisci-headless does not support pool mode; use the desktop `openpisci` binary."
                    .to_string(),
        });
    }

    out
}

fn run_subcommand(args: &[String]) -> Result<(), String> {
    let request = parse_run_request(args)?;
    let output_override = request.output.clone();
    let response = run_pisci_once(request)?;
    write_response(output_override.as_deref(), &response)
}

fn capabilities_subcommand(args: &[String]) -> Result<(), String> {
    let mode = parse_capabilities_mode(args)?;
    let app_data_dir = resolve_app_data_dir(None);
    let report = json!({
        "kernel_version": pisci_kernel::KERNEL_VERSION,
        "headless": true,
        "host": "cli",
        "os": current_os(),
        "mode": mode.as_str(),
        "app_data_dir": app_data_dir,
        "tools_profile": "headless",
        "disabled_tools": headless_disabled_tools(mode),
        "schema": {
            "headless_cli_request": "pisci_core::host::HeadlessCliRequest",
            "headless_cli_response": "pisci_core::host::HeadlessCliResponse",
        }
    });
    let json_str =
        serde_json::to_string_pretty(&report).map_err(|e| format!("Serialize failed: {e}"))?;
    println!("{json_str}");
    Ok(())
}

fn real_main(args: &[String]) -> Result<(), String> {
    let subcommand = args.first().map(String::as_str).unwrap_or("capabilities");
    match subcommand {
        "run" => run_subcommand(&args[1..]),
        "capabilities" | "caps" | "--capabilities" => capabilities_subcommand(&args[1..]),
        "--version" | "version" | "-v" => {
            println!("openpisci-headless {}", pisci_kernel::KERNEL_VERSION);
            Ok(())
        }
        "--help" | "-h" | "help" => {
            print_usage();
            Ok(())
        }
        other => {
            eprintln!(
                "openpisci-headless: unknown subcommand `{other}`. Available: run, capabilities, version"
            );
            Err(String::new())
        }
    }
}

fn main() {
    // Lightweight stderr-based tracing so kernel log lines surface when the
    // user sets `RUST_LOG=info` (CI / debugging) without polluting the
    // NDJSON stream on stdout.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    // Force the argv Vec to be dropped before exit so destructors flush.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = real_main(&args);
    if let Err(err) = result {
        if !err.is_empty() {
            eprintln!("{err}");
        }
        std::process::exit(1);
    }
}
