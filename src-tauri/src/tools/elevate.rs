/// Elevated (administrator) command execution via Windows UAC.
///
/// How it works:
/// 1. Write the command to a temp .ps1 script file.
/// 2. Use ShellExecuteW with verb="runas" to launch a new elevated PowerShell process.
///    This triggers the Windows UAC consent dialog — the user sees and approves it.
/// 3. The elevated process runs the script and writes stdout/stderr/exitcode to a temp result file.
/// 4. We poll the result file until it appears (the elevated process writes it when done).
/// 5. Read and return the result, then clean up temp files.
///
/// Limitations:
/// - The UAC dialog is shown by Windows itself (not our app) — this is by design and correct.
/// - If the user clicks "No" in UAC, ShellExecuteW returns error code 5 (ERROR_ACCESS_DENIED).
/// - Elevated process runs in a separate session; it cannot interact with our process directly.
/// - Timeout applies to the entire operation including UAC wait time.

use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::{sleep, timeout};

#[cfg(target_os = "windows")]
use windows::Win32::UI::Shell::ShellExecuteW;
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
#[cfg(target_os = "windows")]
use windows::core::PCWSTR;

pub struct ElevatedResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run a PowerShell command with administrator privileges via UAC.
/// Returns the output after the elevated process completes.
/// `timeout_secs` includes the time the user takes to respond to the UAC dialog.
pub async fn run_elevated_powershell(
    command: &str,
    arch: &str,
    timeout_secs: u64,
) -> Result<ElevatedResult> {
    let tmp_dir = std::env::temp_dir();
    let id = uuid::Uuid::new_v4().simple().to_string();
    let script_path = tmp_dir.join(format!("pisci_elev_{}.ps1", id));
    let result_path = tmp_dir.join(format!("pisci_elev_{}.result", id));

    // Write the wrapper script that captures output and writes to result file
    let result_path_escaped = result_path.to_string_lossy().replace('\\', "\\\\");
    let script_content = format!(
        r#"
$ErrorActionPreference = 'Continue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8
chcp 65001 | Out-Null

$stdout = ""
$stderr = ""
$exitCode = 0

try {{
    $result = & {{
        {command}
    }} 2>&1

    $stdout = ($result | Where-Object {{ $_ -isnot [System.Management.Automation.ErrorRecord] }}) -join "`n"
    $stderr = ($result | Where-Object {{ $_ -is [System.Management.Automation.ErrorRecord] }}) -join "`n"
    $exitCode = $LASTEXITCODE
    if ($exitCode -eq $null) {{ $exitCode = 0 }}
}} catch {{
    $stderr = $_.Exception.Message
    $exitCode = 1
}}

$output = @{{
    exit_code = $exitCode
    stdout = $stdout
    stderr = $stderr
}} | ConvertTo-Json -Compress

[System.IO.File]::WriteAllText("{result_path_escaped}", $output, [System.Text.Encoding]::UTF8)
"#,
        command = command,
        result_path_escaped = result_path_escaped
    );

    std::fs::write(&script_path, script_content.as_bytes())?;

    // Choose PowerShell executable
    let ps_exe = if arch == "x86" {
        r"C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe".to_string()
    } else {
        "powershell.exe".to_string()
    };

    // Build the arguments: -File <script_path>
    let script_path_str = script_path.to_string_lossy().to_string();
    let ps_args = format!("-NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{}\"", script_path_str);

    // Launch elevated via ShellExecuteW runas
    #[cfg(target_os = "windows")]
    let launch_result = launch_elevated_windows(&ps_exe, &ps_args);

    #[cfg(not(target_os = "windows"))]
    let launch_result: Result<()> = Err(anyhow::anyhow!("UAC elevation is only supported on Windows"));

    if let Err(e) = launch_result {
        let _ = std::fs::remove_file(&script_path);
        return Err(e);
    }

    // Poll for result file with timeout
    let poll_result = timeout(
        Duration::from_secs(timeout_secs),
        poll_for_result(&result_path),
    ).await;

    // Clean up script file
    let _ = std::fs::remove_file(&script_path);

    match poll_result {
        Err(_) => {
            let _ = std::fs::remove_file(&result_path);
            Err(anyhow::anyhow!(
                "Elevated command timed out after {}s. \
                 The user may have cancelled the UAC dialog, or the command took too long.",
                timeout_secs
            ))
        }
        Ok(Err(e)) => {
            let _ = std::fs::remove_file(&result_path);
            Err(e)
        }
        Ok(Ok(content)) => {
            let _ = std::fs::remove_file(&result_path);
            parse_result(&content)
        }
    }
}

async fn poll_for_result(result_path: &PathBuf) -> Result<String> {
    // Poll every 500ms until the result file appears
    loop {
        if result_path.exists() {
            // Small delay to ensure the file write is complete
            sleep(Duration::from_millis(100)).await;
            let content = std::fs::read_to_string(result_path)?;
            if !content.is_empty() {
                return Ok(content);
            }
        }
        sleep(Duration::from_millis(500)).await;
    }
}

fn parse_result(json_str: &str) -> Result<ElevatedResult> {
    let v: serde_json::Value = serde_json::from_str(json_str.trim())
        .map_err(|e| anyhow::anyhow!("Failed to parse elevated result: {} | raw: {}", e, json_str))?;

    Ok(ElevatedResult {
        exit_code: v["exit_code"].as_i64().unwrap_or(-1) as i32,
        stdout: v["stdout"].as_str().unwrap_or("").to_string(),
        stderr: v["stderr"].as_str().unwrap_or("").to_string(),
    })
}

#[cfg(target_os = "windows")]
fn launch_elevated_windows(exe: &str, args: &str) -> Result<()> {
    let verb = "runas\0".encode_utf16().collect::<Vec<u16>>();
    let file: Vec<u16> = exe.encode_utf16().chain(std::iter::once(0)).collect();
    let params: Vec<u16> = args.encode_utf16().chain(std::iter::once(0)).collect();

    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(params.as_ptr()),
            PCWSTR::null(),
            SW_HIDE,
        )
    };

    // ShellExecuteW returns > 32 on success
    let code = result.0 as usize;
    if code > 32 {
        Ok(())
    } else if code == 5 {
        // ERROR_ACCESS_DENIED — user clicked "No" in UAC dialog
        Err(anyhow::anyhow!(
            "UAC elevation was denied by the user (error code 5). \
             The operation requires administrator privileges. \
             Please try again and click 'Yes' in the UAC dialog."
        ))
    } else {
        Err(anyhow::anyhow!(
            "ShellExecuteW runas failed with code {}. \
             The system may not support UAC elevation in the current context.",
            code
        ))
    }
}
