# 🐟 OpenPisci

**Open-source Windows AI Agent Desktop**

OpenPisci is a local-first AI Agent desktop application for Windows, built with Tauri 2 + Rust + React. The main agent is **Pisci** (大鱼 / Big Fish), and user-defined sub-agents are called **Fish** (小鱼 / Small Fish).

[中文](./README.md) | English

---

## ✨ Key Features

### 🤖 Powerful Agent Capabilities
- **Multi-LLM support**: Claude (Anthropic), GPT (OpenAI), DeepSeek, Qwen, Zhipu, Kimi, MiniMax, and any OpenAI-compatible endpoint
- **Automatic memory extraction**: After each conversation, an LLM pass extracts 0–3 key facts and stores them as long-term memories; relevant memories are injected automatically in future sessions
- **Active memory**: The agent can call the `memory_store` tool mid-conversation to save important information
- **Task decomposition**: Complex tasks are broken down and executed step-by-step via HostAgent
- **Crash recovery**: Checkpoints are written every iteration; the agent resumes from the last checkpoint after a crash
- **Heartbeat mechanism**: Configurable periodic heartbeat for proactive task checking
- **Loop detection**: Four detectors (GenericRepeat / KnownPollNoProgress / PingPong / GlobalCircuitBreaker) prevent the agent from getting stuck in infinite loops

### 🛠️ Rich Windows Toolset

| Tool | Description |
|------|-------------|
| `file_read` / `file_write` | Read and write files (chunked reading for large files) |
| `file_edit` | Exact string replacement; supports `edits` array for atomic multi-location edits |
| `file_diff` | Preview unified diff before writing, or compare two files |
| `file_list` | Structured directory listing (JSON with size, modified date, type) |
| `file_search` | Glob search by name or grep search by content (supports `file_extensions` filter) |
| `code_run` | Coding-focused command runner with structured output and automatic error diagnosis |
| `shell` / `powershell_query` | PowerShell execution / structured system queries |
| `wmi` | WMI/WQL queries for hardware and system information |
| `web_search` | Parallel multi-engine search (DuckDuckGo, Bing, Baidu, 360); results merged and deduplicated |
| `browser` | Chrome browser automation via CDP |
| `uia` | Windows UI Automation — control any desktop application |
| `screen_capture` | Screenshots (full screen / window / region), with optional Vision AI analysis |
| `com` / `com_invoke` | COM/ActiveX object invocation (32-bit and 64-bit) |
| `office` | Automate Word, Excel, PowerPoint, Outlook via COM |
| `email` | Send/receive email (SMTP/IMAP) |
| `ssh` | SSH remote connection and command execution |
| `pdf` | PDF read/write |
| `memory_store` | Write information to long-term memory |
| `plan_todo` | Maintain a visible execution plan and todo state for complex tasks |
| User-defined tools | TypeScript plugins with custom configuration interfaces |
| MCP tools | Connect to external tool servers via the MCP protocol |

### 🐠 Fish (小鱼) Sub-Agent System
- Define custom sub-agents via `FISH.toml` with their own persona, tool permissions, and configuration
- Fish are **stateless, ephemeral workers**: the main Agent delegates sub-tasks via the `call_fish` tool; the Fish returns only the final result
- **Key benefit**: intermediate reasoning and tool calls inside the Fish do NOT pollute the main Agent's context, effectively saving context window budget
- User Fish definitions live in `%APPDATA%\com.pisci.desktop\fish\`
- Ideal for batch file processing, data collection, code scanning, and other multi-step tasks

### ⚡ Skills System
- Skills are defined in `SKILL.md` format: YAML frontmatter (name, description, tool list, etc.) + Markdown body (instructions)
- Skill content is injected into the system prompt on every agent call, guiding the agent to use specific tools and workflows
- Install skills from a URL or local path
- Built-in skills: Office Automation, File Management, Web Automation, System Administration, Desktop Control

> **Note**: SKILL.md is OpenPisci's own skill format. It is **not** the same as Anthropic's MCP (Model Context Protocol) — they are two separate specifications.

### 💻 Coding Capabilities (new in v0.3.0)
- **`code_run` tool**: Designed for coding tasks — returns structured `exit_code` / `stdout` / `stderr` / `duration_ms` and automatically diagnoses common Rust/Python/Node errors
- **`file_edit` batch edits**: `edits` array atomically applies multiple replacements in one call — validates all first, then writes once
- **`file_diff` tool**: Preview unified diff before applying changes, or compare two files — helps the agent self-verify edits
- **`file_search` enhancements**: Result limit raised to 500, new `file_extensions` filter, per-file grep limit raised to 200 KB
- **Coding workflow guidance**: System prompt includes a complete "understand → edit → verify → debug" loop

### 🔍 Context Preview (new in v0.3.0)
- Click the 🔍 button in the chat UI to inspect the exact message sequence that will be sent to the LLM on the next turn
- Structured display of each message's role and blocks (text / tool_use / tool_result), with collapsible tool calls and results
- Shows token usage vs. context budget with a progress bar, making context compression effects visible

### 🔗 Clickable File Links (new in v0.3.0)
- Local paths in LLM output (e.g. `C:\Users\...\file.md`) are automatically converted to clickable links
- Clicking opens the file or directory with the system's default application
- Supports Windows paths, UNC paths, Unix paths, and `file://` URIs

### 📱 Multi-Platform IM Gateway

| Platform | Mode |
|----------|------|
| Feishu / Lark | WebSocket long-connection inbound + outbound reply |
| WeCom (Enterprise WeChat) | Local relay inbound + outbound reply |
| DingTalk | Stream-mode WebSocket inbound + outbound reply |
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

> **⚠️ Security Warning**: OpenPisci is an AI Agent with high-privilege capabilities including file read/write, command execution, and UI automation. It is strongly recommended to run it inside a virtual machine (VMware, VirtualBox, Hyper-V) to prevent accidental damage to your host system. The developers are not responsible for any data loss or system damage caused by running it directly on a host machine.

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

Restart the app and the new Fish will appear on the Fish page. The main Agent will automatically delegate matching tasks to Fish via the `call_fish` tool.

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
│   │   ├── llm/        # LLM clients (Claude, OpenAI, DeepSeek, Qwen, etc.)
│   │   ├── memory/     # Memory system (vector search, FTS)
│   │   ├── policy/     # Policy gate, injection detection
│   │   ├── scheduler/  # Cron scheduler
│   │   ├── security/   # Encryption, key management
│   │   ├── skills/     # Skill loader (SKILL.md format)
│   │   ├── store/      # SQLite database, settings persistence
│   │   └── tools/      # Tool implementations (incl. code_run, file_diff)
│   └── Cargo.toml
└── src/                # React frontend
    ├── components/     # Page components
    ├── i18n/           # Chinese / English translations
    ├── services/       # Tauri IPC service layer
    └── store/          # Redux state management
```

---

## 📋 Changelog

### v0.4.1
- **New `plan_todo` tool**: the Agent can now maintain a Cursor-style visible task plan with `pending / in_progress / completed / cancelled` states during complex work
- **Real-time plan panel**: the chat UI now shows the current task plan live during execution and keeps it visible for review after completion
- **Planning prompt guidance**: the system prompt now includes a Planning section so the Agent proactively maintains short plans for multi-step tasks
- **More app controls exposed to the Agent**: theme switching, minimal mode, window movement, built-in tool toggles, and user tool configuration are now controllable via `app_control`

### v0.4.0
- **Stateless Fish refactor**: Fish sub-agents redesigned from session-based to stateless ephemeral workers; the main Agent delegates via `call_fish`, intermediate steps don't pollute the main context
- **Enhanced call_fish prompts**: System prompt now includes a Sub-Agent Delegation strategy section, guiding the main Agent to proactively use Fish for multi-step tasks
- **Unified confirmation dialogs**: New shared `ConfirmDialog` component replaces all `window.confirm()` calls (skill uninstall, tool uninstall, MCP delete, scheduled task delete, memory clear, audit log clear)
- **Skill loader fix**: Fixed installed skills being incorrectly classified as built-in, causing them not to appear in the UI

### v0.3.0
- **Coding capabilities**: New `code_run` tool (structured output + error diagnosis), `file_diff` tool (unified diff preview)
- **`file_edit` batch edits**: `edits` array for atomic multi-location edits in one call
- **`file_search` enhancements**: Result limit 500, new `file_extensions` filter, grep limit 200 KB per file
- **Context preview**: New 🔍 button in chat UI — inspect the exact message sequence sent to the LLM with token stats
- **Clickable file links**: Local paths in LLM output auto-converted to clickable links that open with the system default app

### v0.2.0
- Multimodal vision agent (screenshot + Vision AI)
- UIA precision test
- MCP / SSH / PDF tools
- Extended multi-LLM support (Zhipu, Kimi, MiniMax)

---

## 📄 License

[MIT License](./LICENSE)

---

<p align="center">Built with ❤️ by the OpenPisci community</p>
