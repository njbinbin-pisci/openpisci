/// PowerShell structured query tool.
/// Returns JSON output for AI to parse directly, unlike shell.rs which returns raw text.
use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

const QUERY_TIMEOUT_SECS: u64 = 30;

pub struct PowerShellTool;

#[async_trait]
impl Tool for PowerShellTool {
    fn name(&self) -> &str { "powershell_query" }

    fn description(&self) -> &str {
        "Query Windows system information via PowerShell, returning structured JSON. \
         Use for processes, services, files, registry, installed apps, network config, etc. \
         Unlike the 'shell' tool, output is always JSON for easy AI parsing. \
         Use `arch: \"x86\"` for queries that need 32-bit PowerShell (e.g. querying 32-bit COM registrations, WOW6432Node registry)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "enum": [
                        "get_processes", "get_services", "get_files",
                        "get_registry", "get_installed_apps", "get_network",
                        "get_env_vars", "get_scheduled_tasks", "get_event_log",
                        "get_disk_info", "get_system_info", "custom"
                    ],
                    "description": "Query type"
                },
                "path": {
                    "type": "string",
                    "description": "Path for get_files or registry key for get_registry"
                },
                "filter": {
                    "type": "string",
                    "description": "Filter string (e.g. process name, service name)"
                },
                "registry_value": {
                    "type": "string",
                    "description": "Registry value name (for get_registry)"
                },
                "log_name": {
                    "type": "string",
                    "description": "Event log name (for get_event_log, e.g. 'Application', 'System')"
                },
                "max_entries": {
                    "type": "integer",
                    "description": "Maximum entries to return (default: 20)"
                },
                "ps_command": {
                    "type": "string",
                    "description": "Custom PowerShell command (for 'custom' query). ConvertTo-Json is appended automatically unless already present."
                },
                "arch": {
                    "type": "string",
                    "enum": ["x64", "x86"],
                    "description": "Architecture: 'x64' = 64-bit PowerShell (default), 'x86' = 32-bit PowerShell. Use x86 when querying WOW6432Node registry or 32-bit COM registrations."
                }
            },
            "required": ["query"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let query = match input["query"].as_str() {
            Some(q) => q,
            None => return Ok(ToolResult::err("Missing required parameter: query")),
        };

        let max = input["max_entries"].as_u64().unwrap_or(20);
        let filter = input["filter"].as_str().unwrap_or("");

        let ps_cmd = match query {
            "get_processes" => {
                if filter.is_empty() {
                    format!(
                        "Get-Process | Select-Object Name,Id,CPU,WorkingSet,StartTime | \
                         Sort-Object CPU -Descending | Select-Object -First {} | ConvertTo-Json -Depth 2",
                        max
                    )
                } else {
                    format!(
                        "Get-Process -Name '*{}*' -ErrorAction SilentlyContinue | \
                         Select-Object Name,Id,CPU,WorkingSet,StartTime | ConvertTo-Json -Depth 2",
                        filter
                    )
                }
            }
            "get_services" => {
                if filter.is_empty() {
                    format!(
                        "Get-Service | Select-Object Name,DisplayName,Status,StartType | \
                         Select-Object -First {} | ConvertTo-Json -Depth 2",
                        max
                    )
                } else {
                    format!(
                        "Get-Service -Name '*{}*' -ErrorAction SilentlyContinue | \
                         Select-Object Name,DisplayName,Status,StartType | ConvertTo-Json -Depth 2",
                        filter
                    )
                }
            }
            "get_files" => {
                let path = input["path"].as_str().unwrap_or(".");
                format!(
                    "Get-ChildItem -Path '{}' -ErrorAction SilentlyContinue | \
                     Select-Object Name,FullName,Length,LastWriteTime,Attributes | \
                     Select-Object -First {} | ConvertTo-Json -Depth 2",
                    path.replace('\'', "''"), max
                )
            }
            "get_registry" => {
                let key = input["path"].as_str()
                    .unwrap_or("HKLM:\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
                let value = input["registry_value"].as_str().unwrap_or("");
                if value.is_empty() {
                    format!(
                        "Get-ItemProperty -Path '{}' -ErrorAction SilentlyContinue | ConvertTo-Json -Depth 2",
                        key.replace('\'', "''")
                    )
                } else {
                    format!(
                        "Get-ItemPropertyValue -Path '{}' -Name '{}' -ErrorAction SilentlyContinue | ConvertTo-Json",
                        key.replace('\'', "''"),
                        value.replace('\'', "''")
                    )
                }
            }
            "get_installed_apps" => {
                // Use registry-based query to avoid Get-Package's network calls
                format!(
                    "Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*', \
                     'HKLM:\\SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*' \
                     -ErrorAction SilentlyContinue | \
                     Where-Object {{ $_.DisplayName }} | \
                     Select-Object DisplayName,DisplayVersion,Publisher | \
                     Sort-Object DisplayName | Select-Object -First {} | ConvertTo-Json -Depth 2",
                    max
                )
            }
            "get_network" => {
                "Get-NetIPConfiguration | Select-Object InterfaceAlias,IPv4Address,IPv6Address,DNSServer | \
                 ConvertTo-Json -Depth 4".to_string()
            }
            "get_env_vars" => {
                "Get-ChildItem Env: | Select-Object Name,Value | ConvertTo-Json -Depth 2".to_string()
            }
            "get_scheduled_tasks" => {
                format!(
                    "Get-ScheduledTask | Select-Object TaskName,TaskPath,State | \
                     Select-Object -First {} | ConvertTo-Json -Depth 2",
                    max
                )
            }
            "get_event_log" => {
                let log = input["log_name"].as_str().unwrap_or("System");
                format!(
                    "Get-EventLog -LogName '{}' -Newest {} -ErrorAction SilentlyContinue | \
                     Select-Object TimeGenerated,EntryType,Source,Message | ConvertTo-Json -Depth 2",
                    log, max
                )
            }
            "get_disk_info" => {
                "Get-PSDrive -PSProvider FileSystem | \
                 Select-Object Name,Root,Used,Free | ConvertTo-Json -Depth 2".to_string()
            }
            "get_system_info" => {
                // Get-ComputerInfo is very slow on some systems; use faster individual cmdlets
                "@{
                    ComputerName = $env:COMPUTERNAME;
                    OSName = (Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion' -ErrorAction SilentlyContinue).ProductName;
                    OSVersion = [System.Environment]::OSVersion.VersionString;
                    OSArchitecture = $env:PROCESSOR_ARCHITECTURE;
                    TotalMemoryGB = [math]::Round((Get-CimInstance Win32_PhysicalMemory -ErrorAction SilentlyContinue | Measure-Object Capacity -Sum).Sum / 1GB, 2);
                    ProcessorName = (Get-CimInstance Win32_Processor -ErrorAction SilentlyContinue | Select-Object -First 1).Name;
                    LogicalProcessors = [System.Environment]::ProcessorCount;
                    UserName = $env:USERNAME;
                    Domain = $env:USERDOMAIN
                } | ConvertTo-Json -Depth 2".to_string()
            }
            "custom" => {
                let cmd = match input["ps_command"].as_str() {
                    Some(c) => c,
                    None => return Ok(ToolResult::err("custom query requires ps_command")),
                };
                // Append ConvertTo-Json if not already present
                if cmd.to_lowercase().contains("convertto-json") {
                    cmd.to_string()
                } else {
                    format!("{} | ConvertTo-Json -Depth 3", cmd)
                }
            }
            _ => return Ok(ToolResult::err(format!("Unknown query: {}", query))),
        };

        let arch = input["arch"].as_str().unwrap_or("x64");
        self.run_ps(&ps_cmd, arch).await
    }
}

impl PowerShellTool {
    async fn run_ps(&self, command: &str, arch: &str) -> Result<ToolResult> {
        let utf8_command = format!(
            "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;\
             $OutputEncoding=[System.Text.Encoding]::UTF8;\
             chcp 65001 | Out-Null; {}",
            command
        );

        let ps_exe = if arch == "x86" {
            r"C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe"
        } else {
            "powershell"
        };

        // CREATE_NO_WINDOW: prevents a blue console window from flashing on screen
        #[cfg(target_os = "windows")]
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let mut cmd = Command::new(ps_exe);
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", &utf8_command])
            .current_dir("C:\\")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);

        let result = timeout(Duration::from_secs(QUERY_TIMEOUT_SECS), cmd.output()).await;

        match result {
            Err(_) => Ok(ToolResult::err(format!("Query timed out after {}s", QUERY_TIMEOUT_SECS))),
            Ok(Err(e)) => Ok(ToolResult::err(format!("Failed to run PowerShell: {}", e))),
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

                if !output.status.success() {
                    let msg = if stderr.is_empty() { stdout.clone() } else { stderr.clone() };
                    if msg.is_empty() {
                        return Ok(ToolResult::err("Query failed: no output"));
                    }
                    return Ok(ToolResult::err(format!("Query failed: {}", msg)));
                }
                if stdout.is_empty() {
                    // Success but no output — return informative message, not an error
                    let note = if !stderr.is_empty() {
                        format!("No results. (stderr: {})", stderr)
                    } else {
                        "No results.".to_string()
                    };
                    return Ok(ToolResult::ok(note));
                }

                match serde_json::from_str::<Value>(&stdout) {
                    Ok(json_val) => {
                        Ok(ToolResult::ok(serde_json::to_string_pretty(&json_val).unwrap_or(stdout)))
                    }
                    Err(_) => Ok(ToolResult::ok(stdout)),
                }
            }
        }
    }
}
