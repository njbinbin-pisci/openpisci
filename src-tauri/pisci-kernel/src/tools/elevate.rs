/// Cross-platform elevated command execution helpers.
///
/// Windows: ShellExecute `runas` triggers the native UAC consent dialog.
/// macOS: AppleScript `do shell script ... with administrator privileges`
/// opens the system admin-password prompt.
/// Linux: `pkexec` asks polkit to show an authentication dialog; availability
/// depends on the desktop environment / installed polkit agent.
use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::{sleep, timeout};

#[cfg(target_os = "windows")]
use windows::core::PCWSTR;
#[cfg(target_os = "windows")]
use windows::Win32::UI::Shell::ShellExecuteW;
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

pub struct ElevatedResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[cfg(not(target_os = "windows"))]
struct ElevatedPaths {
    script_path: PathBuf,
    result_path: PathBuf,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
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

    // Write the wrapper script that captures output and writes to result file.
    //
    // Key design decisions:
    // 1. Write the user command to a separate inner script file, then run it via
    //    Start-Process with stdout/stderr redirected to temp files. This correctly
    //    captures $LASTEXITCODE from native executables (regsvr32, reg, etc.) that
    //    the & { } 2>&1 approach loses.
    // 2. Write result with UTF8NoBOM (New-Object System.Text.UTF8Encoding($false))
    //    to avoid the BOM that Windows [System.Text.Encoding]::UTF8 emits by default,
    //    which would cause serde_json to fail with "expected value at line 1 column 1".
    let result_path_escaped = result_path.to_string_lossy().replace('\\', "\\\\");
    let inner_script_path = tmp_dir.join(format!("pisci_elev_{}_inner.ps1", id));
    let inner_script_path_escaped = inner_script_path.to_string_lossy().replace('\\', "\\\\");

    // Write the inner script (the actual user command) separately
    let inner_content = format!(
        "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8\n$OutputEncoding=[System.Text.Encoding]::UTF8\nchcp 65001 | Out-Null\n{}\n",
        command
    );
    std::fs::write(&inner_script_path, inner_content.as_bytes())?;

    // Use the same PowerShell bitness for the inner process as requested by the caller
    let inner_ps_exe = if arch == "x86" {
        r"C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe"
    } else {
        "powershell.exe"
    };

    let script_content = format!(
        r#"
$tmpOut = [System.IO.Path]::GetTempFileName()
$tmpErr = [System.IO.Path]::GetTempFileName()
$exitCode = 0

try {{
    $proc = Start-Process -FilePath "{inner_ps_exe}" `
        -ArgumentList @("-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", "{inner_script_path_escaped}") `
        -RedirectStandardOutput $tmpOut `
        -RedirectStandardError $tmpErr `
        -Wait -PassThru -NoNewWindow
    $exitCode = if ($proc.ExitCode -ne $null) {{ $proc.ExitCode }} else {{ 0 }}
}} catch {{
    $exitCode = 1
    $utf8NoBom2 = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($tmpErr, $_.Exception.Message, $utf8NoBom2)
}}

$stdout = if (Test-Path $tmpOut) {{ [System.IO.File]::ReadAllText($tmpOut, [System.Text.Encoding]::UTF8).Trim() }} else {{ "" }}
$stderr = if (Test-Path $tmpErr) {{ [System.IO.File]::ReadAllText($tmpErr, [System.Text.Encoding]::UTF8).Trim() }} else {{ "" }}
Remove-Item $tmpOut, $tmpErr, "{inner_script_path_escaped}" -ErrorAction SilentlyContinue

$output = [PSCustomObject]@{{
    exit_code = [int]$exitCode
    stdout = $stdout
    stderr = $stderr
}} | ConvertTo-Json -Compress

$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText("{result_path_escaped}", $output, $utf8NoBom)
"#,
        inner_ps_exe = inner_ps_exe,
        inner_script_path_escaped = inner_script_path_escaped,
        result_path_escaped = result_path_escaped
    );

    std::fs::write(&script_path, script_content.as_bytes())?;

    // Launch elevated via ShellExecuteW runas
    #[cfg(target_os = "windows")]
    let launch_result = {
        let ps_exe = if arch == "x86" {
            r"C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe".to_string()
        } else {
            "powershell.exe".to_string()
        };

        let script_path_str = script_path.to_string_lossy().to_string();
        let ps_args = format!(
            "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{}\"",
            script_path_str
        );

        launch_elevated_windows(&ps_exe, &ps_args)
    };

    #[cfg(not(target_os = "windows"))]
    let launch_result: Result<()> = Err(anyhow::anyhow!(
        "UAC elevation is only supported on Windows"
    ));

    if let Err(e) = launch_result {
        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&inner_script_path);
        return Err(e);
    }

    // Poll for result file with timeout
    let poll_result = timeout(
        Duration::from_secs(timeout_secs),
        poll_for_result(&result_path),
    )
    .await;

    // Clean up script files (inner script is also cleaned by the PS script itself,
    // but remove here as a safety net in case the elevated process was killed)
    let _ = std::fs::remove_file(&script_path);
    let _ = std::fs::remove_file(&inner_script_path);

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

#[cfg(target_os = "macos")]
pub async fn run_elevated_shell(
    command: &str,
    cwd: &std::path::Path,
    env: &[(String, String)],
    timeout_secs: u64,
) -> Result<ElevatedResult> {
    let paths = write_unix_wrapper(command, cwd, env)?;
    let shell_cmd = format!("/bin/sh {}", shell_quote(&paths.script_path.to_string_lossy()));
    let script = format!(
        "do shell script \"{}\" with administrator privileges",
        apple_script_escape(&shell_cmd)
    );

    let result = timeout(
        Duration::from_secs(timeout_secs),
        Command::new("osascript").args(["-e", &script]).output(),
    )
    .await;

    finalize_unix_result(paths, result, "macOS administrator prompt", timeout_secs)
}

#[cfg(target_os = "linux")]
pub async fn run_elevated_shell(
    command: &str,
    cwd: &std::path::Path,
    env: &[(String, String)],
    timeout_secs: u64,
) -> Result<ElevatedResult> {
    if !command_exists("pkexec") {
        return Err(anyhow::anyhow!(
            "pkexec is not available. Install polkit/pkexec or rerun the command manually with sudo in a terminal."
        ));
    }

    let paths = write_unix_wrapper(command, cwd, env)?;
    let result = timeout(
        Duration::from_secs(timeout_secs),
        Command::new("pkexec")
            .args(["/bin/sh", &paths.script_path.to_string_lossy()])
            .output(),
    )
    .await;

    finalize_unix_result(paths, result, "polkit authentication", timeout_secs)
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
pub async fn run_elevated_shell(
    _command: &str,
    _cwd: &std::path::Path,
    _env: &[(String, String)],
    _timeout_secs: u64,
) -> Result<ElevatedResult> {
    Err(anyhow::anyhow!(
        "Elevated shell execution is not implemented for this platform"
    ))
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

#[cfg(not(target_os = "windows"))]
fn write_unix_wrapper(
    command: &str,
    cwd: &std::path::Path,
    env: &[(String, String)],
) -> Result<ElevatedPaths> {
    let tmp_dir = std::env::temp_dir();
    let id = uuid::Uuid::new_v4().simple().to_string();
    let script_path = tmp_dir.join(format!("pisci_elev_{}.sh", id));
    let result_path = tmp_dir.join(format!("pisci_elev_{}.exit", id));
    let stdout_path = tmp_dir.join(format!("pisci_elev_{}.out", id));
    let stderr_path = tmp_dir.join(format!("pisci_elev_{}.err", id));

    let exports = env
        .iter()
        .map(|(key, value)| format!("export {}={}\n", key, shell_quote(value)))
        .collect::<String>();

    let script = format!(
        "#!/bin/sh\ncd {} || exit 1\n{} /bin/sh -lc {} > {} 2> {}\nprintf '%s' $? > {}\n",
        shell_quote(&cwd.to_string_lossy()),
        exports,
        shell_quote(command),
        shell_quote(&stdout_path.to_string_lossy()),
        shell_quote(&stderr_path.to_string_lossy()),
        shell_quote(&result_path.to_string_lossy()),
    );

    std::fs::write(&script_path, script.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path)?.permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(&script_path, perms)?;
    }

    Ok(ElevatedPaths {
        script_path,
        result_path,
        stdout_path,
        stderr_path,
    })
}

#[cfg(not(target_os = "windows"))]
fn finalize_unix_result(
    paths: ElevatedPaths,
    launch_result: Result<Result<std::process::Output, std::io::Error>, tokio::time::error::Elapsed>,
    prompt_name: &str,
    timeout_secs: u64,
) -> Result<ElevatedResult> {
    let result = match launch_result {
        Err(_) => {
            cleanup_unix_paths(&paths);
            return Err(anyhow::anyhow!(
                "Elevated command timed out after {}s while waiting for {}",
                timeout_secs,
                prompt_name
            ));
        }
        Ok(Err(e)) => {
            cleanup_unix_paths(&paths);
            return Err(anyhow::anyhow!("Failed to start elevated command: {}", e));
        }
        Ok(Ok(output)) => output,
    };

    if !paths.result_path.exists() {
        let stderr = String::from_utf8_lossy(&result.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&result.stdout).trim().to_string();
        cleanup_unix_paths(&paths);
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(anyhow::anyhow!(
            "Elevated command did not complete. The user may have cancelled authentication or the system could not show the privilege prompt. {}",
            detail
        ));
    }

    let exit_code = std::fs::read_to_string(&paths.result_path)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(-1);
    let stdout = std::fs::read_to_string(&paths.stdout_path).unwrap_or_default();
    let stderr = std::fs::read_to_string(&paths.stderr_path).unwrap_or_default();
    cleanup_unix_paths(&paths);

    Ok(ElevatedResult {
        exit_code,
        stdout: stdout.trim().to_string(),
        stderr: stderr.trim().to_string(),
    })
}

#[cfg(not(target_os = "windows"))]
fn cleanup_unix_paths(paths: &ElevatedPaths) {
    let _ = std::fs::remove_file(&paths.script_path);
    let _ = std::fs::remove_file(&paths.result_path);
    let _ = std::fs::remove_file(&paths.stdout_path);
    let _ = std::fs::remove_file(&paths.stderr_path);
}

#[cfg(not(target_os = "windows"))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
fn apple_script_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(target_os = "linux")]
fn command_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn parse_result(json_str: &str) -> Result<ElevatedResult> {
    // Strip UTF-8 BOM (U+FEFF) that Windows WriteAllText with UTF8 encoding emits,
    // then also strip whitespace. serde_json rejects any leading non-JSON bytes.
    let stripped = json_str.trim_start_matches('\u{FEFF}').trim();
    let v: serde_json::Value = serde_json::from_str(stripped).map_err(|e| {
        anyhow::anyhow!("Failed to parse elevated result: {} | raw: {}", e, json_str)
    })?;

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
