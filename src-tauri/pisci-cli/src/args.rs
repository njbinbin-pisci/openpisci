//! Shared CLI argument parsing for the `openpisci` / `openpisci-headless`
//! binaries.
//!
//! Both bins accept the same schema (`capabilities` + `run`) with the same
//! flag surface. Centralising the parser here lets the desktop binary and
//! the CLI-only binary stay in lock-step without copy-paste drift.

use std::fs;
use std::path::{Path, PathBuf};

use pisci_core::host::{HeadlessCliMode, HeadlessCliRequest, HeadlessCliResponse};

/// Opinionated default for the usage banner printed on `--help` / bad args.
pub const USAGE: &str = concat!(
    "Usage:\n",
    "  openpisci-headless                                     # interactive REPL (same as `chat`)\n",
    "  openpisci-headless chat                                # interactive REPL\n",
    "  openpisci-headless run --prompt <text> [--workspace <dir>] [--mode pisci|pool] [--output <file>]\n",
    "  openpisci-headless run --input <request.json> [--output <result.json>]\n",
    "  openpisci-headless capabilities [--mode pisci|pool]\n",
    "  openpisci-headless version\n",
);

pub fn print_usage() {
    eprintln!("{USAGE}");
}

#[derive(Default)]
pub struct RunArgOverrides {
    pub prompt: Option<String>,
    pub workspace: Option<String>,
    pub mode: Option<HeadlessCliMode>,
    pub session_id: Option<String>,
    pub session_title: Option<String>,
    pub channel: Option<String>,
    pub config_dir: Option<String>,
    pub pool_id: Option<String>,
    pub pool_name: Option<String>,
    pub pool_size: Option<u32>,
    pub koi_ids: Option<Vec<String>>,
    pub task_timeout_secs: Option<u32>,
    pub wait_for_completion: bool,
    pub wait_timeout_secs: Option<u64>,
    pub extra_system_context: Option<String>,
    pub output: Option<String>,
}

pub fn parse_mode(raw: &str) -> Result<HeadlessCliMode, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pisci" => Ok(HeadlessCliMode::Pisci),
        "pool" => Ok(HeadlessCliMode::Pool),
        other => Err(format!(
            "Unsupported mode '{other}'. Use 'pisci' or 'pool'."
        )),
    }
}

fn next_value(args: &[String], idx: &mut usize, flag: &str) -> Result<String, String> {
    *idx += 1;
    args.get(*idx)
        .cloned()
        .ok_or_else(|| format!("Missing value for '{flag}'."))
}

/// Parse the trailing portion of `openpisci-headless run <...>` into a
/// [`HeadlessCliRequest`]. `args` should exclude the `run` subcommand
/// itself.
pub fn parse_run_request(args: &[String]) -> Result<HeadlessCliRequest, String> {
    let mut input_path: Option<PathBuf> = None;
    let mut overrides = RunArgOverrides::default();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--input" => input_path = Some(PathBuf::from(next_value(args, &mut i, "--input")?)),
            "--output" => overrides.output = Some(next_value(args, &mut i, "--output")?),
            "--prompt" => overrides.prompt = Some(next_value(args, &mut i, "--prompt")?),
            "--workspace" => overrides.workspace = Some(next_value(args, &mut i, "--workspace")?),
            "--mode" => overrides.mode = Some(parse_mode(&next_value(args, &mut i, "--mode")?)?),
            "--session-id" => {
                overrides.session_id = Some(next_value(args, &mut i, "--session-id")?)
            }
            "--session-title" => {
                overrides.session_title = Some(next_value(args, &mut i, "--session-title")?)
            }
            "--channel" => overrides.channel = Some(next_value(args, &mut i, "--channel")?),
            "--config-dir" => {
                overrides.config_dir = Some(next_value(args, &mut i, "--config-dir")?)
            }
            "--pool-id" => overrides.pool_id = Some(next_value(args, &mut i, "--pool-id")?),
            "--pool-name" => overrides.pool_name = Some(next_value(args, &mut i, "--pool-name")?),
            "--pool-size" => {
                let raw = next_value(args, &mut i, "--pool-size")?;
                overrides.pool_size = Some(
                    raw.parse::<u32>()
                        .map_err(|_| format!("Invalid --pool-size '{raw}'."))?,
                );
            }
            "--koi-ids" => {
                let raw = next_value(args, &mut i, "--koi-ids")?;
                let items = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                overrides.koi_ids = Some(items);
            }
            "--task-timeout-secs" => {
                let raw = next_value(args, &mut i, "--task-timeout-secs")?;
                overrides.task_timeout_secs = Some(
                    raw.parse::<u32>()
                        .map_err(|_| format!("Invalid --task-timeout-secs '{raw}'."))?,
                );
            }
            "--wait-for-completion" => overrides.wait_for_completion = true,
            "--wait-timeout-secs" => {
                let raw = next_value(args, &mut i, "--wait-timeout-secs")?;
                overrides.wait_timeout_secs = Some(
                    raw.parse::<u64>()
                        .map_err(|_| format!("Invalid --wait-timeout-secs '{raw}'."))?,
                );
            }
            "--extra-system-context" => {
                overrides.extra_system_context =
                    Some(next_value(args, &mut i, "--extra-system-context")?)
            }
            "--help" | "-h" => {
                print_usage();
                return Err(String::new());
            }
            other => return Err(format!("Unknown flag '{other}'.")),
        }
        i += 1;
    }

    let mut request = if let Some(path) = input_path {
        let raw = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read request file '{}': {e}", path.display()))?;
        serde_json::from_str::<HeadlessCliRequest>(&raw)
            .map_err(|e| format!("Failed to parse request file '{}': {e}", path.display()))?
    } else {
        HeadlessCliRequest::default()
    };

    if let Some(value) = overrides.prompt {
        request.prompt = value;
    }
    if let Some(value) = overrides.workspace {
        request.workspace = Some(value);
    }
    if let Some(value) = overrides.mode {
        request.mode = value;
    }
    if let Some(value) = overrides.session_id {
        request.session_id = Some(value);
    }
    if let Some(value) = overrides.session_title {
        request.session_title = Some(value);
    }
    if let Some(value) = overrides.channel {
        request.channel = Some(value);
    }
    if let Some(value) = overrides.config_dir {
        request.config_dir = Some(value);
    }
    if let Some(value) = overrides.pool_id {
        request.pool_id = Some(value);
    }
    if let Some(value) = overrides.pool_name {
        request.pool_name = Some(value);
    }
    if let Some(value) = overrides.pool_size {
        request.pool_size = Some(value);
    }
    if let Some(value) = overrides.koi_ids {
        request.koi_ids = value;
    }
    if let Some(value) = overrides.task_timeout_secs {
        request.task_timeout_secs = Some(value);
    }
    if overrides.wait_for_completion {
        request.wait_for_completion = true;
    }
    if let Some(value) = overrides.wait_timeout_secs {
        request.wait_timeout_secs = Some(value);
    }
    if let Some(value) = overrides.extra_system_context {
        request.extra_system_context = Some(value);
    }
    if let Some(value) = overrides.output {
        request.output = Some(value);
    }

    if request.prompt.trim().is_empty() {
        return Err("Missing prompt. Use --prompt <text> or provide it via --input.".to_string());
    }

    Ok(request)
}

/// Parse `openpisci-headless capabilities <...>` into a mode selector.
pub fn parse_capabilities_mode(args: &[String]) -> Result<HeadlessCliMode, String> {
    let mut mode = HeadlessCliMode::Pisci;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--mode" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| "Missing value for '--mode'.".to_string())?;
                mode = parse_mode(raw)?;
            }
            "--help" | "-h" => {
                print_usage();
                return Err(String::new());
            }
            other => return Err(format!("Unknown flag '{other}'.")),
        }
        i += 1;
    }
    Ok(mode)
}

pub fn write_response(output: Option<&str>, response: &HeadlessCliResponse) -> Result<(), String> {
    let json =
        serde_json::to_string_pretty(response).map_err(|e| format!("Serialize failed: {e}"))?;
    if let Some(path) = output.map(str::trim).filter(|s| !s.is_empty()) {
        let path = Path::new(path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create '{}': {e}", parent.display()))?;
            }
        }
        fs::write(path, format!("{json}\n"))
            .map_err(|e| format!("Failed to write '{}': {e}", path.display()))?;
    } else {
        println!("{json}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parse_mode_round_trip() {
        assert_eq!(parse_mode("pisci").unwrap(), HeadlessCliMode::Pisci);
        assert_eq!(parse_mode(" POOL ").unwrap(), HeadlessCliMode::Pool);
        assert!(parse_mode("bogus").is_err());
    }

    #[test]
    fn parse_run_prompt_and_defaults() {
        let args = owned(&["--prompt", "Hello world", "--workspace", "C:\\tmp"]);
        let req = parse_run_request(&args).expect("parse ok");
        assert_eq!(req.prompt, "Hello world");
        assert_eq!(req.workspace.as_deref(), Some("C:\\tmp"));
        assert_eq!(req.mode, HeadlessCliMode::Pisci);
    }

    #[test]
    fn parse_run_rejects_missing_prompt() {
        let err = parse_run_request(&owned(&[])).unwrap_err();
        assert!(err.contains("Missing prompt"), "got: {err}");
    }

    #[test]
    fn parse_run_honors_koi_list_and_timeout() {
        let args = owned(&[
            "--prompt",
            "x",
            "--koi-ids",
            "a,b,,c",
            "--task-timeout-secs",
            "42",
            "--wait-for-completion",
        ]);
        let req = parse_run_request(&args).unwrap();
        assert_eq!(req.koi_ids, vec!["a", "b", "c"]);
        assert_eq!(req.task_timeout_secs, Some(42));
        assert!(req.wait_for_completion);
    }

    #[test]
    fn parse_capabilities_defaults_to_pisci() {
        let mode = parse_capabilities_mode(&[]).unwrap();
        assert_eq!(mode, HeadlessCliMode::Pisci);
    }

    #[test]
    fn parse_capabilities_mode_pool() {
        let mode = parse_capabilities_mode(&owned(&["--mode", "pool"])).unwrap();
        assert_eq!(mode, HeadlessCliMode::Pool);
    }
}
