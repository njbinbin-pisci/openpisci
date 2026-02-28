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
         Unlike the 'shell' tool, output is always JSON for easy AI parsing."
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
                    "description": "Custom PowerShell command (for 'custom' query, will append | ConvertTo-Json)"
                }
            },
            "required": ["query"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
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
                format!(
                    "Get-Package -ErrorAction SilentlyContinue | \
                     Select-Object Name,Version,ProviderName | \
                     Sort-Object Name | Select-Object -First {} | ConvertTo-Json -Depth 2",
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
                "Get-ComputerInfo | Select-Object \
                 WindowsProductName,WindowsVersion,OsArchitecture,\
                 CsTotalPhysicalMemory,CsProcessors,CsName | ConvertTo-Json -Depth 3".to_string()
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

        self.run_ps(&ps_cmd, &ctx.workspace_root).await
    }
}

impl PowerShellTool {
    async fn run_ps(&self, command: &str, cwd: &std::path::Path) -> Result<ToolResult> {
        // Force UTF-8 so Chinese/CJK output is not garbled
        let utf8_command = format!(
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
             $OutputEncoding = [System.Text.Encoding]::UTF8; \
             chcp 65001 | Out-Null; \
             {}",
            command
        );
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", &utf8_command])
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let result = timeout(Duration::from_secs(QUERY_TIMEOUT_SECS), cmd.output()).await;

        match result {
            Err(_) => Ok(ToolResult::err(format!("Query timed out after {}s", QUERY_TIMEOUT_SECS))),
            Ok(Err(e)) => Ok(ToolResult::err(format!("Failed to run PowerShell: {}", e))),
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

                if !output.status.success() || stdout.is_empty() {
                    let msg = if stderr.is_empty() { "No output".to_string() } else { stderr };
                    return Ok(ToolResult::err(format!("Query failed: {}", msg)));
                }

                // Validate it's JSON
                match serde_json::from_str::<Value>(&stdout) {
                    Ok(json_val) => {
                        Ok(ToolResult::ok(serde_json::to_string_pretty(&json_val).unwrap_or(stdout)))
                    }
                    Err(_) => {
                        // Return as-is if not valid JSON (some PS commands return non-JSON)
                        Ok(ToolResult::ok(stdout))
                    }
                }
            }
        }
    }
}
