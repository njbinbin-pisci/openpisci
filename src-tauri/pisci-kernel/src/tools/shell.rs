use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

#[cfg(target_os = "windows")]
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
        "Execute a shell command on Windows. By default uses 64-bit PowerShell. \
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
         - Needs admin (e.g. install software, write to Program Files, modify system registry, regsvr32): use elevated=true"
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
        // Terse: keep the critical operational tips (cwd=C:\, auto-elevate
        // on access-denied) so minimal-mode agents still call it right.
        // Long prose (per-interpreter recipes, tips list) only appears
        // via `description()` on schema-correction / recall.
        Cow::Borrowed(
            "Execute a Windows shell command. Defaults to 64-bit PowerShell; set \
             interpreter to powershell32 or cmd when needed. Working directory defaults \
             to C:\\ — use absolute paths. Set elevated=true to run as Administrator \
             (UAC prompt). Always retry with elevated=true on Access Denied.",
        )
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
        #[cfg(target_os = "windows")]
        let interpreter = input["interpreter"].as_str().unwrap_or("powershell");
        #[cfg(target_os = "windows")]
        let elevated = input["elevated"].as_bool().unwrap_or(false);

        // Elevated path: use UAC ShellExecute runas + temp file bridge
        #[cfg(target_os = "windows")]
        if elevated {
            let arch = match interpreter {
                "powershell32" => "x86",
                _ => "x64",
            };
            // For elevated, timeout includes UAC dialog wait — use a longer default
            let elev_timeout = input["timeout"].as_u64().unwrap_or(180);
            return match elevate::run_elevated_powershell(command, arch, elev_timeout).await {
                Ok(r) => {
                    let mut parts =
                        vec![format!("Exit code: {} (ran as Administrator)", r.exit_code)];
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
                    Ok(ToolResult::ok(parts.join("\n\n")))
                }
                Err(e) => Ok(ToolResult::err(format!("Elevated execution failed: {}", e))),
            };
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
        if let Some(env_obj) = input["env"].as_object() {
            for (k, v) in env_obj {
                if let Some(val) = v.as_str() {
                    cmd.env(k, val);
                }
            }
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
                #[cfg(target_os = "windows")]
                if !elevated && is_permission_error(exit_code, &stdout, &stderr) {
                    tracing::info!(
                        "shell: permission error detected (exit={}), auto-retrying with elevation",
                        exit_code
                    );
                    let arch = match interpreter {
                        "powershell32" => "x86",
                        _ => "x64",
                    };
                    // Use a longer timeout to give the user time to respond to UAC
                    let elev_timeout = input["timeout"].as_u64().unwrap_or(180);
                    return match elevate::run_elevated_powershell(command, arch, elev_timeout).await {
                        Ok(r) => {
                            let mut parts = vec![format!(
                                "Exit code: {} (auto-elevated to Administrator after permission error)",
                                r.exit_code
                            )];
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
                            Ok(ToolResult::ok(parts.join("\n\n")))
                        }
                        Err(e) => {
                            // UAC was denied or failed — return original error with a hint
                            let mut parts = vec![format!("Exit code: {}", exit_code)];
                            if !stdout.is_empty() {
                                parts.push(format!("STDOUT:\n{}", stdout));
                            }
                            if !stderr.is_empty() {
                                parts.push(format!("STDERR:\n{}", stderr));
                            }
                            parts.push(format!(
                                "\n⚠️ Auto-elevation attempted but failed (UAC denied or error: {}). \
                                 To retry manually, use `elevated: true` in your next shell call.",
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
#[cfg(target_os = "windows")]
fn is_permission_error(exit_code: i32, stdout: &str, stderr: &str) -> bool {
    // Non-zero exit code required — don't auto-elevate successful commands
    if exit_code == 0 {
        return false;
    }
    let combined = format!("{} {}", stdout, stderr).to_lowercase();
    // Common Windows permission error strings
    combined.contains("access is denied")
        || combined.contains("access denied")
        || combined.contains("拒绝访问")
        || combined.contains("requires elevation")
        || combined.contains("elevated")
        || combined.contains("administrator")
        || combined.contains("privileged")
        || combined.contains("0x80070005") // E_ACCESSDENIED HRESULT
        || combined.contains("error 5")    // ERROR_ACCESS_DENIED Win32
        || combined.contains("error: 5,")
        || (exit_code == 1 && combined.contains("regsvr32"))
        || combined.contains("cannot be loaded because running scripts is disabled")
        || combined.contains("unauthorizedaccessexception")
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
