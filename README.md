# 🐟 OpenPisci

**开源 Windows AI Agent 桌面应用**

OpenPisci 是一款运行在 Windows 桌面的本地优先 AI Agent，基于 Tauri 2 + Rust + React 构建。大鱼（Pisci）是主 Agent，小鱼（Fish）是用户自定义的专属子 Agent。

[English](./README_EN.md) | 中文

---

## ✨ 核心特性

### 🤖 强大的 Agent 能力
- **多 LLM 支持**：Claude（Anthropic）、GPT（OpenAI）、DeepSeek、通义千问（Qwen），以及任意 OpenAI 兼容接口
- **自动记忆**：对话结束后自动调用 LLM 提取关键信息存入长期记忆，下次对话自动注入相关上下文
- **主动记忆**：Agent 在对话中可主动调用 `memory_store` 工具保存重要信息
- **任务分解**：复杂任务自动分解为子任务并依次执行（HostAgent）
- **崩溃恢复**：每次迭代写入 checkpoint，程序崩溃后可从断点恢复
- **心跳机制**：可配置定时心跳，Agent 自主检查待处理任务

### 🛠️ 丰富的 Windows 工具集

| 工具 | 说明 |
|------|------|
| `file_read` / `file_write` | 文件读写 |
| `shell` / `powershell` | PowerShell 命令执行 |
| `powershell_query` | 结构化查询系统信息（进程、服务、注册表等） |
| `wmi` | WMI/WQL 查询硬件和系统信息 |
| `web_search` | 多引擎并行搜索（DuckDuckGo、Bing、百度、360），结果合并去重 |
| `browser` | Chrome 浏览器自动化（CDP 协议） |
| `uia` | Windows UI Automation — 控制任意桌面应用 |
| `screen_capture` | 截图（全屏/窗口/区域），支持 Vision AI 分析 |
| `com` | 剪贴板读写、文件关联打开、特殊目录路径 |
| `office` | 通过 COM 自动化 Word、Excel、Outlook |
| `email` | 发送/接收邮件（SMTP/IMAP） |
| `memory_store` | 向长期记忆写入信息 |
| 用户自定义工具 | TypeScript 插件，支持自定义配置接口 |

### 🐠 小鱼（Fish）子 Agent 系统
- 通过 `FISH.toml` 定义专属子 Agent，拥有独立人设、工具权限和配置
- 内置"文件助手"小鱼，专注文件管理任务
- 用户可在 `%APPDATA%\com.pisci.desktop\fish\` 目录放置自定义小鱼
- 激活后在独立会话中工作，支持配置表单动态渲染

### ⚡ 技能系统（Skills）
- 使用 `SKILL.md` 格式定义技能：YAML frontmatter（名称、描述、工具列表等）+ Markdown 正文（使用说明）
- 技能内容在每次 Agent 调用时自动注入系统提示词，引导 Agent 使用特定工具和流程
- 支持从 URL 或本地路径安装技能
- 内置技能：Office 自动化、文件管理、Web 自动化、系统管理、桌面控制

> **注意**：SKILL.md 是 OpenPisci 自定义的技能格式，与 Anthropic MCP（Model Context Protocol）是两套不同的规范。

### 📱 多平台 IM 网关

| 平台 | 模式 |
|------|------|
| 飞书（Feishu/Lark） | WebSocket 长连接收件 + 出站回复 |
| 企业微信（WeCom） | 本地中继收件 + 出站回复 |
| 钉钉（DingTalk） | Stream 模式 WebSocket 收件 + 出站回复 |
| Telegram | 长轮询收件 + 出站回复 |
| Slack | 出站 Webhook |
| Discord | 出站 Webhook |
| Microsoft Teams | 出站 Webhook |
| Matrix | 出站发送 |
| 通用 Webhook | 出站 Webhook |

> IM 消息与 Agent 双向通信：每个 IM 频道/用户拥有独立的持久会话，消息历史完整保留。

### ⏰ 定时任务
- Cron 表达式调度
- 任务历史记录（运行次数、最后执行时间、状态）
- 支持立即触发

### 🔒 安全机制
- API 密钥 ChaCha20Poly1305 加密存储
- 三种策略模式：Strict（严格）/ Balanced（均衡）/ Dev（开发）
- 提示注入检测（v2）
- 工具调用频率限制
- 危险操作二次确认

### 🎨 界面特性
- 极简模式：悬浮 HUD 面板，工具调用以 Toast 气泡展示
- 双主题：紫罗兰 / 黑金
- 窗口边框颜色随主题动态变化（Windows 11+）
- 中英文国际化

---

## 🚀 快速开始

### 系统要求

- Windows 10 / 11（64-bit）
- WebView2 Runtime（Windows 11 已预装；Windows 10 可从 [Microsoft 官网](https://developer.microsoft.com/microsoft-edge/webview2/) 下载）

### 下载安装

官网：[www.dimnuo.com](https://www.dimnuo.com)

前往 [Releases](https://github.com/njbinbin-pisci/openpisci/releases) 下载最新安装包（`.exe`）。

> **⚠️ 安全警告**：OpenPisci 是一款具备文件读写、命令执行、UI 自动化等高权限操作能力的 AI Agent。建议在虚拟机（如 VMware、VirtualBox、Hyper-V）中运行，以防止 AI 误操作导致宿主机数据损失。开发者不对因直接在宿主机运行而造成的任何数据丢失或系统损坏承担责任。

### 首次配置

1. 启动后进入引导向导
2. 选择 LLM 提供商并填入 API Key
3. 设置工作区目录（Agent 文件操作的默认根目录）
4. 开始使用

---

## 🔧 开发环境搭建

### 依赖

- [Rust](https://rustup.rs/) stable（≥ 1.77.2）
- [Node.js](https://nodejs.org/) 20 LTS
- [Visual Studio 2022 Build Tools](https://visualstudio.microsoft.com/downloads/)（Desktop C++ 工作负载）

### 克隆与运行

```bash
git clone https://github.com/njbinbin-pisci/openpisci.git
cd openpisci

# 安装前端依赖
npm install

# 开发模式（热重载）
npm run tauri dev

# 构建发行版
npm run tauri build
```

### 重新生成图标

```bash
npm run icon:emoji
```

---

## 🐠 自定义小鱼（Fish）

在 `%APPDATA%\com.pisci.desktop\fish\my-fish\FISH.toml` 创建文件：

```toml
id = "my-fish"
name = "我的小鱼"
description = "专注于某类任务的助手"
icon = "🐡"
tools = ["file_read", "shell", "memory_store"]

[agent]
system_prompt = "你是一条专注于..."
max_iterations = 20
model = "default"

[[settings]]
key = "workspace"
label = "工作目录"
setting_type = "text"
default = ""
placeholder = "例如：C:\\Users\\你的用户名\\Documents"
```

重启应用后在"小鱼"页面即可看到新小鱼。

---

## ⚡ 自定义技能（Skills）

在 `%APPDATA%\com.pisci.desktop\skills\my-skill\SKILL.md` 创建文件：

```markdown
---
name: My Skill
description: 描述这个技能的用途
version: "1.0"
tools:
  - file_read
  - shell
---

# My Skill

## 使用说明

当用户需要...时，按照以下步骤操作：
1. 首先...
2. 然后...
```

---

## 🔧 自定义工具（User Tools）

在"工具"页面安装 TypeScript 插件，支持自定义配置接口（如 SMTP 账号、API Key 等）。

用户工具存放路径：`%APPDATA%\com.pisci.desktop\user-tools\`

---

## 📁 数据目录

| 路径 | 内容 |
|------|------|
| `%APPDATA%\com.pisci.desktop\` | 配置文件、数据库 |
| `%APPDATA%\com.pisci.desktop\skills\` | 技能目录 |
| `%APPDATA%\com.pisci.desktop\fish\` | 用户自定义小鱼 |
| `%APPDATA%\com.pisci.desktop\user-tools\` | 用户自定义工具 |
| `%LOCALAPPDATA%\pisci\logs\` | 日志文件、崩溃报告 |

---

## 🏗️ 技术架构

```
OpenPisci
├── src-tauri/          # Rust 后端
│   ├── src/
│   │   ├── agent/      # Agent Loop、HostAgent、消息管理
│   │   ├── commands/   # Tauri IPC 命令层
│   │   ├── fish/       # Fish 子 Agent 系统
│   │   ├── gateway/    # IM 网关（飞书、钉钉、Telegram 等）
│   │   ├── llm/        # LLM 客户端（Claude、OpenAI、DeepSeek、Qwen）
│   │   ├── memory/     # 记忆系统（向量搜索、FTS）
│   │   ├── policy/     # 策略门控、注入检测
│   │   ├── scheduler/  # Cron 调度器
│   │   ├── security/   # 加密、密钥管理
│   │   ├── skills/     # 技能加载器（SKILL.md 格式）
│   │   ├── store/      # SQLite 数据库、设置持久化
│   │   └── tools/      # 工具实现
│   └── Cargo.toml
└── src/                # React 前端
    ├── components/     # 页面组件
    ├── i18n/           # 中英文翻译
    ├── services/       # Tauri IPC 服务层
    └── store/          # Redux 状态管理
```

---

## 📄 许可证

[MIT License](./LICENSE)

---

<p align="center">Built with ❤️ by the OpenPisci community</p>
