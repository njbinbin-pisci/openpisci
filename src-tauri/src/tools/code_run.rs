use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::time::timeout;

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 300;
/// Keep head + tail of each stream; long build logs are rarely useful in full.
const MAX_STREAM_BYTES: usize = 8 * 1024; // 8 KB per stream

pub struct CodeRunTool;

#[async_trait]
impl Tool for CodeRunTool {
    fn name(&self) -> &str { "code_run" }

    fn description(&self) -> &str {
        "Run a shell command in a project directory and capture its output. \
         Designed for coding tasks: building, testing, linting, running scripts. \
         Returns exit_code, stdout, stderr, and duration_ms. \
         Use `cwd` to set the project root (e.g. where Cargo.toml / package.json lives). \
         Examples: \
         - Build Rust: command=\"cargo build\", cwd=\"C:\\\\myproject\" \
         - Run tests:  command=\"cargo test\", cwd=\"C:\\\\myproject\" \
         - Python:     command=\"python main.py\", cwd=\"C:\\\\myproject\" \
         - Node:       command=\"npm run build\", cwd=\"C:\\\\myproject\" \
         - Lint:       command=\"cargo clippy -- -D warnings\", cwd=\"C:\\\\myproject\" \
         Prefer this over `shell` for code execution — output is structured and \
         long outputs are automatically trimmed to keep context window usage low."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Command to run (e.g. \"cargo test\", \"python main.py\", \"npm run build\")"
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory — should be the project root. Defaults to workspace_root."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout in seconds (default 60, max 300). Increase for slow builds."
                },
                "env": {
                    "type": "object",
                    "description": "Extra environment variables (key-value string pairs)"
                }
            },
            "required": ["command"]
        })
    }

    fn needs_confirmation(&self, _input: &Value) -> bool { true }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let command = match input["command"].as_str() {
            Some(c) if !c.trim().is_empty() => c,
            _ => return Ok(ToolResult::err("Missing required parameter: command")),
        };

        let cwd = if let Some(cwd_str) = input["cwd"].as_str() {
            if std::path::Path::new(cwd_str).is_absolute() {
                std::path::PathBuf::from(cwd_str)
            } else {
                ctx.workspace_root.join(cwd_str)
            }
        } else {
            ctx.workspace_root.clone()
        };

        if !cwd.exists() {
            return Ok(ToolResult::err(format!(
                "Working directory does not exist: {}",
                cwd.display()
            )));
        }

        let timeout_secs = input["timeout_secs"]
            .as_u64()
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS);

        let mut cmd = build_cmd(command);
        cmd.current_dir(&cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        if let Some(env_obj) = input["env"].as_object() {
            for (k, v) in env_obj {
                if let Some(val) = v.as_str() {
                    cmd.env(k, val);
                }
            }
        }

        let start = Instant::now();
        let run_result = timeout(Duration::from_secs(timeout_secs), cmd.output()).await;
        let duration_ms = start.elapsed().as_millis() as u64;

        match run_result {
            Err(_) => {
                Ok(ToolResult::err(format!(
                    "Command timed out after {}s.\n\
                     Consider: breaking into smaller steps, increasing timeout_secs, \
                     or checking if the process is hanging waiting for input.",
                    timeout_secs
                )))
            }
            Ok(Err(e)) => Ok(ToolResult::err(format!("Failed to spawn process: {}", e))),
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);
                let stdout_raw = String::from_utf8_lossy(&output.stdout);
                let stderr_raw = String::from_utf8_lossy(&output.stderr);

                let stdout = trim_stream(&stdout_raw, MAX_STREAM_BYTES);
                let stderr = trim_stream(&stderr_raw, MAX_STREAM_BYTES);

                let status_label = if exit_code == 0 { "SUCCESS" } else { "FAILED" };

                let mut parts = vec![
                    format!("exit_code: {} ({})  duration: {}ms  cwd: {}",
                        exit_code, status_label, duration_ms, cwd.display()),
                ];

                if !stdout.is_empty() {
                    parts.push(format!("--- stdout ---\n{}", stdout));
                }
                if !stderr.is_empty() {
                    parts.push(format!("--- stderr ---\n{}", stderr));
                }
                if stdout.is_empty() && stderr.is_empty() {
                    parts.push("(no output)".to_string());
                }

                // Annotate common failure patterns to help the LLM triage faster
                if exit_code != 0 {
                    let combined = format!("{}{}", stdout_raw, stderr_raw);
                    if let Some(hint) = diagnose(&combined, command) {
                        parts.push(format!("--- diagnosis ---\n{}", hint));
                    }
                }

                Ok(ToolResult::ok(parts.join("\n\n")))
            }
        }
    }
}

/// Trim a stream to at most `max_bytes`, keeping head and tail.
fn trim_stream(s: &str, max_bytes: usize) -> String {
    let s = s.trim();
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let head_bytes = max_bytes * 3 / 4;
    let tail_bytes = max_bytes - head_bytes;
    // Find valid char boundaries
    let head_end = s.char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= head_bytes)
        .last()
        .unwrap_or(0);
    let tail_start = s.char_indices()
        .rev()
        .map(|(i, _)| i)
        .take_while(|&i| s.len() - i <= tail_bytes)
        .last()
        .unwrap_or(s.len());
    format!(
        "{}\n\n... [{} bytes trimmed — use file_read or scroll output for full log] ...\n\n{}",
        &s[..head_end],
        s.len() - max_bytes,
        &s[tail_start..]
    )
}

/// Heuristic diagnosis for common build/test failure patterns.
fn diagnose(output: &str, command: &str) -> Option<String> {
    let out_lower = output.to_lowercase();
    let cmd_lower = command.to_lowercase();

    // Rust / Cargo
    if cmd_lower.contains("cargo") {
        if out_lower.contains("error[e") {
            let error_count = output.matches("error[E").count() + output.matches("error[e").count();
            return Some(format!(
                "Rust compilation failed with ~{} error(s). \
                 Fix each `error[Exxxx]` in order — later errors often cascade from earlier ones.",
                error_count
            ));
        }
        if out_lower.contains("test failed") || out_lower.contains("failures:") {
            let failed = output.lines()
                .filter(|l| l.trim_start().starts_with("FAILED"))
                .count();
            return Some(format!(
                "{} test(s) failed. Check the `failures:` section above for panic messages and expected vs actual values.",
                failed.max(1)
            ));
        }
        if out_lower.contains("warning:") && !out_lower.contains("error") {
            return Some("Build succeeded with warnings. Run `cargo clippy` for detailed lint suggestions.".to_string());
        }
    }

    // Python
    if cmd_lower.contains("python") || cmd_lower.contains("pytest") || cmd_lower.contains("pip") {
        if out_lower.contains("syntaxerror") {
            return Some("Python SyntaxError detected. Check the file and line number shown above.".to_string());
        }
        if out_lower.contains("modulenotfounderror") || out_lower.contains("importerror") {
            return Some("Missing Python module. Run `pip install <module>` or check your virtual environment is activated.".to_string());
        }
        if out_lower.contains("failed") && cmd_lower.contains("pytest") {
            return Some("pytest failures detected. Check the FAILED lines and assertion errors above.".to_string());
        }
    }

    // Node / npm
    if cmd_lower.contains("npm") || cmd_lower.contains("node") || cmd_lower.contains("yarn") {
        if out_lower.contains("err!") || out_lower.contains("npm error") {
            return Some("npm error detected. Check for missing dependencies (`npm install`) or script errors above.".to_string());
        }
        if out_lower.contains("typeerror") || out_lower.contains("syntaxerror") {
            return Some("JavaScript/TypeScript error detected. Check the file and line shown above.".to_string());
        }
    }

    // Generic
    if out_lower.contains("permission denied") || out_lower.contains("access is denied") {
        return Some("Permission denied. Try running with elevated privileges or check file ownership.".to_string());
    }
    if out_lower.contains("command not found") || out_lower.contains("is not recognized") {
        return Some("Command not found. Ensure the tool is installed and on PATH, or use an absolute path.".to_string());
    }

    None
}

#[cfg(target_os = "windows")]
fn build_cmd(command: &str) -> Command {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // Use cmd.exe so PATH-based tools (cargo, python, npm, git) resolve correctly
    // without needing PowerShell profile overhead.
    let full = format!("chcp 65001 >nul 2>&1 & {}", command);
    let mut c = Command::new("cmd");
    c.args(["/C", &full])
     .creation_flags(CREATE_NO_WINDOW);
    c
}

#[cfg(not(target_os = "windows"))]
fn build_cmd(command: &str) -> Command {
    let mut c = Command::new("sh");
    c.args(["-c", command]);
    c
}
