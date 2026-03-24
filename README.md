# 🐟 OpenPisci

**开源 Windows AI Agent 桌面应用**

OpenPisci 是一款运行在 Windows 桌面的本地优先 AI Agent，基于 Tauri 2 + Rust + React 构建。大鱼（Pisci）是主 Agent，锦鲤（Koi）是持久化协作 Agent，小鱼（Fish）是无状态临时子 Agent。

[English](./README_EN.md) | 中文

---

## ✨ 核心特性

### 🤖 强大的 Agent 能力
- **多 LLM 支持**：Claude（Anthropic）、GPT（OpenAI）、DeepSeek、通义千问（Qwen）、智谱、Kimi、MiniMax，以及任意 OpenAI 兼容接口
- **自动记忆**：对话结束后自动调用 LLM 提取关键信息存入长期记忆，下次对话自动注入相关上下文
- **主动记忆**：Agent 在对话中可主动调用 `memory_store` 工具保存重要信息
- **任务分解**：复杂任务自动分解为子任务并依次执行（HostAgent）
- **崩溃恢复**：每次迭代写入 checkpoint，程序崩溃后可从断点恢复
- **心跳机制**：可配置定时心跳，Agent 自主检查待处理任务
- **循环检测**：四种检测器（GenericRepeat / KnownPollNoProgress / PingPong / GlobalCircuitBreaker）防止 Agent 陷入死循环

### 🐟 Pisci / Koi / Fish：三层 Agent 架构

| 角色 | 定位 | 生命周期 | 典型职责 | 与其他角色的关系 |
|------|------|----------|----------|------------------|
| `Pisci` | 主 Agent / 项目经理 / 用户入口 | 常驻 | 与用户对话、调用工具、创建鱼池、协调多 Agent、判断项目是否可收尾 | 负责组织 Koi，也可调用 Fish 处理临时子任务 |
| `Koi` | 持久化协作 Agent | 持久化存在，可多项目复用 | 在鱼池中承担角色分工，如架构、编码、测试、审查、研究 | 通过 `pool_chat` 在鱼池中协作，必要时 @mention 彼此或 @pisci |
| `Fish` | 无状态临时子 Agent | 一次性 / 按需创建 | 处理批量扫描、资料整理、单次分析、上下文隔离的多步骤工作 | 由 Pisci 或 Koi 通过 `call_fish` 委派，不直接参与鱼池协作 |

**理解方式：**
- `Pisci` 是总控入口，负责对用户负责。
- `Koi` 是长期团队成员，适合持续项目协作。
- `Fish` 是临时工，适合“做完即走”的子任务。

**关键区别：**
- `Pisci` 决定是否建池、如何分工、何时继续推进、何时请求用户确认收尾。
- `Koi` 有独立身份、独立记忆归属、独立待办，能在多个项目中被反复唤醒。
- `Fish` 不维护长期项目状态，不占用主会话上下文，只返回最终结果。

### 🏞️ 鱼池（Pond）里有什么

鱼池不是一个单独 Agent，而是一套围绕项目协作构建的可视化工作区：

- **项目池（Pool Session）**：一个项目对应一个池，包含项目名、状态、组织规范（`org_spec`）和可选 `project_dir`
- **Pool Chat**：Pisci、Koi 在这里自然对话、交接、提问、@mention 协作
- **看板（Board / Kanban）**：展示 Koi todo 的 `todo / in_progress / blocked / done / cancelled`
- **Koi 面板**：展示每个 Koi 的身份、角色、在线状态、当前工作负载
- **Pisci Inbox / Heartbeat**：Pisci 的项目级收件箱，用于接收 `@pisci`、状态信号、心跳巡检结果
- **知识库（`kb/`）**：项目共享知识区，用于沉淀架构、API、缺陷、决策等文档
- **项目目录 / Git worktree**：若设置 `project_dir`，每个 Koi 可在自己的分支和 worktree 中工作，减少文件冲突

### 🤝 鱼池如何协同

一个标准的鱼池项目通常按下面的机制运行：

1. **用户发起项目**
   - 用户可以在应用内聊天，也可以通过飞书等 IM 直接告诉 Pisci“创建一个鱼池项目”
   - Pisci 通过 `pool_org(action="create")` 创建项目池，并写入 `org_spec`

2. **Pisci 组织团队**
   - Pisci 根据项目目标选择合适的 Koi 角色
   - Pisci 优先通过 `pool_chat` 发送带 `@KoiName` 的消息来发起工作，而不是死板串行分配

3. **Koi 自主协作**
   - Koi 在 `pool_chat` 中汇报进展、交接工作、提出问题、请求复审
   - `@mention` 是消息，不是硬命令：被提及的 Koi 会自主判断是立即响应、继续当前工作，还是请求 Pisci 协调
   - `@all` 可向整个项目团队广播

4. **待办与状态同步**
   - 任务通过 `koi_todos` 追踪，状态流转为 `todo -> in_progress -> done / blocked / cancelled`
   - Pisci 和任务所有者可以更新任务状态；其他 Koi 需要通过 `@pisci` 请求变更
   - `pool_chat` 中的 `[ProjectStatus] follow_up_needed / waiting / ready_for_pisci_review` 信号会辅助 Pisci判断项目是否继续推进

5. **Pisci 心跳与继续推进**
   - 心跳会扫描池内新消息、待办和状态信号
   - 只要仍有 active todo，或有人发出 `follow_up_needed / waiting`，Pisci 就应继续协调，而不是把项目误判为结束
   - 只有当工作真正收敛，并且有人明确用 `ready_for_pisci_review @pisci` 把判断权交回时，Pisci 才进入收尾审查

6. **项目收尾**
   - Koi 只能建议“可由 Pisci 审查是否结束”，不能单方面宣布项目结束
   - 最终是否归档，由 Pisci 汇总后向用户确认，再执行 `pool_org(action="archive")`

### 🛠️ 丰富的 Windows 工具集

| 工具 | 说明 |
|------|------|
| `file_read` / `file_write` | 文件读写（支持分块读取大文件） |
| `file_edit` | 精确字符串替换，支持 `edits` 数组批量原子修改 |
| `file_diff` | 修改前预览 unified diff，或对比两个文件 |
| `file_list` | 结构化目录列表（JSON，含大小/修改时间） |
| `file_search` | 按名称 glob 搜索或按内容 grep 搜索（支持 `file_extensions` 过滤） |
| `code_run` | 专为编程场景设计的命令执行工具，返回结构化输出并自动诊断常见错误 |
| `shell` / `powershell_query` | PowerShell 命令执行 / 结构化系统查询 |
| `wmi` | WMI/WQL 查询硬件和系统信息 |
| `web_search` | 多引擎并行搜索（DuckDuckGo、Bing、百度、360），结果合并去重 |
| `browser` | Chrome 浏览器自动化（CDP 协议） |
| `uia` | Windows UI Automation — 控制任意桌面应用 |
| `screen_capture` | 截图（全屏/窗口/区域），支持 Vision AI 分析 |
| `com` / `com_invoke` | COM/ActiveX 对象调用（支持 32/64 位） |
| `office` | 通过 COM 自动化 Word、Excel、PowerPoint、Outlook |
| `email` | 发送/接收邮件（SMTP/IMAP） |
| `ssh` | SSH 远程连接与命令执行 |
| `pdf` | PDF 读写、页面渲染为图像（`render_page_image` / `render_region_image`） |
| `vision_context` | 视觉上下文管理：跨轮次保存/选择图像，供 Agent 主动决策下一步看什么 |
| `memory_store` | 向长期记忆写入信息 |
| `plan_todo` | 为复杂任务维护可视化执行计划与待办状态 |
| 用户自定义工具 | TypeScript 插件，支持自定义配置接口 |
| MCP 工具 | 通过 MCP 协议接入外部工具服务器 |

### 🐠 小鱼（Fish）子 Agent 系统
- 通过 `FISH.toml` 定义专属子 Agent，拥有独立人设、工具权限和配置
- 小鱼是**无状态临时工作者**：主 Agent 或 Koi 通过 `call_fish` 工具委派子任务，小鱼执行完毕后仅返回最终结果
- **核心价值**：小鱼的中间推理和工具调用不会污染主 Agent / Koi 的上下文，有效节省上下文窗口
- 用户可在 `%APPDATA%\com.pisci.desktop\fish\` 目录放置自定义小鱼
- 适用于批量文件处理、数据收集、代码扫描等多步骤任务，而不是长期项目协作

### ⚡ 技能系统（Skills）
- 使用 `SKILL.md` 格式定义技能：YAML frontmatter（名称、描述、工具列表等）+ Markdown 正文（使用说明）
- 技能内容在每次 Agent 调用时自动注入系统提示词，引导 Agent 使用特定工具和流程
- **自动触发**：Agent 每次收到任务时优先调用 `skill_search` 查找匹配技能，找到则按技能指令执行
- **zip 包安装**：支持将 `SKILL.md` + `reference.md` + `examples.md` 等打包为 `.zip` 一键安装
- 支持从 URL 或本地路径安装技能（单文件或 zip 包）
- 技能持久化：安装的技能写入磁盘并同步到数据库，重启后自动恢复
- 内置技能：Office 自动化、文件管理、Web 自动化、系统管理、桌面控制

> **注意**：SKILL.md 是 OpenPisci 自定义的技能格式，与 Anthropic MCP（Model Context Protocol）是两套不同的规范。

### 💻 编程能力（v0.3.0 新增）
- **`code_run` 工具**：专为编程任务设计，返回结构化 `exit_code` / `stdout` / `stderr` / `duration_ms`，并对 Rust/Python/Node 常见错误自动诊断
- **`file_edit` 批量替换**：`edits` 数组一次调用原子修改多处，先全量验证再统一写入
- **`file_diff` 工具**：修改前预览 unified diff，或对比两个文件，帮助 Agent 自我校验
- **`file_search` 增强**：结果上限提升至 500，新增 `file_extensions` 精确过滤，单文件 grep 上限提升至 200KB
- **编程工作流指导**：系统提示词内置完整的"理解→修改→验证→调试"闭环指导

### 🔍 上下文预览（v0.3.0 新增）
- 点击聊天界面的 🔍 按钮，查看下一轮将要发给 LLM 的完整消息序列
- 结构化展示每条消息的 role、blocks（文本/工具调用/工具结果），工具调用和结果可折叠展开
- 显示 token 使用量与上下文预算进度条，帮助了解上下文压缩效果

### 🔗 文件链接（v0.3.0 新增）
- LLM 输出中的本地路径（如 `C:\Users\...\file.md`）自动转为可点击链接
- 点击后用系统默认程序打开对应文件或目录
- 支持 Windows 路径、UNC 路径、Unix 路径，以及 `file://` URI

### 📱 多平台 IM 网关

| 平台 | 模式 |
|------|------|
| 微信（WeChat） | 扫码绑定，双向收发（iLink Bot API，无需 CLI） |
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

重启应用后在"小鱼"页面即可看到新小鱼。主 Agent 会通过 `call_fish` 工具自动委派匹配的任务给小鱼。

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
│   │   ├── llm/        # LLM 客户端（Claude、OpenAI、DeepSeek、Qwen 等）
│   │   ├── memory/     # 记忆系统（向量搜索、FTS）
│   │   ├── policy/     # 策略门控、注入检测
│   │   ├── scheduler/  # Cron 调度器
│   │   ├── security/   # 加密、密钥管理
│   │   ├── skills/     # 技能加载器（SKILL.md 格式）
│   │   ├── store/      # SQLite 数据库、设置持久化
│   │   └── tools/      # 工具实现（含 code_run、file_diff 等）
│   └── Cargo.toml
└── src/                # React 前端
    ├── components/     # 页面组件
    ├── i18n/           # 中英文翻译
    ├── services/       # Tauri IPC 服务层
    └── store/          # Redux 状态管理
```

---

## 📋 更新日志

### v0.5.19
- **Excel 图表修复**：修复 sheet_check 逻辑错误导致指定工作表时条件判断反转的 bug；dd_chart 在 SetSourceData 之后再次强制设置图表类型，防止 Excel 自动重置为默认类型；强化工具描述，要求 AI 必须显式传 chart_type（折线图=line，柱状图=column，饼图=pie 等），避免误生成饼图

### v0.5.18
- **Koi 超时修复**：Koi 超时后自动将其 in_progress 任务改为 blocked 状态，并向鱼池发送 @pisci 通知；心跳扫描新增对 blocked todo 的持久唤醒逻辑，确保项目不会因 Koi 超时而永久卡死
- **文件编码增强**：ile_read 自动识别并透明处理 UTF-8 BOM、UTF-16 LE/BE、GBK/GB18030；ile_write/ile_edit 写回时自动保留原文件 BOM；工具描述和系统提示词新增文件编码操作指南

### v0.5.17
- **微信接入**：直接对接腾讯 iLink Bot HTTP API，无需安装 Node.js 或任何 CLI；在设置页启用微信通道后点击「绑定微信」，扫描二维码即可完成绑定；Agent 回复通过 iLink sendmessage 接口实时送达微信用户

### v0.5.16
- **UAC 执行修复**：修复 elevated 命令返回值解析失败的两个根本原因：① Windows `[System.Text.Encoding]::UTF8` 写文件时默认带 UTF-8 BOM，导致 `serde_json` 解析失败（`expected value at line 1 column 1`）；② `regsvr32`、`reg` 等原生可执行文件在 `& { } 2>&1` 块内执行时 `$LASTEXITCODE` 不会被正确设置，导致退出码始终为 0；新方案将用户命令写入独立 inner 脚本，通过 `Start-Process -Wait -PassThru` 执行并用 `$proc.ExitCode` 获取真实退出码，结果文件改用无 BOM 的 UTF-8 编码写入

### v0.5.15
- **实时持久化**：彻底修复消息丢失问题——之前持久化在 `run()` 结束时批量写入，若程序在迭代中途退出（编译重启、崩溃等）则所有中间消息全部丢失；现在每产生一条消息立即写入数据库，程序任何时刻退出都不会丢失已完成的步骤

### v0.5.14
- **最终总结持久化修复**：修复上下文压缩（compaction）后最终总结消息丢失的根本原因——原方案依赖 `context_len` 偏移量定位新消息，压缩后列表缩短导致偏移越界、新消息全部丢失；新方案在 `AgentLoop::run()` 内部维护独立的 `new_messages` buffer，与 LLM 上下文窗口完全分离，压缩只影响上下文，不影响持久化，从根本上消除了该类问题
- **关于页面重新设计**：GitHub 链接与产品介绍并排展示；新增"关于我们"卡片，包含团队介绍和官网链接；更新产品描述，加入 Koi 三层多智能体架构说明
- **内部会话自动打开修复**：启动时不再自动激活心跳/巡检等内部会话，始终优先选择用户可见会话

### v0.5.13
- **会话切换修复**：修复切换会话后消息区不更新、始终显示同一会话内容的问题；修复 IM 会话不在会话列表中导致无法切换的问题
- **超宽内容局部滚动**：表格、代码块等超宽内容在气泡内生成局部横向滚动条，不再撑宽气泡或导致整个消息区出现横向滚动条
- **流式事件归属修复**：`done`/`error` 事件使用注册时的会话 ID，防止用户切换会话时污染其他会话的状态

### v0.5.12
- **压缩算法单元测试**：在 `AgentLoop` 中新增 11 个专项测试，覆盖 Level-1 工具结果截断（`compact_trim_tool_results`）、Level-2 LLM 摘要压缩（`compact_summarise`）、`estimate_message_tokens` 各消息类型估算，以及 154 条消息崩溃场景的回归验证
- **每个 Koi 独立 `max_iterations`**：可在 Koi 详情中单独设置最大迭代次数，覆盖全局默认值
- **压缩算法修复**：修复 `compact_summarise` 对 ToolUse/ToolResult 消息内容提取为空的问题，确保摘要提示词包含真实工具调用信息；修复压缩失败时无限循环的问题；新增主动压缩触发（上下文超过预算 80% 时提前压缩）；压缩后注入续任务提示，防止 LLM 误判任务已完成
- **聊天气泡稳定性**：迭代结束后保留流式消息的合并视图，不再拆分为大量独立气泡
- **聊天滚动修复**：修复消息刷新时整个主界面向上跳动的问题

### v0.5.8
- **项目暂停 / 恢复 / 归档**：用户可直接在鱼池 UI 中暂停、恢复、归档项目，无需通过 Pisci 对话操作；暂停时自动取消正在运行的 Koi 任务并重置进行中的待办
- **`complete_todo` 强制摘要**：`complete_todo` 工具现在必须传入 `summary` 参数，确保 Koi 完成任务后聊天界面始终显示简洁的完成摘要，不再出现空白 Result 消息
- **Koi 上限提升至 10**：最大 Koi 数量从 5 提升至 10
- **Pisci 可管理 Koi**：`app_control` 工具新增 `koi_list` / `koi_create` / `koi_delete` 动作，Pisci 可在用户明确要求时帮助创建或删除 Koi（提示词要求不主动创建）
- **Koi worktree 严格隔离**：Koi 在 Git worktree 中工作时，`allow_outside_workspace` 始终为 `false`，防止意外写入主项目目录

### v0.5.7
- **看板精度提升**：修复看板 todo 状态同步问题，改善 Pool Chat 消息分页加载
- **Koi 状态管理改进**：Koi 身份强化、任务提示词优化，防止角色混淆
- **消息分页与 UI 改进**：Pool Chat 和 Coordinator Inbox 支持分页加载，新增 Koi tooltip 面板
- **Koi 结果截断上限提升**：`call_koi` 结果截断上限大幅提高，避免摘要被截断
- **Inbox 空消息抑制**：修复 Coordinator Inbox 中出现空心跳消息的问题

### v0.5.6
- **Pool Chat Markdown 渲染**：鱼池聊天消息支持 Markdown 渲染，本地文件路径自动转为可点击链接
- **Coordinator Inbox 增强**：新增删除按钮、Markdown 渲染、会话删除确认对话框
- **`file://` 协议支持**：修复 ReactMarkdown 中 `file://` 链接无法点击的问题

### v0.5.5
- **每个 Koi 独立 LLM 配置**：每个 Koi 可单独选择 LLM 提供商和模型，不再共享全局设置
- **单实例锁**：应用启动时检测已运行实例，防止重复启动
- **LLM 提供商管理入口调整**：LLM 提供商管理移入 AI Provider 设置区

### v0.5.4
- **文件工具相对路径感知**：`file_read` / `file_write` 等工具在 Koi worktree 场景下正确处理相对路径，防止 Koi 绕过 worktree 隔离
- **Git 协作流程修复**：修复 Koi 与 Pisci 通过 Git 分支协作的流程，确保 Koi 在独立分支工作、Pisci 负责合并
- **心跳与协作提示词重写**：重写心跳和 Koi 协作提示词，修复 Pisci 误判项目结束的问题

### v0.5.3
- **补充多 Agent 文档**：新增 Pisci / Koi / Fish 分层说明，以及鱼池组件与协同机制说明
- **修复 Pisci 心跳误判**：有 `follow_up_needed / waiting` 但无 active todo 时，不再误报 `HEARTBEAT_OK`，而是要求继续协调
- **协同测试覆盖增强**：多 Agent 集成测试新增心跳保护、短 `pool_id` 解析与陈旧状态恢复覆盖

### v0.5.2
- **修复 unnamed 技能幽灵问题**：卸载技能后切回技能页不再出现 unnamed 占位技能；在 FS→DB 同步、DB→FS 反向同步、`list_skills` 返回值四处均过滤无效记录，并在启动时主动清理历史遗留的 unnamed 条目

### v0.5.1
- **设置实时刷新**：Agent 通过工具修改配置（SSH 服务器、API Key、工具开关等）后，设置页面立即自动刷新，无需重启
- **MCP 配置入口说明**：MCP 工具配置入口在侧边栏 → 🔧 工具 → 🔗 MCP 标签页，支持 stdio / SSE 两种传输方式

### v0.5.0
- **多模态视觉迭代（Vision Artifact Store）**：新增 `vision_context` 工具，Agent 可跨轮次主动保存、选择图像；PDF 工具新增 `render_page_image` / `render_region_image` 动作，Agent 可自主决定"下一步看哪里"
- **技能 zip 包安装**：安装技能支持 `.zip` 压缩包（本地路径或 URL），一次安装带上 `SKILL.md` + `reference.md` + `examples.md` + 辅助脚本等所有文件
- **工具调用可中断**：停止按钮现在可在 200ms 内中断正在执行的工具调用（`tokio::select!` 取消监听），不再需要等待工具跑完
- **技能自动触发**：系统提示词开头新增强制规则，Agent 每次收到任务优先调用 `skill_search` 查找匹配技能
- **技能持久化修复**：启动时主动扫描磁盘技能目录同步到数据库，重启后已安装技能不再消失
- **路径特殊字符清理**：安装技能时自动过滤从 Windows 资源管理器复制路径时插入的 Unicode 不可见字符（`U+202A` 等）

### v0.4.1
- **新增 `plan_todo` 工具**：Agent 可像 Cursor 一样维护当前复杂任务的待办计划，支持 `pending / in_progress / completed / cancelled` 状态更新
- **计划面板实时可视化**：聊天界面新增计划面板，执行中和执行后都可查看当前任务计划与进度
- **计划策略提示词**：系统提示词新增 Planning 段落，引导 Agent 在复杂任务中主动维护短计划
- **工具控制能力继续开放**：主题切换、极简模式、窗口移动、内置工具开关、用户工具配置已可通过 Agent 的 `app_control` 工具操作

### v0.4.0
- **小鱼无状态重构**：小鱼（Fish）从独立会话模式重构为无状态临时工作者，由主 Agent 通过 `call_fish` 委派子任务，中间过程不污染主上下文
- **call_fish 提示词增强**：系统提示词新增 Sub-Agent Delegation 策略段落，引导主 Agent 主动使用小鱼处理多步骤任务
- **统一确认对话框**：创建共享 `ConfirmDialog` 组件，替换所有 `window.confirm()` 调用（技能卸载、工具卸载、MCP 删除、定时任务删除、记忆清空、审计日志清空）
- **技能加载修复**：修复后安装技能被错误分类为内置技能导致不显示的问题

### v0.3.0
- **编程能力增强**：新增 `code_run` 工具（结构化输出 + 错误诊断）、`file_diff` 工具（unified diff 预览）
- **`file_edit` 批量替换**：支持 `edits` 数组，一次调用原子修改多处
- **`file_search` 增强**：结果上限 500，新增 `file_extensions` 过滤，grep 单文件上限 200KB
- **上下文预览**：聊天界面新增 🔍 按钮，结构化查看发给 LLM 的消息序列（含 token 统计）
- **文件链接**：LLM 输出中的本地路径自动转为可点击链接，点击用系统程序打开

### v0.2.0
- 多模态视觉 Agent（截图 + Vision AI）
- UIA 精度测试
- MCP / SSH / PDF 工具
- 多 LLM 支持扩展（智谱、Kimi、MiniMax）

---

## 📄 许可证

[MIT License](./LICENSE)

---

<p align="center">Built with ❤️ by the OpenPisci community</p>
