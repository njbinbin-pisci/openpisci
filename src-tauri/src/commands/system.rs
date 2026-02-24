use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct VmStatus {
    pub backend: String,
    pub available: bool,
    pub description: String,
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
