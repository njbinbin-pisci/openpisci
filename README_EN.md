# 🐟 OpenPisci

**Open-source Windows AI Agent Desktop**

OpenPisci is a local-first AI Agent desktop application for Windows, built with Tauri 2 + Rust + React. The main agent is **Pisci** (大鱼 / Big Fish), and user-defined sub-agents are called **Fish** (小鱼 / Small Fish).

[中文](./README.md) | English

---

## ✨ Key Features

### 🤖 Powerful Agent Capabilities
- **Multi-LLM support**: Claude (Anthropic), GPT (OpenAI), DeepSeek, Qwen, and any OpenAI-compatible endpoint
- **Automatic memory extraction**: After each conversation, an LLM pass extracts 0–3 key facts and stores them as long-term memories; relevant memories are injected automatically in future sessions
- **Active memory**: The agent can call the `memory_store` tool mid-conversation to save important information
- **Task decomposition**: Complex tasks are broken down and executed step-by-step via HostAgent
- **Crash recovery**: Checkpoints are written every iteration; the agent resumes from the last checkpoint after a crash
- **Heartbeat mechanism**: Configurable periodic heartbeat for proactive task checking

### 🛠️ Rich Windows Toolset

| Tool | Description |
|------|-------------|
| `file_read` / `file_write` | Read and write files |
| `shell` / `powershell` | PowerShell command execution |
| `powershell_query` | Structured queries for processes, services, registry, etc. |
| `wmi` | WMI/WQL queries for hardware and system information |
| `web_search` | Parallel multi-engine search (DuckDuckGo, Bing, Baidu, 360); results merged and deduplicated |
| `browser` | Chrome browser automation via CDP |
| `uia` | Windows UI Automation — control any desktop application |
| `screen_capture` | Screenshots (full screen / window / region), with optional Vision AI analysis |
| `com` | Clipboard read/write, file association open, special folder paths |
| `office` | Automate Word, Excel, Outlook via COM |
| `email` | Send/receive email (SMTP/IMAP) |
| `memory_store` | Write information to long-term memory |
| User-defined tools | TypeScript plugins with custom configuration interfaces |

### 🐠 Fish (小鱼) Sub-Agent System
- Define custom sub-agents via `FISH.toml` with their own persona, tool permissions, and configuration
- Built-in "File Assistant" Fish for file management tasks
- User Fish definitions live in `%APPDATA%\com.pisci.desktop\fish\`
- Each Fish runs in its own isolated session with a dynamically rendered config form

### ⚡ Skills System
- Skills are defined in `SKILL.md` format: YAML frontmatter (name, description, tool list, etc.) + Markdown body (instructions)
- Skill content is injected into the system prompt on every agent call, guiding the agent to use specific tools and workflows
- Install skills from a URL or local path
- Built-in skills: Office Automation, File Management, Web Automation, System Administration, Desktop Control

> **Note**: SKILL.md is OpenPisci's own skill format. It is **not** the same as Anthropic's MCP (Model Context Protocol) — they are two separate specifications.

### 📱 Multi-Platform IM Gateway

| Platform | Mode |
|----------|------|
| Feishu / Lark | Polling inbound + outbound reply |
| WeCom (Enterprise WeChat) | Local relay inbound + outbound reply |
| DingTalk | Polling inbound + outbound reply |
| Telegram | Long-polling inbound + outbound reply |
| Slack | Outbound webhook |
| Discord | Outbound webhook |
| Microsoft Teams | Outbound webhook |
| Matrix | Outbound send |
| Generic Webhook | Outbound webhook |

> IM messages and the Agent communicate bidirectionally: each IM channel/user has its own persistent session with full message history.

### ⏰ Scheduled Tasks
- Cron expression scheduling
- Task history (run count, last execution time, status)
- Immediate trigger support

### 🔒 Security
- API keys encrypted with ChaCha20Poly1305
- Three policy modes: Strict / Balanced / Dev
- Prompt injection detection (v2)
- Tool call rate limiting
- Dangerous operation confirmation

### 🎨 UI Features
- Minimal mode: floating HUD panel, tool calls shown as toast notifications
- Two themes: Violet / Black-Gold
- Window border color dynamically matches the active theme (Windows 11+)
- Chinese / English internationalization

---

## 🚀 Quick Start

### Requirements

- Windows 10 / 11 (64-bit)
- WebView2 Runtime (pre-installed on Windows 11; download from [Microsoft](https://developer.microsoft.com/microsoft-edge/webview2/) for Windows 10)

### Download

Go to [Releases](https://github.com/njbinbin-pisci/openpisci/releases) and download the latest installer (`.exe`).

### First-time Setup

1. Launch the app and follow the setup wizard
2. Choose your LLM provider and enter your API key
3. Set your workspace directory (the default root for file operations)
4. Start chatting

---

## 🔧 Development Setup

### Prerequisites

- [Rust](https://rustup.rs/) stable (≥ 1.77.2)
- [Node.js](https://nodejs.org/) 20 LTS
- [Visual Studio 2022 Build Tools](https://visualstudio.microsoft.com/downloads/) (Desktop C++ workload)

### Clone & Run

```bash
git clone https://github.com/njbinbin-pisci/openpisci.git
cd openpisci

# Install frontend dependencies
npm install

# Development mode (hot reload)
npm run tauri dev

# Build release
npm run tauri build
```

### Regenerate Icons

```bash
npm run icon:emoji
```

---

## 🐠 Creating a Custom Fish

Create `%APPDATA%\com.pisci.desktop\fish\my-fish\FISH.toml`:

```toml
id = "my-fish"
name = "My Fish"
description = "An assistant focused on a specific task"
icon = "🐡"
tools = ["file_read", "shell", "memory_store"]

[agent]
system_prompt = "You are a fish that specializes in..."
max_iterations = 20
model = "default"

[[settings]]
key = "workspace"
label = "Working Directory"
setting_type = "text"
default = ""
placeholder = "e.g. C:\\Users\\YourName\\Documents"
```

Restart the app and the new Fish will appear on the Fish page.

---

## ⚡ Creating a Custom Skill

Create `%APPDATA%\com.pisci.desktop\skills\my-skill\SKILL.md`:

```markdown
---
name: My Skill
description: What this skill does
version: "1.0"
tools:
  - file_read
  - shell
---

# My Skill

## Instructions

When the user needs to..., follow these steps:
1. First...
2. Then...
```

---

## 🔧 User-Defined Tools

Install TypeScript plugins from the Tools page. Each plugin can declare its own configuration interface (e.g. SMTP credentials, API keys).

User tools are stored in: `%APPDATA%\com.pisci.desktop\user-tools\`

---

## 📁 Data Directories

| Path | Contents |
|------|----------|
| `%APPDATA%\com.pisci.desktop\` | Config, database |
| `%APPDATA%\com.pisci.desktop\skills\` | Skills directory |
| `%APPDATA%\com.pisci.desktop\fish\` | User-defined Fish |
| `%APPDATA%\com.pisci.desktop\user-tools\` | User-defined tools |
| `%LOCALAPPDATA%\pisci\logs\` | Logs and crash reports |

---

## 🏗️ Architecture

```
OpenPisci
├── src-tauri/          # Rust backend
│   ├── src/
│   │   ├── agent/      # Agent loop, HostAgent, message management
│   │   ├── commands/   # Tauri IPC command layer
│   │   ├── fish/       # Fish sub-agent system
│   │   ├── gateway/    # IM gateways (Feishu, DingTalk, Telegram, etc.)
│   │   ├── llm/        # LLM clients (Claude, OpenAI, DeepSeek, Qwen)
│   │   ├── memory/     # Memory system (vector search, FTS)
│   │   ├── policy/     # Policy gate, injection detection
│   │   ├── scheduler/  # Cron scheduler
│   │   ├── security/   # Encryption, key management
│   │   ├── skills/     # Skill loader (SKILL.md format)
│   │   ├── store/      # SQLite database, settings persistence
│   │   └── tools/      # Tool implementations
│   └── Cargo.toml
└── src/                # React frontend
    ├── components/     # Page components
    ├── i18n/           # Chinese / English translations
    ├── services/       # Tauri IPC service layer
    └── store/          # Redux state management
```

---

## 🤝 Acknowledgements

OpenPisci draws inspiration from these excellent open-source projects:

- [OpenClaw](https://github.com/mariozechner/openclaw) — Cross-platform AI Agent; pi-agent architecture reference
- [OpenFang](https://github.com/RightNow-AI/openfang) — Rust + Tauri Agent OS; Loop Guard and Hand system reference
- [LobsterAI](https://github.com/lobsterai/lobsterai) — Claude Agent SDK integration reference

---

## 📄 License

[MIT License](./LICENSE)

---

<p align="center">Built with ❤️ by the OpenPisci community</p>
