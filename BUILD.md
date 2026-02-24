# Pisci Desktop — Build Guide

## Windows Build (Recommended)

### Prerequisites

1. **Rust** (stable): https://rustup.rs/
2. **Node.js** 18+: https://nodejs.org/
3. **Visual Studio Build Tools** (C++ workload):
   ```
   winget install Microsoft.VisualStudio.2022.BuildTools
   ```
4. **WebView2** (pre-installed on Windows 11, or download from Microsoft)

### Quick Build

```powershell
# Clone the repository
git clone http://192.168.31.100:9980/njbinbin/piscidesktop.git
cd piscidesktop

# Install frontend dependencies
npm install

# Development mode (hot reload)
npm run tauri dev

# Production build (generates NSIS installer)
npm run tauri build
```

The installer will be at:
```
src-tauri/target/release/bundle/nsis/Pisci_0.1.0_x64-setup.exe
```

### Size Optimization

The `Cargo.toml` is configured for minimal binary size:
```toml
[profile.release]
codegen-units = 1
lto = true
opt-level = "s"   # optimize for size
panic = "abort"
strip = true      # strip debug symbols
```

Expected installer size: **15-25 MB** (WebView2 is a Windows system component, not bundled).

## Linux Build (Development Only)

Linux builds require additional system packages:

```bash
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev \
  libgtk-3-dev \
  libssl-dev \
  pkg-config \
  libsoup-3.0-dev \
  libjavascriptcoregtk-4.1-dev \
  librsvg2-dev

npm install
npm run tauri dev
```

Note: Windows-specific tools (UIA, screen capture) are disabled on Linux via `#[cfg(target_os = "windows")]`.

## First-Run Experience

On first launch, Pisci detects if no API key is configured and shows the **Onboarding Wizard**:

1. Welcome screen
2. API key configuration (Anthropic Claude or OpenAI GPT)
3. Workspace directory selection
4. Ready to chat

The wizard can be re-triggered by clearing `%APPDATA%\com.pisci.desktop\config.json`.

## Signing (Optional)

For distribution, sign the installer with a code signing certificate:

```powershell
# In tauri.conf.json, set:
# "certificateThumbprint": "YOUR_CERT_THUMBPRINT"
# "timestampUrl": "http://timestamp.digicert.com"
```
