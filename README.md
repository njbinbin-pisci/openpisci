# Pisci Desktop

A Windows AI Agent desktop application built with Rust (Tauri) + React.

## Features

- 💬 **Chat** — Streaming AI conversations with Claude/GPT
- 🧠 **Memory** — Persistent user preferences extracted from conversations
- ⚡ **Skills** — Toggleable tool capabilities
- ⏰ **Scheduler** — Cron-based recurring tasks
- ⚙️ **Settings** — API key configuration with first-run onboarding
- 🔒 **Policy Gate** — File path and command security validation
- 🖥️ **Windows UIA** — Native Windows UI Automation (Windows only)
- 📸 **Screen Vision** — Screenshot + Vision AI fallback (Windows only)

## Architecture

```
Tauri (single process)
├── WebView2 Frontend (React + TypeScript)
│   ├── Chat UI (streaming via Tauri events)
│   ├── Memory / Skills / Scheduler / Settings pages
│   └── Onboarding wizard (first-run API key setup)
└── Rust Backend (Tauri Commands)
    ├── Agent Loop (async, cancellable)
    ├── LLM Client (Claude / OpenAI, streaming SSE)
    ├── Tool Registry (shell, file, web, UIA, screen)
    ├── Policy Gate (path whitelist + command blacklist)
    └── SQLite (rusqlite, bundled)
```

## Building (Windows)

### Prerequisites

- Windows 10/11 (64-bit)
- [Rust](https://rustup.rs/) (stable)
- [Node.js](https://nodejs.org/) 18+
- [WebView2](https://developer.microsoft.com/en-us/microsoft-edge/webview2/) (pre-installed on Windows 11)
- Visual Studio Build Tools (C++ workload)

### Development

```powershell
# Install dependencies
npm install

# Run in development mode
npm run tauri dev
```

### Release Build

```powershell
# Build installer
npm run tauri build

# Output: src-tauri/target/release/bundle/nsis/Pisci_0.1.0_x64-setup.exe
```

## Configuration

On first launch, Pisci shows an onboarding wizard to configure:
- AI provider (Anthropic Claude or OpenAI GPT)
- API key
- Workspace directory (files are restricted to this path)

Configuration is stored in `%APPDATA%\com.pisci.desktop\config.json`.

## Security

- **Policy Gate**: All file operations are restricted to the workspace root
- **Command Blacklist**: Dangerous shell commands (format, rm -rf /, etc.) are blocked
- **User Confirmation**: Shell commands and file writes require user approval (configurable)
- **No VM/Sandbox**: Runs directly on host for simplicity; Policy Gate provides security

## Project Structure

```
piscidesktop/
├── src-tauri/              # Rust backend
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs         # Tauri entry point
│       ├── agent/          # Agent loop, messages, tool trait
│       ├── commands/       # Tauri IPC commands
│       ├── llm/            # Claude + OpenAI clients
│       ├── policy/         # Policy Gate
│       ├── scheduler/      # Cron scheduler
│       ├── store/          # SQLite + settings
│       └── tools/          # Shell, file, web, UIA, screen tools
├── src/                    # React frontend
│   ├── App.tsx
│   ├── components/         # Chat, Memory, Skills, Scheduler, Settings, Onboarding
│   ├── services/tauri.ts   # Tauri IPC client
│   └── store/              # Redux Toolkit
├── package.json
└── vite.config.ts
```

## Target Package Size

| Component | Size |
|-----------|------|
| Rust kernel (stripped release) | ~8-15 MB |
| WebView2 (Windows system component) | 0 MB |
| Frontend JS/CSS bundle | ~2 MB |
| **NSIS installer total** | **~15-25 MB** |
