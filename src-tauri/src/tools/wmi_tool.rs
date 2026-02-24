/// WMI (Windows Management Instrumentation) query tool.
/// Executes WQL queries via PowerShell Get-CimInstance for structured system data.
use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

const WMI_TIMEOUT_SECS: u64 = 30;

pub struct WmiTool;

#[async_trait]
impl Tool for WmiTool {
    fn name(&self) -> &str { "wmi" }

    fn description(&self) -> &str {
        "Query Windows system information via WMI (WQL). \
         Returns structured JSON data about processes, hardware, OS, disks, network, etc. \
         Use preset queries or write custom WQL. Read-only, safe to run concurrently."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "WQL query string (e.g. 'SELECT * FROM Win32_Process WHERE Name=\"chrome.exe\"') OR preset name"
                },
                "preset": {
                    "type": "string",
                    "enum": [
                        "system_info", "cpu_info", "memory_info", "disk_info",
                        "running_processes", "network_adapters", "installed_software",
                        "startup_programs", "services", "bios_info", "gpu_info"
                    ],
                    "description": "Use a preset query instead of writing WQL"
                },
                "class": {
                    "type": "string",
                    "description": "WMI class name for simple SELECT * queries (e.g. 'Win32_Process')"
                },
                "filter": {
                    "type": "string",
                    "description": "WHERE clause filter (e.g. 'Name=\"chrome.exe\"')"
                },
                "properties": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Properties to select (default: all)"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum results to return (default: 20)"
                }
            }
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let max = input["max_results"].as_u64().unwrap_or(20);

        // Build WQL from preset, class, or direct query
        let wql = if let Some(preset) = input["preset"].as_str() {
            self.preset_to_wql(preset)
        } else if let Some(class) = input["class"].as_str() {
            let props = if let Some(arr) = input["properties"].as_array() {
                arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(",")
            } else {
                "*".to_string()
            };
            let filter = input["filter"].as_str().unwrap_or("");
            if filter.is_empty() {
                format!("SELECT {} FROM {}", props, class)
            } else {
                format!("SELECT {} FROM {} WHERE {}", props, class, filter)
            }
        } else if let Some(q) = input["query"].as_str() {
            q.to_string()
        } else {
            return Ok(ToolResult::err("Provide 'preset', 'class', or 'query'"));
        };

        self.run_wql(&wql, max, &ctx.workspace_root).await
    }
}

impl WmiTool {
    fn preset_to_wql(&self, preset: &str) -> String {
        match preset {
            "system_info"       => "SELECT Caption,Version,BuildNumber,OSArchitecture,TotalVisibleMemorySize,FreePhysicalMemory FROM Win32_OperatingSystem".into(),
            "cpu_info"          => "SELECT Name,NumberOfCores,NumberOfLogicalProcessors,MaxClockSpeed,LoadPercentage FROM Win32_Processor".into(),
            "memory_info"       => "SELECT Capacity,Speed,Manufacturer,PartNumber FROM Win32_PhysicalMemory".into(),
            "disk_info"         => "SELECT DeviceID,MediaType,Size,FreeSpace,FileSystem,VolumeName FROM Win32_LogicalDisk WHERE DriveType=3".into(),
            "running_processes" => "SELECT Name,ProcessId,ExecutablePath,WorkingSetSize,CreationDate FROM Win32_Process".into(),
            "network_adapters"  => "SELECT Description,MACAddress,IPAddress,IPSubnet,DefaultIPGateway,DNSServerSearchOrder FROM Win32_NetworkAdapterConfiguration WHERE IPEnabled=True".into(),
            "installed_software" => "SELECT Name,Version,Vendor,InstallDate FROM Win32_Product".into(),
            "startup_programs"  => "SELECT Name,Command,Location FROM Win32_StartupCommand".into(),
            "services"          => "SELECT Name,DisplayName,State,StartMode,PathName FROM Win32_Service".into(),
            "bios_info"         => "SELECT Manufacturer,Name,Version,ReleaseDate,SMBIOSBIOSVersion FROM Win32_BIOS".into(),
            "gpu_info"          => "SELECT Name,AdapterRAM,DriverVersion,VideoModeDescription FROM Win32_VideoController".into(),
            _                   => format!("SELECT * FROM Win32_{}", preset),
        }
    }

    async fn run_wql(&self, wql: &str, max: u64, cwd: &std::path::Path) -> Result<ToolResult> {
        // Use Get-CimInstance (modern, faster than Get-WmiObject)
        let ps_cmd = format!(
            "Get-CimInstance -Query {} | Select-Object -First {} | ConvertTo-Json -Depth 3 -WarningAction SilentlyContinue",
            serde_json::to_string(wql).unwrap(),
            max
        );

        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", &ps_cmd])
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let result = timeout(Duration::from_secs(WMI_TIMEOUT_SECS), cmd.output()).await;

        match result {
            Err(_) => Ok(ToolResult::err(format!("WMI query timed out after {}s", WMI_TIMEOUT_SECS))),
            Ok(Err(e)) => Ok(ToolResult::err(format!("Failed to run WMI query: {}", e))),
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

                if stdout.is_empty() {
                    let msg = if stderr.is_empty() { "No results".to_string() } else { stderr };
                    return Ok(ToolResult::ok(format!("WMI query returned no results.\nQuery: {}\n{}", wql, msg)));
                }

                match serde_json::from_str::<Value>(&stdout) {
                    Ok(json_val) => Ok(ToolResult::ok(format!(
                        "WMI Query: {}\n\n{}",
                        wql,
                        serde_json::to_string_pretty(&json_val).unwrap_or(stdout)
                    ))),
                    Err(_) => Ok(ToolResult::ok(format!("WMI Query: {}\n\n{}", wql, stdout))),
                }
            }
        }
    }
}
