use crate::browser::download;
use crate::host::DesktopHostTools;
use crate::store::AppState;
use serde::Serialize;
use std::collections::HashMap;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct VmStatus {
    pub backend: String,
    pub available: bool,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct RuntimeCapabilities {
    pub vm: VmStatus,
    pub tools: Vec<String>,
    pub channels: Vec<String>,
    pub configured_provider: String,
    pub workspace_root: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct RuntimeCheckItem {
    pub name: String,
    pub available: bool,
    pub version: Option<String>,
    pub download_url: String,
    pub hint: String,
}

/// Detect Node.js, npm, Python and other runtimes needed by skills.
/// `custom_paths` maps runtime key (e.g. "python") to an absolute executable path
/// supplied by the user when auto-detection fails.
#[tauri::command]
pub async fn check_runtimes(state: State<'_, AppState>) -> Result<Vec<RuntimeCheckItem>, String> {
    let custom_paths = {
        let settings = state.settings.lock().await;
        settings.runtime_paths.clone()
    };

    let mut items = Vec::new();

    // Node.js
    let node = probe_with_override("node", &["--version"], &custom_paths)
        .or_else(|| probe_command("node", &["--version"]));
    items.push(RuntimeCheckItem {
        name: "Node.js".into(),
        available: node.is_some(),
        version: node,
        download_url: "https://nodejs.org/en/download".into(),
        hint: "Required for npm-based skills".into(),
    });

    // npm (bundled with Node, but check separately)
    let npm = probe_with_override("npm", &["--version"], &custom_paths)
        .or_else(|| probe_command("npm", &["--version"]));
    items.push(RuntimeCheckItem {
        name: "npm".into(),
        available: npm.is_some(),
        version: npm,
        download_url: "https://nodejs.org/en/download".into(),
        hint: "Package manager for Node.js skills".into(),
    });

    // Python
    let python = probe_with_override("python", &["--version"], &custom_paths)
        .or_else(|| probe_command("python", &["--version"]))
        .or_else(|| probe_command("python3", &["--version"]));
    items.push(RuntimeCheckItem {
        name: "Python".into(),
        available: python.is_some(),
        version: python,
        download_url: "https://www.python.org/downloads/".into(),
        hint: "Required for Python-based skills".into(),
    });

    // pip
    let pip = probe_with_override("pip", &["--version"], &custom_paths)
        .or_else(|| probe_command("pip", &["--version"]))
        .or_else(|| probe_command("pip3", &["--version"]));
    items.push(RuntimeCheckItem {
        name: "pip".into(),
        available: pip.is_some(),
        version: pip,
        download_url: "https://pip.pypa.io/en/stable/installation/".into(),
        hint: "Package manager for Python skills".into(),
    });

    // Git (needed for some skill installs)
    let git = probe_with_override("git", &["--version"], &custom_paths)
        .or_else(|| probe_command("git", &["--version"]));
    items.push(RuntimeCheckItem {
        name: "Git".into(),
        available: git.is_some(),
        version: git,
        download_url: "https://git-scm.com/downloads".into(),
        hint: "Required for git-based skill sources".into(),
    });

    // Browser (Chrome/Brave for browser automation tool)
    // Check system Chrome/Brave first, then cached Chrome for Testing
    let chrome_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("com.pisci.desktop")
        .join("chrome");
    let (browser_available, browser_version, browser_hint) =
        if let Some(sys_path) = download::find_system_chrome() {
            let name = sys_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("chrome")
                .to_string();
            let ver = probe_command(sys_path.to_str().unwrap_or("chrome"), &["--version"])
                .unwrap_or_else(|| sys_path.to_string_lossy().to_string());
            (true, Some(ver), format!("System browser: {}", name))
        } else if let Some(cached) = download::chrome_exists(&chrome_dir) {
            let ver = probe_command(cached.to_str().unwrap_or(""), &["--version"])
                .unwrap_or_else(|| "Chrome for Testing (cached)".to_string());
            (
                true,
                Some(ver),
                "Chrome for Testing (cached, ~111 MB)".to_string(),
            )
        } else {
            (
                false,
                None,
                "No Chromium browser found. Chrome or Brave recommended. \
                 Chrome for Testing (~111 MB) will be auto-downloaded on first use."
                    .to_string(),
            )
        };
    items.push(RuntimeCheckItem {
        name: "Browser (Chrome/Brave)".into(),
        available: browser_available,
        version: browser_version,
        download_url: "https://www.google.com/chrome/".into(),
        hint: browser_hint,
    });

    Ok(items)
}

/// Save a user-specified runtime executable path and return updated check results.
#[tauri::command]
pub async fn set_runtime_path(
    state: State<'_, AppState>,
    runtime_key: String,
    exe_path: String,
) -> Result<Vec<RuntimeCheckItem>, String> {
    {
        let mut settings = state.settings.lock().await;
        if exe_path.is_empty() {
            settings.runtime_paths.remove(&runtime_key);
        } else {
            settings.runtime_paths.insert(runtime_key, exe_path);
        }
        settings.save().map_err(|e| e.to_string())?;
    }
    check_runtimes(state).await
}

/// Try to run the executable at the user-specified path (if any) for this runtime key.
fn probe_with_override(
    key: &str,
    args: &[&str],
    custom: &HashMap<String, String>,
) -> Option<String> {
    let path = custom.get(key)?;
    if path.is_empty() {
        return None;
    }
    probe_command(path, args)
}

fn probe_command(cmd: &str, args: &[&str]) -> Option<String> {
    let mut command = std::process::Command::new(cmd);
    command.args(args);
    // CREATE_NO_WINDOW: prevents a console window from flashing during runtime detection
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = command.output().ok()?;
    if output.status.success() {
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // Some tools (like Python) print to stderr
        let raw = if raw.is_empty() {
            String::from_utf8_lossy(&output.stderr).trim().to_string()
        } else {
            raw
        };
        Some(raw)
    } else {
        None
    }
}

/// Returns the current execution mode (always "host" in this Rust implementation)
#[tauri::command]
pub async fn get_vm_status() -> Result<VmStatus, String> {
    #[cfg(target_os = "windows")]
    {
        // Check if Windows Sandbox is available
        let sandbox_available =
            std::path::Path::new(r"C:\Windows\System32\WindowsSandbox.exe").exists();

        if sandbox_available {
            return Ok(VmStatus {
                backend: "windows_sandbox".into(),
                available: true,
                description: "Windows Sandbox available (optional isolation)".into(),
            });
        }
    }

    Ok(VmStatus {
        backend: "host".into(),
        available: true,
        description: "Running directly on host with Policy Gate security".into(),
    })
}

/// Runtime snapshot for parity tracking and diagnostics.
#[tauri::command]
pub async fn get_runtime_capabilities(
    state: State<'_, AppState>,
) -> Result<RuntimeCapabilities, String> {
    let vm = get_vm_status().await?;

    let settings = state.settings.lock().await.clone();
    let registry = DesktopHostTools {
        browser: Some(state.browser.clone()),
        builtin_tool_enabled: Some(settings.builtin_tool_enabled.clone()),
        ..Default::default()
    }
    .build_registry();
    let tools = registry
        .all()
        .iter()
        .map(|t| t.name().to_string())
        .collect::<Vec<_>>();

    let channels = state
        .gateway
        .list_channels()
        .await
        .into_iter()
        .map(|c| c.name)
        .collect::<Vec<_>>();

    Ok(RuntimeCapabilities {
        vm,
        tools,
        channels,
        configured_provider: settings.provider,
        workspace_root: settings.workspace_root,
    })
}
