use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_OUTPUT_BYTES: usize = 100 * 1024; // 100 KB

pub struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str { "shell" }

    fn description(&self) -> &str {
        "Execute a shell command. On Windows, commands run via PowerShell. \
         Returns stdout, stderr, and exit code. \
         Working directory defaults to the workspace root."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute"
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory (defaults to workspace root)"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default 120)"
                }
            },
            "required": ["command"]
        })
    }

    fn needs_confirmation(&self, _input: &Value) -> bool { true }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let command = match input["command"].as_str() {
            Some(c) => c,
            None => return Ok(ToolResult::err("Missing required parameter: command")),
        };

        let cwd = if let Some(cwd_str) = input["cwd"].as_str() {
            let p = if std::path::Path::new(cwd_str).is_absolute() {
                std::path::PathBuf::from(cwd_str)
            } else {
                ctx.workspace_root.join(cwd_str)
            };
            p
        } else {
            ctx.workspace_root.clone()
        };

        // Ensure cwd exists
        if !cwd.exists() {
            std::fs::create_dir_all(&cwd)?;
        }

        let timeout_secs = input["timeout"].as_u64().unwrap_or(DEFAULT_TIMEOUT_SECS);

        // Force UTF-8 output so Chinese/CJK filenames and content are not garbled.
        #[cfg(target_os = "windows")]
        let utf8_command = format!(
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
             $OutputEncoding = [System.Text.Encoding]::UTF8; \
             chcp 65001 | Out-Null; \
             {}",
            command
        );

        // Build command
        #[cfg(target_os = "windows")]
        let mut cmd = {
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-NonInteractive", "-Command", &utf8_command]);
            c
        };

        #[cfg(not(target_os = "windows"))]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.args(["-c", command]);
            c
        };

        cmd.current_dir(&cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let result = timeout(
            Duration::from_secs(timeout_secs),
            cmd.output(),
        ).await;

        match result {
            Err(_) => Ok(ToolResult::err(format!(
                "Command timed out after {} seconds: {}",
                timeout_secs, command
            ))),
            Ok(Err(e)) => Ok(ToolResult::err(format!("Failed to execute command: {}", e))),
            Ok(Ok(output)) => {
                let stdout = truncate_output(
                    &String::from_utf8_lossy(&output.stdout),
                    MAX_OUTPUT_BYTES / 2,
                );
                let stderr = truncate_output(
                    &String::from_utf8_lossy(&output.stderr),
                    MAX_OUTPUT_BYTES / 2,
                );
                let exit_code = output.status.code().unwrap_or(-1);

                let mut result_text = format!("Exit code: {}\n", exit_code);
                if !stdout.is_empty() {
                    result_text.push_str(&format!("\nSTDOUT:\n{}", stdout));
                }
                if !stderr.is_empty() {
                    result_text.push_str(&format!("\nSTDERR:\n{}", stderr));
                }

                let is_error = exit_code != 0;
                if is_error {
                    Ok(ToolResult::err(result_text))
                } else {
                    Ok(ToolResult::ok(result_text))
                }
            }
        }
    }
}

fn truncate_output(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let half = max_bytes / 2;
    let start = &s[..half];
    let end = &s[s.len() - half..];
    format!("{}\n\n... [{} bytes truncated] ...\n\n{}", start, s.len() - max_bytes, end)
}
