/// Chrome for Testing auto-download manager.
/// Downloads chrome-headless-shell from the official JSON API.
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::info;

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
    // Prefer full chrome (better CDP compatibility) over chrome-headless-shell
    #[cfg(target_os = "windows")]
    let full_name = "chrome.exe";
    #[cfg(not(target_os = "windows"))]
    let full_name = "chrome";

    // Walk subdirs (Chrome for Testing extracts into a versioned subdirectory)
    fn find_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
        let direct = dir.join(name);
        if direct.exists() { return Some(direct); }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    let candidate = p.join(name);
                    if candidate.exists() { return Some(candidate); }
                }
            }
        }
        None
    }

    if let Some(p) = find_in_dir(chrome_dir, full_name) {
        return Some(p);
    }
    // Fallback: chrome-headless-shell
    if let Some(p) = find_in_dir(chrome_dir, chrome_exe_name()) {
        return Some(p);
    }
    None
}

/// Try to find a Chromium-based browser installed on the system.
/// NOTE: Microsoft Edge is intentionally excluded — its CDP implementation is
/// incompatible with chromiumoxide (WebSocket messages fail to deserialize).
/// Priority on Windows: Chrome → Chrome Beta/Dev/Canary → Brave
pub fn find_system_chrome() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        // Google Chrome (most compatible with chromiumoxide/CDP)
        let chrome_candidates = [
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files\Google\Chrome Beta\Application\chrome.exe",
            r"C:\Program Files\Google\Chrome Dev\Application\chrome.exe",
            r"C:\Program Files\Google\Chrome Canary\Application\chrome.exe",
        ];
        for path in &chrome_candidates {
            let p = PathBuf::from(path);
            if p.exists() {
                info!("Found system Chrome: {}", p.display());
                return Some(p);
            }
        }
        // Per-user Chrome (LOCALAPPDATA)
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let p = PathBuf::from(&local).join(r"Google\Chrome\Application\chrome.exe");
            if p.exists() {
                info!("Found user-installed Chrome: {}", p.display());
                return Some(p);
            }
        }
        // Brave browser (Chromium-based, CDP compatible)
        let brave_candidates = [
            r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
            r"C:\Program Files (x86)\BraveSoftware\Brave-Browser\Application\brave.exe",
        ];
        for path in &brave_candidates {
            let p = PathBuf::from(path);
            if p.exists() {
                info!("Found Brave browser: {}", p.display());
                return Some(p);
            }
        }
        // Edge is intentionally last and skipped — its CDP is incompatible with chromiumoxide.
        // Uncomment only if chromiumoxide adds Edge support.
        // let edge_candidates = [...];
    }
    #[cfg(target_os = "macos")]
    {
        let candidates = [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            // Edge excluded on macOS too for the same CDP compatibility reason
        ];
        for path in &candidates {
            let p = PathBuf::from(path);
            if p.exists() {
                return Some(p);
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        for name in &["microsoft-edge", "google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "brave-browser"] {
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

    // Prefer full "chrome" binary over "chrome-headless-shell":
    // chrome-headless-shell lacks full CDP support and causes "error decoding response body"
    // with chromiumoxide. Fall back to chrome-headless-shell only if chrome is unavailable.
    let download_url = resp["channels"]["Stable"]["downloads"]["chrome"]
        .as_array()
        .and_then(|arr| arr.iter().find(|item| item["platform"].as_str() == Some(platform)))
        .and_then(|item| item["url"].as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            resp["channels"]["Stable"]["downloads"]["chrome-headless-shell"]
                .as_array()
                .and_then(|arr| arr.iter().find(|item| item["platform"].as_str() == Some(platform)))
                .and_then(|item| item["url"].as_str())
                .map(|s| s.to_string())
        })
        .ok_or_else(|| anyhow::anyhow!("Could not find chrome download URL for platform: {}", platform))?;

    let version = resp["channels"]["Stable"]["version"]
        .as_str()
        .unwrap_or("unknown");

    info!("Downloading Chrome for Testing {} ({})...", version, platform);
    info!("URL: {}", download_url);

    std::fs::create_dir_all(dest_dir)
        .context("Failed to create Chrome download directory")?;

    // Download the zip
    let zip_path = dest_dir.join("chrome-for-testing.zip");
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
    // Search for full chrome first (preferred), then chrome-headless-shell as fallback
    #[cfg(target_os = "windows")]
    let candidates = ["chrome.exe", "chrome-headless-shell.exe"];
    #[cfg(not(target_os = "windows"))]
    let candidates = ["chrome", "chrome-headless-shell"];

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

    for name in &candidates {
        if let Some(found) = walk(dir, name) {
            return Some(found);
        }
    }
    None
}
