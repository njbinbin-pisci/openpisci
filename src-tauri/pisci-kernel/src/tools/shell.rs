use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

use super::elevate;

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_OUTPUT_BYTES: usize = 200 * 1024; // 200 KB

pub struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        #[cfg(target_os = "windows")]
        {
            return "Execute a shell command on Windows. By default uses 64-bit PowerShell. \
         Use `interpreter: \"powershell32\"` for 32-bit PowerShell (required for legacy COM/ActiveX components). \
         Use `interpreter: \"cmd\"` for cmd.exe (useful for dir, reg, findstr, etc.). \
         Use `elevated: true` to run with administrator privileges — Windows will show a UAC consent dialog \
         for the user to approve. IMPORTANT: If a command fails with 'Access Denied' or permission error, \
         ALWAYS retry with elevated=true. The system will auto-detect permission errors and prompt UAC, \
         but you can also set elevated=true proactively for operations that clearly need admin rights. \
         You are CAPABLE of running admin commands — never skip a task just because it needs elevation. \
         The working directory defaults to C:\\ (not the workspace root) so absolute paths always work. \
         Always returns exit code + stdout + stderr so you can judge success yourself. \
         Tips: \
         - To find files: `cmd /c dir C:\\SomeDir /s /b` \
         - To query registry: `reg query HKLM\\SOFTWARE\\Classes /f keyword /s` \
         - To check 32-bit COM: use powershell32 and New-Object -ComObject ProgID \
         - To list C:\\ root dirs: `cmd /c dir C:\\ /ad /b` \
         - Needs admin (e.g. install software, write to Program Files, modify system registry, regsvr32): use elevated=true";
        }

        #[cfg(target_os = "macos")]
        {
            return "Execute a shell command on macOS. Uses `/bin/sh -c` by default. \
         Use `elevated: true` to trigger the native administrator password dialog via AppleScript. \
         If a command fails with 'Operation not permitted' or 'Permission denied', retry with `elevated: true`. \
         Working directory defaults to `/`. Always returns exit code + stdout + stderr so you can judge success yourself.";
        }

        #[cfg(target_os = "linux")]
        {
            return "Execute a shell command on Linux. Uses `/bin/sh -c` by default. \
         Use `elevated: true` to retry through polkit (`pkexec`) when a command needs root privileges. \
         If a command fails with 'Permission denied' or 'Operation not permitted', retry with `elevated: true`. \
         This requires a desktop polkit agent; otherwise rerun manually with sudo in a terminal. \
         Working directory defaults to `/`. Always returns exit code + stdout + stderr so you can judge success yourself.";
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            "Execute a shell command on the host. Uses `/bin/sh -c` by default. Working directory defaults to `/`. Always returns exit code + stdout + stderr."
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute"
                },
                "interpreter": {
                    "type": "string",
                    "enum": ["powershell", "powershell32", "cmd"],
                    "description": "Interpreter to use. 'powershell' = 64-bit PS (default). 'powershell32' = 32-bit PS (use for legacy COM/ActiveX). 'cmd' = cmd.exe (use for dir/reg/findstr/where)."
                },
                "elevated": {
                    "type": "boolean",
                    "description": "Run with administrator privileges. Windows will show a UAC consent dialog. Use when you get 'Access Denied' or need to modify system files/registry."
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory. Defaults to C:\\ so absolute paths always resolve correctly."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default 120). For elevated commands, includes UAC dialog wait time — set higher if user may need time to respond."
                },
                "env": {
                    "type": "object",
                    "description": "Extra environment variables to set (key-value pairs)"
                }
            },
            "required": ["command"]
        })
    }

    fn description_minimal(&self) -> Cow<'_, str> {
        #[cfg(target_os = "windows")]
        return Cow::Borrowed(
            "Execute a Windows shell command. Defaults to 64-bit PowerShell; set \
             interpreter to powershell32 or cmd when needed. Working directory defaults \
             to C:\\ — use absolute paths. Set elevated=true to run as Administrator \
             (UAC prompt). Always retry with elevated=true on Access Denied.",
        );

        #[cfg(target_os = "macos")]
        return Cow::Borrowed(
            "Execute a macOS shell command via /bin/sh. Working directory defaults to /. \
             Set elevated=true to show the native administrator password dialog.",
        );

        #[cfg(target_os = "linux")]
        return Cow::Borrowed(
            "Execute a Linux shell command via /bin/sh. Working directory defaults to /. \
             Set elevated=true to retry via pkexec/polkit when root privileges are required.",
        );

        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        Cow::Borrowed("Execute a shell command on the host. Working directory defaults to /.")
    }

    fn input_schema_minimal(&self) -> Value {
        // Hand-tuned: keep enum and `required` exactly; drop verbose
        // per-property prose. This is what the model sees on every
        // iteration, so every token counts.
        json!({
            "type": "object",
            "properties": {
                "command":     { "type": "string" },
                "interpreter": { "type": "string", "enum": ["powershell", "powershell32", "cmd"] },
                "elevated":    { "type": "boolean" },
                "cwd":         { "type": "string" },
                "timeout":     { "type": "integer", "minimum": 1 },
                "env":         { "type": "object" }
            },
            "required": ["command"]
        })
    }

    fn needs_confirmation(&self, _input: &Value) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let command = match input["command"].as_str() {
            Some(c) => c,
            None => return Ok(ToolResult::err("Missing required parameter: command")),
        };

        // Default cwd to C:\ on Windows so absolute paths always work.
        // Workspace root is often a project subdirectory that has nothing to do with the task.
        let cwd = if let Some(cwd_str) = input["cwd"].as_str() {
            if std::path::Path::new(cwd_str).is_absolute() {
                std::path::PathBuf::from(cwd_str)
            } else {
                ctx.workspace_root.join(cwd_str)
            }
        } else {
            #[cfg(target_os = "windows")]
            {
                std::path::PathBuf::from("C:\\")
            }
            #[cfg(not(target_os = "windows"))]
            {
                std::path::PathBuf::from("/")
            }
        };

        if !cwd.exists() {
            let _ = std::fs::create_dir_all(&cwd);
        }

        let timeout_secs = input["timeout"].as_u64().unwrap_or(DEFAULT_TIMEOUT_SECS);
        let elevated = input["elevated"].as_bool().unwrap_or(false);
        #[cfg(target_os = "windows")]
        let interpreter = input["interpreter"].as_str().unwrap_or("powershell");

        let env_pairs = input["env"]
            .as_object()
            .map(|env_obj| {
                env_obj
                    .iter()
                    .filter_map(|(key, value)| value.as_str().map(|val| (key.clone(), val.to_string())))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if elevated {
            #[cfg(target_os = "windows")]
            {
                let arch = match interpreter {
                    "powershell32" => "x86",
                    _ => "x64",
                };
                let elev_timeout = input["timeout"].as_u64().unwrap_or(180);
                return render_elevated_result(
                    elevate::run_elevated_powershell(command, arch, elev_timeout).await,
                    "Administrator",
                );
            }

            #[cfg(not(target_os = "windows"))]
            {
                let elev_timeout = input["timeout"].as_u64().unwrap_or(180);
                return render_elevated_result(
                    elevate::run_elevated_shell(command, &cwd, &env_pairs, elev_timeout).await,
                    elevated_label(),
                );
            }
        }

        #[cfg(target_os = "windows")]
        let mut cmd = build_windows_cmd(interpreter, command);

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

        // Apply extra env vars
        for (key, value) in &env_pairs {
            cmd.env(key, value);
        }

        let run_result = timeout(Duration::from_secs(timeout_secs), cmd.output()).await;

        match run_result {
            Err(_) => Ok(ToolResult::err(format!(
                "Command timed out after {}s. Consider breaking it into smaller steps or increasing timeout.",
                timeout_secs
            ))),
            Ok(Err(e)) => Ok(ToolResult::err(format!("Failed to spawn process: {}", e))),
            Ok(Ok(output)) => {
                let stdout = truncate_output(&String::from_utf8_lossy(&output.stdout), MAX_OUTPUT_BYTES * 3 / 4);
                let stderr = truncate_output(&String::from_utf8_lossy(&output.stderr), MAX_OUTPUT_BYTES / 4);
                let exit_code = output.status.code().unwrap_or(-1);

                // Auto-elevate: if the command failed with a permission error and
                // elevated was not already requested, retry automatically with UAC.
                if !elevated && is_permission_error(exit_code, &stdout, &stderr) {
                    tracing::info!(
                        "shell: permission error detected (exit={}), auto-retrying with elevation",
                        exit_code
                    );

                    #[cfg(target_os = "windows")]
                    let retry = {
                        let arch = match interpreter {
                            "powershell32" => "x86",
                            _ => "x64",
                        };
                        elevate::run_elevated_powershell(
                            command,
                            arch,
                            input["timeout"].as_u64().unwrap_or(180),
                        )
                        .await
                    };

                    #[cfg(not(target_os = "windows"))]
                    let retry = elevate::run_elevated_shell(
                        command,
                        &cwd,
                        &env_pairs,
                        input["timeout"].as_u64().unwrap_or(180),
                    )
                    .await;

                    return match retry {
                        Ok(r) => Ok(render_elevated_output(
                            &r,
                            &format!(
                                "auto-elevated to {} after permission error",
                                elevated_label()
                            ),
                        )),
                        Err(e) => {
                            let mut parts = vec![format!("Exit code: {}", exit_code)];
                            if !stdout.is_empty() {
                                parts.push(format!("STDOUT:\n{}", stdout));
                            }
                            if !stderr.is_empty() {
                                parts.push(format!("STDERR:\n{}", stderr));
                            }
                            parts.push(format!(
                                "\n⚠️ Auto-elevation attempted but failed ({}). To retry manually, use `elevated: true` in your next shell call.",
                                e
                            ));
                            Ok(ToolResult::ok(parts.join("\n\n")))
                        }
                    };
                }

                // Build a clear, structured result the LLM can parse
                let mut parts = vec![format!("Exit code: {}", exit_code)];
                if !stdout.is_empty() {
                    parts.push(format!("STDOUT:\n{}", stdout));
                }
                if !stderr.is_empty() {
                    parts.push(format!("STDERR:\n{}", stderr));
                }
                if stdout.is_empty() && stderr.is_empty() {
                    parts.push("(no output)".to_string());
                }

                // Always ok — let the LLM read exit code and decide
                Ok(ToolResult::ok(parts.join("\n\n")))
            }
        }
    }
}

fn render_elevated_result(
    result: Result<elevate::ElevatedResult, anyhow::Error>,
    label: &str,
) -> Result<ToolResult> {
    match result {
        Ok(r) => Ok(render_elevated_output(&r, &format!("ran as {}", label))),
        Err(e) => Ok(ToolResult::err(format!("Elevated execution failed: {}", e))),
    }
}

fn render_elevated_output(r: &elevate::ElevatedResult, label: &str) -> ToolResult {
    let mut parts = vec![format!("Exit code: {} ({})", r.exit_code, label)];
    if !r.stdout.is_empty() {
        parts.push(format!(
            "STDOUT:\n{}",
            truncate_output(&r.stdout, MAX_OUTPUT_BYTES * 3 / 4)
        ));
    }
    if !r.stderr.is_empty() {
        parts.push(format!(
            "STDERR:\n{}",
            truncate_output(&r.stderr, MAX_OUTPUT_BYTES / 4)
        ));
    }
    if r.stdout.is_empty() && r.stderr.is_empty() {
        parts.push("(no output)".to_string());
    }
    ToolResult::ok(parts.join("\n\n"))
}

fn elevated_label() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Administrator"
    }
    #[cfg(target_os = "macos")]
    {
        "administrator privileges"
    }
    #[cfg(target_os = "linux")]
    {
        "root via polkit"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        "elevated privileges"
    }
}

#[cfg(target_os = "windows")]
fn build_windows_cmd(interpreter: &str, command: &str) -> Command {
    // UTF-8 preamble for PowerShell to avoid garbled CJK output
    let utf8_preamble = "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;\
                         $OutputEncoding=[System.Text.Encoding]::UTF8;\
                         chcp 65001 | Out-Null; ";

    // CREATE_NO_WINDOW: prevents a blue console window from flashing on screen
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    match interpreter {
        "powershell32" => {
            // 32-bit PowerShell — required for legacy COM/ActiveX (WOW6432Node) components
            let ps32 = r"C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe";
            let full_cmd = format!("{}{}", utf8_preamble, command);
            let mut c = Command::new(ps32);
            c.args(["-NoProfile", "-NonInteractive", "-Command", &full_cmd])
                .creation_flags(CREATE_NO_WINDOW);
            c
        }
        "cmd" => {
            // cmd.exe — best for dir, reg, findstr, where, assoc, ftype, etc.
            // Wrap in chcp 65001 for UTF-8
            let full_cmd = format!("chcp 65001 >nul 2>&1 & {}", command);
            let mut c = Command::new("cmd");
            c.args(["/C", &full_cmd]).creation_flags(CREATE_NO_WINDOW);
            c
        }
        _ => {
            // Default: 64-bit PowerShell
            let full_cmd = format!("{}{}", utf8_preamble, command);
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-NonInteractive", "-Command", &full_cmd])
                .creation_flags(CREATE_NO_WINDOW);
            c
        }
    }
}

/// Detect whether a command failed due to insufficient privileges.
/// Checks common Windows permission error patterns in exit code, stdout, and stderr.
fn is_permission_error(exit_code: i32, stdout: &str, stderr: &str) -> bool {
    // Non-zero exit code required — don't auto-elevate successful commands
    if exit_code == 0 {
        return false;
    }
    let combined = format!("{} {}", stdout, stderr).to_lowercase();
    #[cfg(target_os = "windows")]
    {
    // Common Windows permission error strings
        return combined.contains("access is denied")
            || combined.contains("access denied")
            || combined.contains("拒绝访问")
            || combined.contains("requires elevation")
            || combined.contains("elevated")
            || combined.contains("administrator")
            || combined.contains("privileged")
            || combined.contains("0x80070005")
            || combined.contains("error 5")
            || combined.contains("error: 5,")
            || (exit_code == 1 && combined.contains("regsvr32"))
            || combined.contains("cannot be loaded because running scripts is disabled")
            || combined.contains("unauthorizedaccessexception");
    }

    #[cfg(not(target_os = "windows"))]
    {
        combined.contains("permission denied")
            || combined.contains("operation not permitted")
            || combined.contains("not permitted")
            || combined.contains("must be root")
            || combined.contains("authentication is required")
            || combined.contains("polkit")
            || exit_code == 126
    }
}

fn truncate_output(s: &str, max_bytes: usize) -> String {
    let s = s.trim();
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let half = max_bytes / 2;
    let start = &s[..half];
    let end = &s[s.len() - half..];
    format!(
        "{}\n\n... [{} bytes truncated] ...\n\n{}",
        start,
        s.len() - max_bytes,
        end
    )
}
