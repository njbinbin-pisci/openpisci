use anyhow::Result;
use async_trait::async_trait;
/// Cross-platform system information query — replaces wmi/powershell_query on Linux/macOS.
use pisci_kernel::agent::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use tokio::process::Command;

pub struct SystemInfoTool;

#[async_trait]
impl Tool for SystemInfoTool {
    fn name(&self) -> &str {
        "system_info"
    }

    fn description(&self) -> &str {
        "Query system information cross-platform: CPU, memory, disk, network, processes, OS version, GPU. \
         Use action=query with a category parameter. Supports all major desktop platforms."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["query", "list_processes", "list_services"],
                    "description": "query: get info by category; list_processes: running processes; list_services: system services/daemons"
                },
                "category": {
                    "type": "string",
                    "enum": ["cpu", "memory", "disk", "network", "os", "gpu", "all"],
                    "description": "Information category for action=query. 'all' returns everything."
                },
                "top_n": {
                    "type": "integer",
                    "description": "Limit to top N processes (for list_processes, default 20)"
                }
            },
            "required": ["action"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let action = match input["action"].as_str() {
            Some(a) => a,
            None => return Ok(ToolResult::err("Missing required parameter: action")),
        };

        match action {
            "query" => self.query(&input).await,
            "list_processes" => self.list_processes(&input).await,
            "list_services" => self.list_services().await,
            _ => Ok(ToolResult::err(format!("Unknown action: {}", action))),
        }
    }
}

impl SystemInfoTool {
    async fn query(&self, input: &Value) -> Result<ToolResult> {
        let category = input["category"].as_str().unwrap_or("all");
        let mut sections: Vec<String> = Vec::new();

        if category == "all" {
            sections.push(cpu_info().await);
            sections.push(memory_info().await);
            sections.push(disk_info().await);
            sections.push(network_info().await);
            sections.push(os_info().await);
            sections.push(gpu_info().await);
        } else {
            match category {
                "cpu" => sections.push(cpu_info().await),
                "memory" => sections.push(memory_info().await),
                "disk" => sections.push(disk_info().await),
                "network" => sections.push(network_info().await),
                "os" => sections.push(os_info().await),
                "gpu" => sections.push(gpu_info().await),
                _ => return Ok(ToolResult::err(format!("Unknown category: {}", category))),
            }
        }

        Ok(ToolResult::ok(sections.join("\n\n")))
    }

    async fn list_processes(&self, input: &Value) -> Result<ToolResult> {
        let top_n = input["top_n"].as_u64().unwrap_or(20);
        process_list(top_n as usize).await
    }

    async fn list_services(&self) -> Result<ToolResult> {
        service_list().await
    }
}

// ─── Linux implementation ────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
async fn cpu_info() -> String {
    let lines = read_commands(&[
        ("lscpu", &["-e=CPU,MAXMHZ,MHZ"]),
        ("sh", &["-c", "grep 'model name' /proc/cpuinfo | head -1 | cut -d: -f2"]),
        ("sh", &["-c", "nproc"]),
    ])
    .await;
    format!(
        "=== CPU ===\n  Model: {}\n  Cores: {}\n  Frequencies (MHz):\n{}",
        lines.get(1).map(|s| s.trim()).unwrap_or("unknown"),
        lines.get(2).map(|s| s.trim()).unwrap_or("unknown"),
        lines.first().map(|s| {
            s.lines()
                .map(|l| format!("    {}", l.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        }).unwrap_or_default(),
    )
}

#[cfg(target_os = "linux")]
async fn memory_info() -> String {
    let lines = read_commands(&[
        ("free", &["-h"]),
        ("sh", &["-c", "grep -E 'MemTotal|MemAvailable|SwapTotal|SwapFree' /proc/meminfo"]),
    ])
    .await;
    format!(
        "=== Memory ===\n{}\n\n  /proc/meminfo:\n{}",
        lines.first().map(|s| s.as_str()).unwrap_or(""),
        lines.get(1).map(|s| {
            s.lines()
                .map(|l| format!("    {}", l.trim().replace(":", ": ")))
                .collect::<Vec<_>>()
                .join("\n")
        }).unwrap_or_default(),
    )
}

#[cfg(target_os = "linux")]
async fn disk_info() -> String {
    let lines = read_commands(&[
        ("df", &["-h", "-x", "tmpfs", "-x", "devtmpfs", "-x", "squashfs"]),
    ])
    .await;
    format!(
        "=== Disk ===\n{}",
        lines.first().map(|s| {
            s.lines()
                .map(|l| format!("    {}", l.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        }).unwrap_or_default(),
    )
}

#[cfg(target_os = "linux")]
async fn network_info() -> String {
    let lines = read_commands(&[
        ("ip", &["addr", "show"]),
        ("sh", &["-c", "ip route | grep default"]),
    ])
    .await;
    format!(
        "=== Network ===\n  Interfaces:\n{}\n  Default route: {}",
        lines.first().map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| format!("    {}", l.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        }).unwrap_or_default(),
        lines.get(1).map(|s| s.trim()).unwrap_or("none"),
    )
}

#[cfg(target_os = "linux")]
async fn os_info() -> String {
    let lines = read_commands(&[
        ("uname", &["-a"]),
        ("sh", &["-c", "cat /etc/os-release 2>/dev/null || cat /etc/lsb-release 2>/dev/null || echo 'Unknown'"]),
    ])
    .await;
    format!(
        "=== OS ===\n  Kernel: {}\n  Release:\n{}",
        lines.first().map(|s| s.trim()).unwrap_or(""),
        lines.get(1).map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty() && l.contains('='))
                .map(|l| format!("    {}", l.trim().replace("=", " = ")))
                .collect::<Vec<_>>()
                .join("\n")
        }).unwrap_or_default(),
    )
}

#[cfg(target_os = "linux")]
async fn gpu_info() -> String {
    let lines = read_commands(&[
        ("sh", &["-c", "lspci | grep -i -E 'vga|3d|display' 2>/dev/null || echo 'No GPU detected'"]),
    ])
    .await;
    format!(
        "=== GPU ===\n{}",
        lines.first().map(|s| {
            s.lines()
                .map(|l| format!("    {}", l.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        }).unwrap_or_else(|| "    No GPU detected".to_string()),
    )
}

#[cfg(target_os = "linux")]
async fn process_list(top_n: usize) -> Result<ToolResult> {
    let output = Command::new("ps")
        .args(["aux", "--sort=-%mem"])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("ps failed: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    let header = lines.first().copied().unwrap_or("");
    let body: Vec<&str> = lines.iter().skip(1).take(top_n).copied().collect();

    Ok(ToolResult::ok(format!(
        "Top {} processes (by memory):\n  {}\n{}",
        body.len(),
        header,
        body.iter().map(|l| format!("  {}", l.trim())).collect::<Vec<_>>().join("\n"),
    )))
}

#[cfg(target_os = "linux")]
async fn service_list() -> Result<ToolResult> {
    // Try systemctl first, fall back to service --status-all
    let output = Command::new("sh")
        .args(["-c", "systemctl list-units --type=service --all --no-pager 2>/dev/null | head -40 || service --status-all 2>/dev/null | head -40"])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("service list failed: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();

    if lines.is_empty() {
        Ok(ToolResult::ok("No service information available."))
    } else {
        Ok(ToolResult::ok(format!(
            "Services (first {}):\n{}",
            lines.len().min(40),
            lines.iter().map(|l| format!("  {}", l.trim())).collect::<Vec<_>>().join("\n"),
        )))
    }
}

// ─── macOS implementation ─────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
async fn cpu_info() -> String {
    let lines = read_commands(&[
        ("sysctl", &["-n", "machdep.cpu.brand_string"]),
        ("sysctl", &["-n", "hw.ncpu"]),
    ])
    .await;
    format!(
        "=== CPU ===\n  Model: {}\n  Cores: {}",
        lines.first().map(|s| s.trim()).unwrap_or("unknown"),
        lines.get(1).map(|s| s.trim()).unwrap_or("unknown"),
    )
}

#[cfg(target_os = "macos")]
async fn memory_info() -> String {
    let lines = read_commands(&[
        ("vm_stat", &[]),
        ("sysctl", &["hw.memsize"]),
    ])
    .await;
    let total_bytes: u64 = lines.get(1).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let total_gb = total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    format!(
        "=== Memory ===\n  Total: {:.1} GB\n  VM Stats:\n{}",
        total_gb,
        lines.first().map(|s| {
            s.lines()
                .map(|l| format!("    {}", l.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        }).unwrap_or_default(),
    )
}

#[cfg(target_os = "macos")]
async fn disk_info() -> String {
    let lines = read_commands(&[("df", &["-h"])]).await;
    format!(
        "=== Disk ===\n{}",
        lines.first().map(|s| {
            s.lines()
                .map(|l| format!("    {}", l.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        }).unwrap_or_default(),
    )
}

#[cfg(target_os = "macos")]
async fn network_info() -> String {
    let lines = read_commands(&[
        ("ifconfig", &[]),
        ("sh", &["-c", "netstat -rn | grep default | head -1"]),
    ])
    .await;
    format!(
        "=== Network ===\n  Interfaces:\n{}\n  Default route: {}",
        lines.first().map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty() && (l.contains("flags=") || l.contains("inet ")))
                .map(|l| format!("    {}", l.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        }).unwrap_or_default(),
        lines.get(1).map(|s| s.trim()).unwrap_or("none"),
    )
}

#[cfg(target_os = "macos")]
async fn os_info() -> String {
    let lines = read_commands(&[
        ("sw_vers", &[]),
        ("uname", &["-a"]),
    ])
    .await;
    format!(
        "=== OS ===\n  Version:\n{}\n  Kernel: {}",
        lines.first().map(|s| {
            s.lines()
                .map(|l| format!("    {}", l.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        }).unwrap_or_default(),
        lines.get(1).map(|s| s.trim()).unwrap_or(""),
    )
}

#[cfg(target_os = "macos")]
async fn gpu_info() -> String {
    let lines = read_commands(&[
        ("system_profiler", &["SPDisplaysDataType"]),
    ])
    .await;
    format!(
        "=== GPU ===\n{}",
        lines.first().map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty() && !l.starts_with("Graphics"))
                .map(|l| format!("    {}", l.trim()))
                .take(30)
                .collect::<Vec<_>>()
                .join("\n")
        }).unwrap_or("    No GPU info"),
    )
}

#[cfg(target_os = "macos")]
async fn process_list(top_n: usize) -> Result<ToolResult> {
    let output = Command::new("ps")
        .args(["aux", "-r"])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("ps failed: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    let header = lines.first().copied().unwrap_or("");
    let body: Vec<&str> = lines.iter().skip(1).take(top_n).copied().collect();

    Ok(ToolResult::ok(format!(
        "Top {} processes:\n  {}\n{}",
        body.len(),
        header,
        body.iter().map(|l| format!("  {}", l.trim())).collect::<Vec<_>>().join("\n"),
    )))
}

#[cfg(target_os = "macos")]
async fn service_list() -> Result<ToolResult> {
    let output = Command::new("launchctl")
        .args(["list"])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("launchctl failed: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).take(30).collect();

    if lines.is_empty() {
        Ok(ToolResult::ok("No service information available."))
    } else {
        Ok(ToolResult::ok(format!(
            "Services (first 30):\n{}",
            lines.iter().map(|l| format!("  {}", l.trim())).collect::<Vec<_>>().join("\n"),
        )))
    }
}

// ─── Windows implementation (shell-based, complements wmi/powershell_query) ───

#[cfg(target_os = "windows")]
async fn cpu_info() -> String {
    let lines = read_commands(&[
        ("powershell", &["-Command", "Get-WmiObject Win32_Processor | Select-Object -ExpandProperty Name"]),
        ("powershell", &["-Command", "(Get-WmiObject Win32_ComputerSystem).NumberOfLogicalProcessors"]),
    ])
    .await;
    format!(
        "=== CPU ===\n  Model: {}\n  Logical Cores: {}",
        lines.first().map(|s| s.trim()).unwrap_or("unknown"),
        lines.get(1).map(|s| s.trim()).unwrap_or("unknown"),
    )
}

#[cfg(target_os = "windows")]
async fn memory_info() -> String {
    let lines = read_commands(&[
        ("powershell", &["-Command", "$os=Get-WmiObject Win32_OperatingSystem; Write-Output \"TotalVisibleMemorySize=$($os.TotalVisibleMemorySize)KB`nFreePhysicalMemory=$($os.FreePhysicalMemory)KB\""]),
    ])
    .await;
    format!(
        "=== Memory ===\n{}",
        lines.first().map(|s| {
            s.lines()
                .map(|l| format!("    {}", l.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        }).unwrap_or_default(),
    )
}

#[cfg(target_os = "windows")]
async fn disk_info() -> String {
    let lines = read_commands(&[
        ("powershell", &["-Command", "Get-PSDrive -PSProvider FileSystem | Select-Object Name,Used,Free | Format-Table -AutoSize"]),
    ])
    .await;
    format!(
        "=== Disk ===\n{}",
        lines.first().map(|s| s.as_str()).unwrap_or(""),
    )
}

#[cfg(target_os = "windows")]
async fn network_info() -> String {
    let lines = read_commands(&[
        ("powershell", &["-Command", "Get-NetIPAddress -AddressFamily IPv4 | Where-Object {$_.InterfaceAlias -notlike '*Loopback*'} | Select-Object InterfaceAlias,IPAddress | Format-Table -AutoSize"]),
    ])
    .await;
    format!(
        "=== Network ===\n{}",
        lines.first().map(|s| s.as_str()).unwrap_or(""),
    )
}

#[cfg(target_os = "windows")]
async fn os_info() -> String {
    let lines = read_commands(&[
        ("powershell", &["-Command", "(Get-WmiObject Win32_OperatingSystem).Caption"]),
        ("powershell", &["-Command", "(Get-WmiObject Win32_OperatingSystem).Version"]),
    ])
    .await;
    format!(
        "=== OS ===\n  Name: {}\n  Version: {}",
        lines.first().map(|s| s.trim()).unwrap_or("unknown"),
        lines.get(1).map(|s| s.trim()).unwrap_or("unknown"),
    )
}

#[cfg(target_os = "windows")]
async fn gpu_info() -> String {
    let lines = read_commands(&[
        ("powershell", &["-Command", "Get-WmiObject Win32_VideoController | Select-Object Name,DriverVersion | Format-List"]),
    ])
    .await;
    format!(
        "=== GPU ===\n{}",
        lines.first().map(|s| s.as_str()).unwrap_or("No GPU info"),
    )
}

#[cfg(target_os = "windows")]
async fn process_list(top_n: usize) -> Result<ToolResult> {
    let ps = format!(
        "Get-Process | Sort-Object WorkingSet -Descending | Select-Object -First {} Name,Id,CPU,WorkingSet | Format-Table -AutoSize",
        top_n
    );
    let output = Command::new("powershell")
        .args(["-Command", &ps])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("powershell process list failed: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(ToolResult::ok(format!(
        "Top {} processes:\n{}",
        top_n,
        stdout.lines().map(|l| format!("  {}", l)).collect::<Vec<_>>().join("\n"),
    )))
}

#[cfg(target_os = "windows")]
async fn service_list() -> Result<ToolResult> {
    let output = Command::new("powershell")
        .args(["-Command", "Get-Service | Select-Object Name,Status,DisplayName | Format-Table -AutoSize"])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("Get-Service failed: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).take(30).collect();

    Ok(ToolResult::ok(format!(
        "Services (first 30):\n{}",
        lines.iter().map(|l| format!("  {}", l.trim())).collect::<Vec<_>>().join("\n"),
    )))
}

// ─── Shared command helpers ────────────────────────────────────────────────────

/// Run several commands in parallel and collect their stdout.
async fn read_commands(commands: &[(&str, &[&str])]) -> Vec<String> {
    let futures: Vec<_> = commands
        .iter()
        .map(|(cmd, args)| async move {
            let output = Command::new(*cmd)
                .args(*args)
                .output()
                .await;
            match output {
                Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
                Err(e) => format!("[{} error: {}]", cmd, e),
            }
        })
        .collect();

    futures::future::join_all(futures).await
}
