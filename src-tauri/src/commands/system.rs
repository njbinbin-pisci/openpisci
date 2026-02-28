use crate::store::AppState;
use crate::tools;
use serde::Serialize;
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

/// Returns the current execution mode (always "host" in this Rust implementation)
#[tauri::command]
pub async fn get_vm_status() -> Result<VmStatus, String> {
    #[cfg(target_os = "windows")]
    {
        // Check if Windows Sandbox is available
        let sandbox_available = std::path::Path::new(
            r"C:\Windows\System32\WindowsSandbox.exe"
        ).exists();

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
pub async fn get_runtime_capabilities(state: State<'_, AppState>) -> Result<RuntimeCapabilities, String> {
    let vm = get_vm_status().await?;

    let settings = state.settings.lock().await.clone();
    let registry = tools::build_registry(state.browser.clone(), None, None);
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
