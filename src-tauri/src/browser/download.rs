/// Chrome for Testing auto-download manager.
/// Downloads chrome-headless-shell from the official JSON API.
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

const API_URL: &str =
    "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";

/// Detect the current platform string used by Chrome for Testing API
fn platform_str() -> &'static str {
    #[cfg(target_os = "windows")]
    { "win64" }
    #[cfg(target_os = "macos")]
    {
        #[cfg(target_arch = "aarch64")]
        { "mac-arm64" }
        #[cfg(not(target_arch = "aarch64"))]
        { "mac-x64" }
    }
    #[cfg(target_os = "linux")]
    { "linux64" }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    { "linux64" }
}

/// Returns the expected chrome-headless-shell executable name
pub fn chrome_exe_name() -> &'static str {
    #[cfg(target_os = "windows")]
    { "chrome-headless-shell.exe" }
    #[cfg(not(target_os = "windows"))]
    { "chrome-headless-shell" }
}

/// Check if a Chrome executable already exists at the given path
pub fn chrome_exists(chrome_dir: &Path) -> Option<PathBuf> {
    // Check for chrome-headless-shell
    let headless = chrome_dir.join(chrome_exe_name());
    if headless.exists() {
        return Some(headless);
    }
    // Check for full chrome
    #[cfg(target_os = "windows")]
    let full = chrome_dir.join("chrome.exe");
    #[cfg(not(target_os = "windows"))]
    let full = chrome_dir.join("chrome");
    if full.exists() {
        return Some(full);
    }
    None
}

/// Try to find Chrome installed on the system
pub fn find_system_chrome() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let candidates = [
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files\Google\Chrome Beta\Application\chrome.exe",
        ];
        for path in &candidates {
            let p = PathBuf::from(path);
            if p.exists() {
                return Some(p);
            }
        }
        // Check LOCALAPPDATA
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let p = PathBuf::from(local).join("Google\\Chrome\\Application\\chrome.exe");
            if p.exists() {
                return Some(p);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let p = PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
        if p.exists() {
            return Some(p);
        }
    }
    #[cfg(target_os = "linux")]
    {
        for name in &["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"] {
            if let Ok(output) = std::process::Command::new("which").arg(name).output() {
                if output.status.success() {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !path.is_empty() {
                        return Some(PathBuf::from(path));
                    }
                }
            }
        }
    }
    None
}

/// Download chrome-headless-shell for the current platform into `dest_dir`.
/// Returns the path to the executable.
pub async fn download_chrome_for_testing(dest_dir: &Path) -> Result<PathBuf> {
    info!("Fetching Chrome for Testing version info from API...");

    let client = reqwest::Client::builder()
        .user_agent("Pisci-Desktop/0.1")
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let resp: serde_json::Value = client
        .get(API_URL)
        .send()
        .await
        .context("Failed to fetch Chrome for Testing API")?
        .json()
        .await
        .context("Failed to parse Chrome for Testing API response")?;

    let platform = platform_str();

    // Find the stable channel download URL for chrome-headless-shell
    let download_url = resp["channels"]["Stable"]["downloads"]["chrome-headless-shell"]
        .as_array()
        .and_then(|arr| {
            arr.iter().find(|item| item["platform"].as_str() == Some(platform))
        })
        .and_then(|item| item["url"].as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("Could not find chrome-headless-shell download URL for platform: {}", platform))?;

    let version = resp["channels"]["Stable"]["version"]
        .as_str()
        .unwrap_or("unknown");

    info!("Downloading Chrome for Testing {} ({})...", version, platform);
    info!("URL: {}", download_url);

    std::fs::create_dir_all(dest_dir)
        .context("Failed to create Chrome download directory")?;

    // Download the zip
    let zip_path = dest_dir.join("chrome-headless-shell.zip");
    let mut resp = client
        .get(&download_url)
        .send()
        .await
        .context("Failed to download Chrome for Testing")?;

    let total = resp.content_length().unwrap_or(0);
    let mut downloaded = 0u64;
    {
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::File::create(&zip_path).await?;
        while let Some(chunk) = resp.chunk().await? {
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;
            if total > 0 && downloaded % (5 * 1024 * 1024) == 0 {
                info!("Downloaded {}/{} MB", downloaded / 1024 / 1024, total / 1024 / 1024);
            }
        }
        file.flush().await?;
    }

    info!("Extracting Chrome for Testing...");
    extract_zip(&zip_path, dest_dir)?;

    // Remove zip after extraction
    let _ = std::fs::remove_file(&zip_path);

    // Find the executable in extracted contents
    let exe = find_extracted_exe(dest_dir)
        .ok_or_else(|| anyhow::anyhow!("Could not find chrome-headless-shell after extraction"))?;

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&exe)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&exe, perms)?;
    }

    info!("Chrome for Testing installed at: {}", exe.display());
    Ok(exe)
}

fn extract_zip(zip_path: &Path, dest_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = match entry.enclosed_name() {
            Some(p) => p.to_owned(),
            None => continue,
        };
        let out_path = dest_dir.join(&name);

        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out_file = std::fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out_file)?;
        }
    }
    Ok(())
}

fn find_extracted_exe(dir: &Path) -> Option<PathBuf> {
    let exe_name = chrome_exe_name();
    // Walk directory looking for the executable
    fn walk(dir: &Path, exe_name: &str) -> Option<PathBuf> {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(found) = walk(&path, exe_name) {
                        return Some(found);
                    }
                } else if path.file_name().and_then(|n| n.to_str()) == Some(exe_name) {
                    return Some(path);
                }
            }
        }
        None
    }
    walk(dir, exe_name)
}
