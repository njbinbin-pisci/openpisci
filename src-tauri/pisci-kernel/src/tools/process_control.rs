/// Process control tool — start, kill, wait for, and check processes.
/// Critical for workflows like: start an app → wait for it to load → use uia to interact.
use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_OUTPUT_BYTES: usize = 100 * 1024;

pub struct ProcessControlTool;

#[async_trait]
impl Tool for ProcessControlTool {
    fn name(&self) -> &str {
        "process_control"
    }

    fn description(&self) -> &str {
        "Start, stop, and monitor Windows processes. \
         Essential for workflows that require launching an application and then automating it. \
         \
         Actions: \
         - 'start': Launch a process. Use wait=true to wait for it to finish and capture output. \
           Use wait=false (default) to launch in background and get the PID. \
         - 'kill': Terminate a process by PID or name. \
         - 'is_running': Check if a process is running by name or PID. Returns true/false + PID list. \
         - 'list': List all running processes matching a name filter. \
         - 'wait_for_window': Wait until a window with the given title appears (useful after launching an app). \
         \
         Typical workflow for app automation: \
         1. process_control(start, path=C:\\App\\app.exe, wait=false) → get PID \
         2. process_control(wait_for_window, window_title='App Name', timeout=30) → wait for UI \
         3. uia(list_windows) → find the window \
         4. uia(click/type/...) → interact"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start", "kill", "is_running", "list", "wait_for_window"],
                    "description": "Action to perform"
                },
                "path": {
                    "type": "string",
                    "description": "Executable path for 'start' action (e.g. C:\\Program Files\\App\\app.exe)"
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Command-line arguments for 'start' action"
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for 'start' action"
                },
                "wait": {
                    "type": "boolean",
                    "description": "For 'start': wait for process to finish and capture output (default false = launch in background)"
                },
                "pid": {
                    "type": "integer",
                    "description": "Process ID for 'kill' or 'is_running'"
                },
                "name": {
                    "type": "string",
                    "description": "Process name (e.g. 'notepad.exe', 'chrome') for 'kill', 'is_running', or 'list'. Partial match supported."
                },
                "window_title": {
                    "type": "string",
                    "description": "Window title to wait for (for 'wait_for_window'). Partial match."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default 60)"
                },
                "force": {
                    "type": "boolean",
                    "description": "For 'kill': force kill (taskkill /F), default true"
                }
            },
            "required": ["action"]
        })
    }

    fn needs_confirmation(&self, input: &Value) -> bool {
        matches!(input["action"].as_str(), Some("start") | Some("kill"))
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let action = match input["action"].as_str() {
            Some(a) => a,
            None => return Ok(ToolResult::err("Missing required parameter: action")),
        };

        match action {
            "start" => self.start_process(&input).await,
            "kill" => self.kill_process(&input).await,
            "is_running" => self.is_running(&input).await,
            "list" => self.list_processes(&input).await,
            "wait_for_window" => self.wait_for_window(&input).await,
            _ => Ok(ToolResult::err(format!("Unknown action: {}", action))),
        }
    }
}

impl ProcessControlTool {
    async fn start_process(&self, input: &Value) -> Result<ToolResult> {
        let path = match input["path"].as_str() {
            Some(p) => p,
            None => return Ok(ToolResult::err("start requires 'path' parameter")),
        };

        let wait = input["wait"].as_bool().unwrap_or(false);
        let timeout_secs = input["timeout"].as_u64().unwrap_or(DEFAULT_TIMEOUT_SECS);

        let args: Vec<String> = input["args"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // CREATE_NO_WINDOW: prevents a console window from flashing when running CLI tools
        #[cfg(target_os = "windows")]
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let mut cmd = Command::new(path);
        cmd.args(&args);

        if let Some(cwd) = input["cwd"].as_str() {
            cmd.current_dir(cwd);
        }

        if wait {
            // When waiting for output, always hide the console window
            #[cfg(target_os = "windows")]
            cmd.creation_flags(CREATE_NO_WINDOW);
            cmd.stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let run = timeout(Duration::from_secs(timeout_secs), cmd.output()).await;
            match run {
                Err(_) => Ok(ToolResult::err(format!(
                    "Process timed out after {}s",
                    timeout_secs
                ))),
                Ok(Err(e)) => Ok(ToolResult::err(format!("Failed to start process: {}", e))),
                Ok(Ok(output)) => {
                    let stdout = truncate(
                        &String::from_utf8_lossy(&output.stdout),
                        MAX_OUTPUT_BYTES * 3 / 4,
                    );
                    let stderr = truncate(
                        &String::from_utf8_lossy(&output.stderr),
                        MAX_OUTPUT_BYTES / 4,
                    );
                    let exit_code = output.status.code().unwrap_or(-1);
                    let mut parts = vec![format!("Exit code: {}", exit_code)];
                    if !stdout.is_empty() {
                        parts.push(format!("STDOUT:\n{}", stdout));
                    }
                    if !stderr.is_empty() {
                        parts.push(format!("STDERR:\n{}", stderr));
                    }
                    Ok(ToolResult::ok(parts.join("\n\n")))
                }
            }
        } else {
            // Launch in background, return PID
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
            match cmd.spawn() {
                Ok(child) => {
                    let pid = child.id().unwrap_or(0);
                    Ok(ToolResult::ok(format!(
                        "Process started in background.\nPID: {}\nPath: {}",
                        pid, path
                    )))
                }
                Err(e) => Ok(ToolResult::err(format!(
                    "Failed to start process '{}': {}",
                    path, e
                ))),
            }
        }
    }

    async fn kill_process(&self, input: &Value) -> Result<ToolResult> {
        let force = input["force"].as_bool().unwrap_or(true);
        let force_flag = if force { "/F " } else { "" };

        let ps_cmd = if let Some(pid) = input["pid"].as_u64() {
            format!("taskkill {}  /PID {} 2>&1; $LASTEXITCODE", force_flag, pid)
        } else if let Some(name) = input["name"].as_str() {
            format!(
                "taskkill {}/IM \"{}\" 2>&1; $LASTEXITCODE",
                force_flag, name
            )
        } else {
            return Ok(ToolResult::err("kill requires 'pid' or 'name'"));
        };

        let output = run_ps(&ps_cmd).await?;
        Ok(ToolResult::ok(output))
    }

    async fn is_running(&self, input: &Value) -> Result<ToolResult> {
        let ps_cmd = if let Some(pid) = input["pid"].as_u64() {
            format!(
                "$p = Get-Process -Id {} -ErrorAction SilentlyContinue; \
                 if ($p) {{ @{{running=$true; pid={pid}; name=$p.Name}} | ConvertTo-Json }} \
                 else {{ @{{running=$false; pid={pid}}} | ConvertTo-Json }}",
                pid,
                pid = pid
            )
        } else if let Some(name) = input["name"].as_str() {
            format!(
                "$procs = Get-Process -Name '*{}*' -ErrorAction SilentlyContinue; \
                 if ($procs) {{ \
                     @{{running=$true; count=$procs.Count; \
                       pids=($procs | ForEach-Object {{$_.Id}})}} | ConvertTo-Json \
                 }} else {{ @{{running=$false; name='{}'}} | ConvertTo-Json }}",
                name, name
            )
        } else {
            return Ok(ToolResult::err("is_running requires 'pid' or 'name'"));
        };

        let output = run_ps(&ps_cmd).await?;
        Ok(ToolResult::ok(output))
    }

    async fn list_processes(&self, input: &Value) -> Result<ToolResult> {
        let filter = input["name"].as_str().unwrap_or("*");
        let ps_cmd = format!(
            "Get-Process -Name '*{}*' -ErrorAction SilentlyContinue | \
             Select-Object Id,Name,CPU,@{{N='MemMB';E={{[math]::Round($_.WorkingSet/1MB,1)}}}} | \
             Sort-Object Name | ConvertTo-Json -Depth 2",
            filter
        );
        let output = run_ps(&ps_cmd).await?;
        Ok(ToolResult::ok(output))
    }

    async fn wait_for_window(&self, input: &Value) -> Result<ToolResult> {
        let title = match input["window_title"].as_str() {
            Some(t) => t,
            None => return Ok(ToolResult::err("wait_for_window requires 'window_title'")),
        };
        let timeout_secs = input["timeout"].as_u64().unwrap_or(30);

        // Poll every 500ms for a window with the given title
        let ps_cmd = format!(
            r#"
$deadline = [DateTime]::Now.AddSeconds({timeout})
$found = $false
while ([DateTime]::Now -lt $deadline) {{
    $windows = Get-Process | Where-Object {{ $_.MainWindowTitle -like '*{title}*' }} | Select-Object Id,Name,MainWindowTitle
    if ($windows) {{
        $found = $true
        $windows | ConvertTo-Json -Depth 2
        break
    }}
    Start-Sleep -Milliseconds 500
}}
if (-not $found) {{
    Write-Output "TIMEOUT: Window '{title}' did not appear within {timeout}s"
}}
"#,
            title = title.replace('\'', "''"),
            timeout = timeout_secs
        );

        let run = timeout(Duration::from_secs(timeout_secs + 5), run_ps(&ps_cmd)).await;

        match run {
            Err(_) => Ok(ToolResult::err(format!(
                "wait_for_window timed out after {}s",
                timeout_secs
            ))),
            Ok(Err(e)) => Ok(ToolResult::err(format!("Failed: {}", e))),
            Ok(Ok(output)) => Ok(ToolResult::ok(output)),
        }
    }
}

async fn run_ps(command: &str) -> Result<String> {
    let utf8_cmd = format!(
        "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;\
         $OutputEncoding=[System.Text.Encoding]::UTF8;\
         chcp 65001 | Out-Null; {}",
        command
    );
    // CREATE_NO_WINDOW: prevents a blue console window from flashing on screen
    #[cfg(target_os = "windows")]
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let mut ps_cmd = Command::new("powershell");
    ps_cmd
        .args(["-NoProfile", "-NonInteractive", "-Command", &utf8_cmd])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    ps_cmd.creation_flags(CREATE_NO_WINDOW);

    let output = ps_cmd.output().await?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stdout.is_empty() && !stderr.is_empty() {
        Ok(stderr)
    } else {
        Ok(stdout)
    }
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    let half = max / 2;
    format!(
        "{}\n...[truncated]...\n{}",
        &s[..half],
        &s[s.len() - half..]
    )
}
