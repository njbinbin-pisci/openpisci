use crate::browser::download;
use crate::host::DesktopHostTools;
use crate::store::{AppState, Settings};
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SystemDependencyItem {
    pub key: String,
    pub name: String,
    pub feature: String,
    pub available: bool,
    pub required: bool,
    pub status: String,
    pub details: Option<String>,
    pub hint: String,
    pub remediation: Option<String>,
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

/// Check platform-specific feature dependencies that affect desktop automation,
/// Windows integrations, and host runtime behavior.
#[tauri::command]
pub async fn check_system_dependencies(
    state: State<'_, AppState>,
) -> Result<Vec<SystemDependencyItem>, String> {
    let settings = state.settings.lock().await.clone();
    Ok(collect_system_dependencies(&settings))
}

pub fn collect_system_dependencies(settings: &Settings) -> Vec<SystemDependencyItem> {
    let tool_enabled = |tool_name: &str| settings.builtin_tool_enabled.get(tool_name).copied().unwrap_or(true);

    let mut items = Vec::new();

    #[cfg(target_os = "linux")]
    {
        let desktop_enabled = tool_enabled("desktop_automation");
        let session_type = std::env::var("XDG_SESSION_TYPE")
            .unwrap_or_else(|_| "unknown".to_string())
            .to_lowercase();
        let x11 = session_type == "x11" || session_type.is_empty();

        items.push(build_dependency_item(
            "linux-session",
            "Display Session (X11)",
            "desktop_automation",
            x11,
            desktop_enabled,
            Some(format!("Current session: {}", if session_type.is_empty() { "unknown" } else { &session_type })),
            if x11 {
                "Desktop automation is running on an X11-compatible session."
            } else {
                "Wayland sessions often block synthetic mouse/keyboard input and window control."
            },
            Some("Use an X11 session for reliable desktop automation, or expect limited support under Wayland."),
        ));

        items.push(build_dependency_item(
            "xdotool",
            "xdotool",
            "desktop_automation",
            command_exists("xdotool"),
            desktop_enabled,
            None,
            "Mouse, keyboard, cursor position, dragging, and scroll automation on Linux.",
            Some("Install xdotool from your distro packages, e.g. `sudo apt install xdotool`."),
        ));

        items.push(build_dependency_item(
            "wmctrl",
            "wmctrl",
            "desktop_automation",
            command_exists("wmctrl"),
            desktop_enabled,
            None,
            "Window listing and window activation for desktop_automation on Linux.",
            Some("Install wmctrl from your distro packages, e.g. `sudo apt install wmctrl`."),
        ));

        items.push(build_dependency_item(
            "xclip",
            "xclip",
            "desktop_automation",
            command_exists("xclip"),
            false,
            None,
            "Recommended for reliable clipboard-based text input; desktop_automation falls back to xdotool typing when missing.",
            Some("Install xclip for better text entry reliability, e.g. `sudo apt install xclip`."),
        ));
    }

    #[cfg(target_os = "macos")]
    {
        let desktop_enabled = tool_enabled("desktop_automation");
        items.push(build_dependency_item(
            "cliclick",
            "cliclick",
            "desktop_automation",
            command_exists("cliclick"),
            desktop_enabled,
            None,
            "Mouse, keyboard, drag, and click automation on macOS.",
            Some("Install cliclick with Homebrew: `brew install cliclick`."),
        ));

        items.push(build_dependency_item(
            "osascript",
            "osascript",
            "desktop_automation",
            command_exists("osascript"),
            desktop_enabled,
            None,
            "Used for window listing, activation, and some fallback automation on macOS.",
            Some("osascript ships with macOS. If unavailable, check shell PATH / system integrity."),
        ));

        let accessibility = probe_command(
            "osascript",
            &[
                "-e",
                "tell application \"System Events\" to return UI elements enabled",
            ],
        );
        let accessibility_ok = accessibility
            .as_deref()
            .map(|v| v.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        items.push(build_dependency_item(
            "macos-accessibility",
            "Accessibility Permission",
            "desktop_automation",
            accessibility_ok,
            desktop_enabled,
            accessibility.map(|v| format!("System Events returned: {}", v)),
            "Required for controlling other apps via System Events / synthetic input.",
            Some("Grant Accessibility access to OpenPisci / terminal in System Settings → Privacy & Security → Accessibility."),
        ));
    }

    #[cfg(target_os = "windows")]
    {
        let desktop_enabled = tool_enabled("desktop_automation");
        let powershell_needed = desktop_enabled
            || tool_enabled("powershell_query")
            || tool_enabled("office")
            || tool_enabled("wmi");
        let powershell = command_exists("powershell") || command_exists("pwsh");

        items.push(build_dependency_item(
            "powershell",
            "PowerShell",
            "windows_integration",
            powershell,
            powershell_needed,
            probe_command("powershell", &["-NoProfile", "-Command", "$PSVersionTable.PSVersion.ToString()"])
                .or_else(|| probe_command("pwsh", &["-NoProfile", "-Command", "$PSVersionTable.PSVersion.ToString()"])),
            "Required by powershell_query and used by Windows desktop automation / Office helpers.",
            Some("Install Windows PowerShell / PowerShell 7 and ensure `powershell` or `pwsh` is available on PATH."),
        ));

        if tool_enabled("uia") {
            items.push(build_dependency_item(
                "uia-runtime",
                "Windows UI Automation Runtime",
                "desktop_automation",
                true,
                true,
                Some("Built into the Windows desktop host".into()),
                "UIA support is compiled into the Windows build; failures are usually app-specific or permission-related.",
                Some("If UIA actions fail, try running the app with matching privilege level and verify the target app exposes UIA elements."),
            ));
        }

        if tool_enabled("wmi") {
            let wmi_status = probe_command(
                "powershell",
                &[
                    "-NoProfile",
                    "-Command",
                    "$svc=Get-Service Winmgmt -ErrorAction SilentlyContinue; if ($svc) { $svc.Status }",
                ],
            )
            .or_else(|| {
                probe_command(
                    "pwsh",
                    &[
                        "-NoProfile",
                        "-Command",
                        "$svc=Get-Service Winmgmt -ErrorAction SilentlyContinue; if ($svc) { $svc.Status }",
                    ],
                )
            });
            items.push(build_dependency_item(
                "wmi-service",
                "WMI Service (Winmgmt)",
                "windows_integration",
                wmi_status.is_some(),
                true,
                wmi_status.clone().map(|v| format!("Service status: {}", v)),
                "Needed for the WMI tool to query hardware, processes, and services.",
                Some("Ensure the Windows Management Instrumentation service exists and can be started."),
            ));
        }

        if tool_enabled("office") {
            let office = probe_command(
                "powershell",
                &[
                    "-NoProfile",
                    "-Command",
                    "$paths=@('HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\excel.exe','HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\winword.exe','HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\powerpnt.exe'); if (($paths | Where-Object { Test-Path $_ }).Count -gt 0) { 'installed' }",
                ],
            )
            .or_else(|| {
                probe_command(
                    "pwsh",
                    &[
                        "-NoProfile",
                        "-Command",
                        "$paths=@('HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\excel.exe','HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\winword.exe','HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\powerpnt.exe'); if (($paths | Where-Object { Test-Path $_ }).Count -gt 0) { 'installed' }",
                    ],
                )
            });
            items.push(build_dependency_item(
                "office-installation",
                "Microsoft Office",
                "office",
                office.is_some(),
                true,
                office.map(|v| format!("Registry probe: {}", v)),
                "Required for Excel / Word / PowerPoint / Outlook COM automation.",
                Some("Install Microsoft Office desktop apps on this machine before enabling the Office tool."),
            ));
        }
    }

    items
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

fn build_dependency_item(
    key: &str,
    name: &str,
    feature: &str,
    available: bool,
    required: bool,
    details: Option<String>,
    hint: &str,
    remediation: Option<&str>,
) -> SystemDependencyItem {
    let status = if available {
        "ok"
    } else if required {
        "missing"
    } else {
        "warning"
    };

    SystemDependencyItem {
        key: key.to_string(),
        name: name.to_string(),
        feature: feature.to_string(),
        available,
        required,
        status: status.to_string(),
        details,
        hint: hint.to_string(),
        remediation: remediation.map(|s| s.to_string()),
    }
}

fn command_exists(cmd: &str) -> bool {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("where");
        command.arg(cmd);
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
        command
    };

    #[cfg(not(target_os = "windows"))]
    let mut command = {
        let mut command = std::process::Command::new("which");
        command.arg(cmd);
        command
    };

    command
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
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
