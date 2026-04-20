use crate::tools::{self, RuntimeToolProfile};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HeadlessCliMode {
    #[default]
    Pisci,
    Pool,
}

impl HeadlessCliMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pisci => "pisci",
            Self::Pool => "pool",
        }
    }

    pub fn tool_profile(self) -> RuntimeToolProfile {
        match self {
            Self::Pisci => RuntimeToolProfile::HeadlessPisci,
            Self::Pool => RuntimeToolProfile::HeadlessPool,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HeadlessCliRequest {
    pub prompt: String,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub mode: HeadlessCliMode,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub session_title: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub config_dir: Option<String>,
    #[serde(default)]
    pub pool_id: Option<String>,
    #[serde(default)]
    pub pool_name: Option<String>,
    #[serde(default)]
    pub pool_size: Option<u32>,
    #[serde(default)]
    pub koi_ids: Vec<String>,
    #[serde(default)]
    pub task_timeout_secs: Option<u32>,
    #[serde(default)]
    pub wait_for_completion: bool,
    #[serde(default)]
    pub wait_timeout_secs: Option<u64>,
    #[serde(default)]
    pub extra_system_context: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
}

impl HeadlessCliRequest {
    pub fn app_data_dir_override(&self) -> Option<PathBuf> {
        self.config_dir
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisabledToolInfo {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PoolWaitSummary {
    pub completed: bool,
    pub timed_out: bool,
    pub active_todos: u32,
    pub done_todos: u32,
    pub cancelled_todos: u32,
    pub blocked_todos: u32,
    pub latest_messages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadlessCliResponse {
    pub ok: bool,
    pub mode: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_id: Option<String>,
    pub response_text: String,
    pub disabled_tools: Vec<DisabledToolInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_wait: Option<PoolWaitSummary>,
}

#[derive(Debug, Clone, Serialize)]
struct CapabilityReport {
    os: &'static str,
    mode: &'static str,
    disabled_tools: Vec<DisabledToolInfo>,
}

#[derive(Default)]
struct RunArgOverrides {
    prompt: Option<String>,
    workspace: Option<String>,
    mode: Option<HeadlessCliMode>,
    session_id: Option<String>,
    session_title: Option<String>,
    channel: Option<String>,
    config_dir: Option<String>,
    pool_id: Option<String>,
    pool_name: Option<String>,
    pool_size: Option<u32>,
    koi_ids: Option<Vec<String>>,
    task_timeout_secs: Option<u32>,
    wait_for_completion: bool,
    wait_timeout_secs: Option<u64>,
    extra_system_context: Option<String>,
    output: Option<String>,
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

fn disabled_tools_for_mode(mode: HeadlessCliMode) -> Vec<DisabledToolInfo> {
    tools::runtime_disabled_tools(mode.tool_profile())
        .into_iter()
        .map(|tool| DisabledToolInfo {
            name: tool.name.to_string(),
            reason: tool.reason.unwrap_or("Disabled by runtime profile.").to_string(),
        })
        .collect()
}

fn print_usage() {
    eprintln!(
        "Usage:\n  openpisci run --prompt <text> [--workspace <dir>] [--mode pisci|pool] [--output <file>]\n  openpisci run --input <request.json> [--output <result.json>]\n  openpisci capabilities [--mode pisci|pool]"
    );
}

fn parse_mode(raw: &str) -> Result<HeadlessCliMode, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pisci" => Ok(HeadlessCliMode::Pisci),
        "pool" => Ok(HeadlessCliMode::Pool),
        other => Err(format!("Unsupported mode '{}'. Use 'pisci' or 'pool'.", other)),
    }
}

fn next_value(args: &[String], idx: &mut usize, flag: &str) -> Result<String, String> {
    *idx += 1;
    args.get(*idx)
        .cloned()
        .ok_or_else(|| format!("Missing value for '{}'.", flag))
}

fn parse_run_request(args: &[String]) -> Result<HeadlessCliRequest, String> {
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
                        .map_err(|_| format!("Invalid --pool-size '{}'.", raw))?,
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
                        .map_err(|_| format!("Invalid --task-timeout-secs '{}'.", raw))?,
                );
            }
            "--wait-for-completion" => overrides.wait_for_completion = true,
            "--wait-timeout-secs" => {
                let raw = next_value(args, &mut i, "--wait-timeout-secs")?;
                overrides.wait_timeout_secs = Some(
                    raw.parse::<u64>()
                        .map_err(|_| format!("Invalid --wait-timeout-secs '{}'.", raw))?,
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
            other => return Err(format!("Unknown flag '{}'.", other)),
        }
        i += 1;
    }

    let mut request = if let Some(path) = input_path {
        let raw = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read request file '{}': {}", path.display(), e))?;
        serde_json::from_str::<HeadlessCliRequest>(&raw)
            .map_err(|e| format!("Failed to parse request file '{}': {}", path.display(), e))?
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

fn write_response(output: Option<&str>, response: &HeadlessCliResponse) -> Result<(), String> {
    let json =
        serde_json::to_string_pretty(response).map_err(|e| format!("Serialize failed: {}", e))?;
    if let Some(path) = output.map(str::trim).filter(|s| !s.is_empty()) {
        let path = Path::new(path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create '{}': {}", parent.display(), e))?;
            }
        }
        fs::write(path, format!("{}\n", json))
            .map_err(|e| format!("Failed to write '{}': {}", path.display(), e))?;
    } else {
        println!("{}", json);
    }
    Ok(())
}

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
            let response = crate::desktop_app::run_headless_cli(request)?;
            write_response(output.as_deref(), &response)
        }
        "capabilities" => {
            let mut mode = HeadlessCliMode::Pisci;
            let mut i = 1usize;
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
                        return Ok(());
                    }
                    other => return Err(format!("Unknown flag '{}'.", other)),
                }
                i += 1;
            }
            let report = CapabilityReport {
                os: current_os(),
                mode: mode.as_str(),
                disabled_tools: disabled_tools_for_mode(mode),
            };
            let json = serde_json::to_string_pretty(&report)
                .map_err(|e| format!("Serialize failed: {}", e))?;
            println!("{}", json);
            Ok(())
        }
        "--help" | "-h" | "help" => {
            print_usage();
            Ok(())
        }
        other => Err(format!("Unknown subcommand '{}'.", other)),
    }
}
