# CodeZ — 类 Cursor AI IDE 设计文档

> 基于 `openpisci`（鱼池 IDE）演进为一个类 Cursor 的 AI IDE 产品。
> 共享一个 agent 内核（`pisci-kernel`），对外提供两种一等模式：
> **IDE 模式**（编辑器为中心，≈ Cursor）与 **Agent 模式**（任务为中心，≈ Codex）。

---

## 1. 现状分析：鱼池 IDE 已具备的资产

`openpisci` 已经是一个分层良好的 **Agent 运行时 + 嵌入式 IDE**，复用率约 70%。

### 1.1 四层 Crate 架构（最大的财富）

```
src-tauri/
├── pisci-core/     纯契约层：HostRuntime / EventSink / Notifier / HostTools / SecretsStore + schema
├── pisci-kernel/   OS 无关的 Agent 运行时（核心资产）
├── pisci-cli/      headless 适配器（openpisci-headless 二进制，NDJSON 流式）
└── src/            pisci-desktop：Tauri 宿主 + 平台工具 + React UI
```

内核**从不 import Tauri**，所有 UI / 密钥 / 平台工具通过 `Arc<dyn HostRuntime>` 注入。
这正是 Cursor（编辑器内嵌）与 Codex（headless / 云端）**共用一个内核**的理想前提。

### 1.2 Agent 内核（`pisci-kernel`）已有能力

| 模块 | 现状 | 对标 |
|---|---|---|
| `agent/loop_.rs`（4693 行）+ `harness/` | 完整 Agent 循环、多 harness（main-chat / Koi / Fish / debug / scheduler）、分层 prompt、budget、上下文构建 | Cursor/Codex agent loop |
| `agent/compaction.rs` | Compaction v2（micro/auto/full 三档）、rolling summary、state frame | 长上下文管理 |
| `llm/` | claude / openai / qwen / deepseek / kimi / minimax / zhipu | 多模型 |
| `pool/` | Pool/Koi/Fish 多智能体编排、coordinator、subagent | Codex 并行/后台任务 |
| `tools/` | file_read/write/list/search/diff、shell、code_run、process_control、web_search、ssh、mcp、plan_todo… | 编码 agent 工具集 |
| `memory/vector.rs` | 向量嵌入（余弦相似度 + hybrid keyword/vector merge） | **可复用做代码库语义索引** |
| `policy/` | 工具策略、denylist、approval 审批 | 安全/确认 |
| `store/` + `security/` | SQLite + 加密 settings（chacha20poly1305） | 持久化 |

### 1.3 已有 IDE（`src/components/Pond/IDE/`）

已是相当完整的 VS Code 式 IDE，但**被埋在 Pond → Collab 协作工作区**，非一等公民：

- **Monaco 编辑器** + LSP（hover / completion / definition / references / diagnostics，
  WebSocket 桥接 rust-analyzer / tsserver / pyright / clangd）
- FileTree、EditorTabs、**Terminal（xterm + PTY）**、GitPanel（status/diff/branches/commit/checkout）、SearchPanel
- 文件 watcher（外部 / Agent 改动实时回灌编辑器）
- `AssistantPanel`：CLI 风格、按项目隔离的 chat
- `ide.ts`：完整的文件 / Git / 终端 / watcher Tauri 命令

### 1.4 已有"类 Codex"雏形

- `openpisci-headless`（pisci-cli）：读 JSON、流 NDJSON 的无界面 agent；已有 `bench_swe_lite` 真实仓库修复评测
- Pool/Koi 编排 + `Pond/Board`（任务看板组件已存在）

### 1.5 关键缺口（代码确认）

1. **IDE 不是顶级模式**，嵌在协作池里
2. **无 inline AI**：无 Tab 补全（next-edit）、无 Cmd-K 行内编辑、无 `applyEdit`/diff 接受拒绝
3. **无代码库语义索引**：检索仅 ripgrep（`file_search`）；但 `memory/vector.rs` 的嵌入 + hybrid 检索基建可复用
4. **@-mention 上下文引用**（文件/符号/codebase）在编辑器 chat 缺失
5. Agent 模式缺**任务级隔离**（git worktree / 分支）与 **diff/PR 评审产物**
6. 平台工具偏 Windows（UIA / PowerShell / WMI / COM），跨平台 IDE 需设为可选

---

## 2. 目标产品形态：一个内核，两种模式

最终产品 = **CodeZ**，共享 `pisci-kernel`，顶栏一键切换两个一等模式。

```
┌──────────────────────────────────────────────────────────┐
│  CodeZ            [ IDE 模式 ⌘1 ]  [ Agent 模式 ⌘2 ]        │
├──────────────────────────────────────────────────────────┤
│                                                            │
│  IDE 模式（≈ Cursor）           Agent 模式（≈ Codex）        │
│  以编辑器为中心：               以任务为中心：               │
│  · Monaco + LSP                · 提交任务 → 后台自治         │
│  · Tab 补全 / Cmd-K            · 隔离 worktree/分支          │
│  · 右侧 AI Chat + @引用        · 计划→编辑→测试→迭代         │
│  · 行内 diff 接受/拒绝          · 任务看板 + diff/PR 评审     │
│                                · 可并行多任务、可云端跑       │
└──────────────────────────────────────────────────────────┘
        共享：pisci-kernel（agent loop / tools / LLM / 索引 / 策略 / 记忆）
```

**核心区别**

- **IDE 模式**：人在回路、低延迟、编辑器内增量改动，agent 作"副驾"。强调 Tab、Cmd-K、Chat-with-Apply。
- **Agent 模式**：交付任务后离开，agent 高自治、长时运行，产出可评审 diff/PR。强调隔离、计划、自验证、可观测。

二者复用同一 agent loop，仅 **harness 配置 + 工具暴露面 + 产物形态**不同——契合内核现有的 `HarnessConfig` 多 harness 设计。

---

## 3. 整体架构设计

### 3.1 Crate 演进（最小侵入）

```
pisci-core      → 不动（契约层已通用）
pisci-kernel    → 新增 3 个子模块：
                  · index/      代码库索引（复用 memory/vector.rs 的嵌入/hybrid）
                  · edit/       编辑原语：行内补全、Cmd-K 编辑、结构化 patch、apply
                  · agent_task/ Agent 模式任务编排（worktree 隔离 + 生命周期）
pisci-cli       → 扩展 run 子命令为长时任务 runner（Agent 模式 headless/云端复用）
src (desktop)   → UI 重构为双模式；IDE 提级；新增 inline AI 与 task board
```

把"编辑/索引/任务"放进内核，保证 **IDE 模式（编辑器内）与 Agent 模式（headless/云端）行为一致**。

### 3.2 两种模式映射到 Harness

复用 `agent/harness/config.rs` 的 `HarnessConfig`：

| 维度 | IDE 模式 harness | Agent 模式 harness |
|---|---|---|
| 工具面 | file_*、edit、index、shell（需确认）、lsp、read_lints | 全量 + process_control + pool 编排 + git worktree |
| 确认策略 `ConfirmFlags` | `confirm_file_write=true`（行内 diff 待人接受） | 自治（白名单自动执行，越界才问） |
| 上下文 | 当前文件 + 选区 + @引用 + 索引召回 | 任务描述 + 仓库索引 + 计划状态 + rolling summary |
| 产物 | 编辑器内 diff（接受/拒绝 hunk） | 分支 commit + `changes.patch` + 测试报告 + 可选 PR |
| 事件 | `agent_event_*`（Tauri 事件总线，已存在） | NDJSON（CLI/云）+ Tauri（桌面看板） |

---

## 4. IDE 模式详细设计（≈ Cursor）

### 4.1 IDE 提为一等模式
- 顶级路由新增 `ide` / `agent`（扩展 `App.tsx` 的 `Tab` 类型），把 `Pond/IDE` 抽到 `src/workspaces/ide/`，与 Pool 解耦（保留 Pool 作"团队协作"子场景）。
- 打开任意本地文件夹即进入 IDE（不再强制 pool 的 `project_dir`）。

### 4.2 三大行内 AI 能力（Cursor 的灵魂）

**(a) Tab 补全 / Next-Edit 预测**
- Monaco `registerInlineCompletionsProvider` → 新 Tauri 命令 `ai_inline_completion(file, prefix, suffix, cursor)` → 内核 `edit/` 轻量 FIM（小模型/低延迟）。
- 进阶：next-edit（预测下一处应改位置），`text_delta` 流式 ghost text。

**(b) Cmd-K 行内编辑/生成**
- 选区 → 浮层输入指令 → `ai_inline_edit(selection, instruction, fileContext)` → 结构化 patch → Monaco inline diff（绿/红）→ 接受/拒绝。
- 复用 `file_diff` 的 diff 语义，新增 `edit/patch.rs` 产出 `{range, newText}[]`。

**(c) Chat 侧栏 + @引用 + Apply**
- 把 `AssistantPanel` 升级为右侧 Chat 面板（复用 `Chat/` 的 Chat UI Protocol v2 与流式）。
- **@引用**：`@file` / `@symbol`（LSP）/ `@codebase`（语义索引）/ `@diff` / `@terminal`。
- 回复代码块带 **Apply 按钮** → 转编辑器 inline diff，逐 hunk 接受。复用 watcher 让 agent 直接写文件时编辑器实时刷新。

### 4.3 代码库语义索引（新增，复用现有嵌入基建）
- 新增内核 `index/`：分块（按符号/行窗口）→ 调 embedding（settings 配 embedding 模型）→ 存 SQLite（`store/db.rs` 加表）→ 检索用 `memory/vector.rs` 的 `cosine_similarity` + `hybrid_merge`（向量 + ripgrep 关键词）。
- 增量更新挂到已有 file watcher。
- 暴露工具 `codebase_search`（语义）给两种模式，并支撑 `@codebase`。

---

## 5. Agent 模式详细设计（≈ Codex）

与 IDE 模式最大差异：**任务化、自治、隔离、可评审**。

### 5.1 任务生命周期

```
提交任务(prompt + 仓库 + 可选基准分支)
   │
   ├─ 1. 隔离：git worktree 新建 codez/task-<id> 分支（新增 agent_task/worktree.rs）
   ├─ 2. 计划：agent 产出 plan_todo（复用 plan.rs / plan_todo 工具）
   ├─ 3. 执行循环：编辑→shell 跑测试/构建→读 lints→自我修正（复用 loop_ + code_run + read_lints）
   ├─ 4. 自验证：跑 test_command（沿用 swe_lite 测试驱动思路）
   ├─ 5. 产物：changes.patch + 测试日志 + 摘要（沿用 swe_lite telemetry）
   └─ 6. 评审：看板看 diff → 接受/合并 / 退回迭代 / 一键开 PR（gh）
```

### 5.2 UI：任务看板（复用现有组件）
- 复用 `Pond/Board` 作 Agent 模式主界面：任务列（排队 / 运行中 / 待评审 / 已合并）。
- 卡片：实时步骤流（复用 `OperationSteps`）、token/成本遥测、diff 预览（复用 Monaco diff）、终端输出。
- **并行多任务**（复用 Pool 的 coordinator/subagent，每任务一个 worktree）。

### 5.3 运行时：本地 / headless / 云端三态共用
- 本地桌面：直接在 `pisci-desktop` 跑（已支持）。
- headless/CI：扩展 `openpisci-headless run` 为长时任务模式，复用 `pisci-cli/runner.rs` 与 NDJSON 事件。
- 云端（后续）：同一 headless 二进制丢进容器/沙箱，NDJSON 回流看板——即 Codex 的"云端 agent"形态，架构零改动铺路。

### 5.4 自治与安全
- 复用 `policy/`：Agent 模式白名单自动执行（文件写、测试、构建），危险操作（rm -rf、网络、git push）触发 `Notifier::request_confirmation`（桌面弹窗 / CLI 回退默认值）。
- 所有改动隔离在 worktree 分支，主分支永不被直接污染。

---

## 6. 共享内核 → 复用映射总表

| 能力 | 直接复用 | 需新增/改造 |
|---|---|---|
| Agent 循环 | `agent/loop_.rs` + `harness/` | 两套 `HarnessConfig` preset |
| LLM | `llm/*` 全部 | embedding 客户端、低延迟补全模型路由 |
| 工具 | file_*/shell/code_run/diff/process_control/ssh/mcp/plan_todo/web_search | `edit`(patch/inline)、`codebase_search`、`git_worktree` |
| 长上下文 | compaction v2 / rolling summary / state frame | — |
| 多智能体 | pool/coordinator/subagent | Agent 模式并行任务编排 |
| 索引/检索 | `memory/vector.rs`(cosine+hybrid) | `index/` 代码分块+持久化+增量 |
| 编辑器 | Monaco + LSP + watcher + Git/Terminal | inline completion / Cmd-K / apply diff |
| 任务/看板 | `Pond/Board` / `OperationSteps` / Chat UI v2 | 任务生命周期 + worktree 评审 |
| Headless/云 | `openpisci-headless` / NDJSON / swe_lite | 长时任务 runner + 沙箱 |
| 安全 | policy / approval / security 加密 | — |

---

## 7. 实施路线图（建议里程碑）

| 里程碑 | 目标 | 预估 |
|---|---|---|
| **M0 解耦与提级** | IDE 抽为顶级工作区，双模式切换骨架；打开任意文件夹即用 | 1–2 周 |
| **M1 IDE 行内 AI** | Cmd-K 行内编辑 + apply diff（最高性价比，先做）；Chat 侧栏 + @file/@symbol | 2–4 周 |
| **M2 代码库索引** | `index/` + `codebase_search` + `@codebase`；接 watcher 增量更新 | 2–3 周 |
| **M3 Tab 补全** | inline completions + next-edit，低延迟模型路由 | 2–3 周 |
| **M2.5 VS Code 生态兼容** | `.vsix` 贡献点适配层（LSP/主题/TextMate/Snippet）+ 兼容性测试套件，详见 §9 | 2–3 周 |
| **M4 Agent 模式 MVP** | worktree 隔离 + 任务看板（复用 Board）+ 计划/执行/自验证 + diff 评审；本地优先 | 3–4 周 |
| **M5 Headless/云端 + 并行** | headless 长时 runner、并行多任务、PR 集成、沙箱化 | 持续 |

---

## 8. 关键风险与取舍

1. **延迟 vs 质量**：Tab 补全必须用小/快模型，与 Chat/Agent 的大模型分路由（settings 加 `completion_model`）。
2. **跨平台**：Windows 专属工具（UIA/PowerShell/WMI/COM）全部 `#[cfg]` 可选；IDE/Agent 核心路径只依赖 neutral tools，保证 Linux/macOS 可用。
3. **diff apply 一致性**：编辑器 inline diff 与内核 patch 必须同一套 patch 表示，避免"应用后与 agent 设想不一致"。
4. **索引成本**：大仓库首次嵌入耗时/费用——支持仅索引选定目录 + 后台增量。
5. **Agent 自治边界**：默认隔离分支 + 白名单 + 危险操作审批，避免破坏工作区。

---

## 9. VS Code 生态兼容策略

> 核心事实：CodeZ 用的是 **Monaco**（编辑器内核），不是完整的 VS Code 工作台。
> VS Code 插件分两类——**声明式贡献（数据/协议）** 与 **命令式 JS API（`vscode.*`）**。
> 前者可小范围兼容并吃到生态里最有价值的部分；后者依赖 Electron + workbench + 扩展宿主，
> 完整兼容等于重写一个 VS Code（Theia / code-server 体量），**不建议做**。
>
> 一句话原则：把 `.vsix` 当作"贡献点数据包"消费，**不要去跑插件的任意 JS**。

### 9.1 可行性分层

| 插件类别 | 兼容方式 | 可行性 | 价值 |
|---|---|---|---|
| **语言服务器（LSP）** | 复用插件内置 language server 二进制，挂到现有 LSP 桥 | ✅ 高（已有 LSP） | 极高 |
| **调试适配器（DAP）** | Debug Adapter Protocol，语言中立（同 LSP 思路） | ✅ 中 | 高 |
| **颜色主题** | VS Code theme JSON → Monaco `defineTheme` 转换器 | ✅ 高（纯数据） | 高 |
| **TextMate 语法高亮** | `vscode-textmate` + `vscode-oniguruma`(WASM) 加载 `.tmLanguage` | ✅ 中 | 高 |
| **代码片段 Snippets** | JSON → Monaco CompletionItem | ✅ 很高 | 中 |
| **文件图标主题** | icon-theme JSON 映射 | ✅ 高 | 中 |
| **格式化 / Linter** | 多为 CLI 或 LSP，包装即可 | ✅ 中 | 中 |
| **命令式 UI 插件**（webview / tree view / command palette / `vscode.window.*` 等完整 API） | 需实现扩展宿主 + 数千 API 面 | ❌ 不建议 | —— |

**最佳切入点**：`LSP + 主题 + TextMate 语法 + Snippet`（已有 LSP，性价比最高）。

### 9.2 技术方案：`.vsix` 贡献点适配层

`.vsix` 本质是 zip，内含 `extension/package.json`，其 `contributes.*` 即声明式贡献点。
新增模块 `extensions/`（前端 + 必要的内核命令）：

```
1. 解压 .vsix → 读 package.json 的 contributes
2. 按"白名单贡献点"分发：
   contributes.languages / grammars   → TextMate 语法（vscode-textmate + oniguruma WASM）
   contributes.themes                  → 主题转换器 → monaco.editor.defineTheme
   contributes.iconThemes              → 文件图标映射
   contributes.snippets                → Monaco 补全项
   contributes.debuggers               → DAP 适配器进程
   （检测到 LSP server）               → 复用现有 ide_lsp_* 桥
3. 未识别贡献点 → 明确"不支持"提示（不静默失败）
```

**落点（贴合现有代码）**
- **主题**：现状 `CodeEditor.tsx` 写死 `vs-dark` → 改为 `monaco.editor.defineTheme(name, convert(vscodeThemeJson))`，与现有 `themes/violet|gold.css` 并存。
- **语法**：在 `CodeEditor.tsx` 注册 TextMate provider，覆盖 Monaco 自带 Monarch。
- **LSP**：直接复用 `ide_lsp_*` + `services/tauri/lsp.ts`，仅 server 命令来自插件而非内置列表。
- **DAP**：新增 `ide_dap_*` 命令 + 轻量调试面板（后续里程碑）。

> **进阶（可选、谨慎）**：若要支持极小子集的 `vscode.*` API，需起一个 Node sidecar 作
> **迷你扩展宿主**，只实现 allow-list 的几十个 API（如 `languages.registerCompletionItemProvider`）。
> 这是独立大工程，**默认不做**，列为远期可选项。

### 9.3 自带兼容性测试（Conformance Suite）

作为该特性的准入门槛。建 `tests/extension-compat/`：

```
tests/extension-compat/
├── fixtures/   # 真实 .vsix（主题 / 语法 / LSP / snippet 各几个）
├── golden/     # 期望输出（token 序列、主题色映射、hover 文本…）
└── cases/      # 测试用例 + 断言
```

**分层测试（绝大多数可在 Linux CI headless 运行）**
1. **纯数据层（无需进程，最快）**
   - 主题：转换 theme JSON → 断言关键 scope 颜色映射正确。
   - 语法：用 grammar 对样例文件 tokenization → 比对 golden scope 序列。
   - Snippet / 图标：解析 → 断言映射表。
2. **协议层（需 server 二进制）**
   - LSP：启动 server → `initialize` → 对样例文件断言 hover/completion 非空且字段正确。
   - DAP：启动适配器 → 断言断点命中事件。
3. **回归门禁**：每新增一个"已支持贡献点"，必须配套 fixture + golden，CI 红线保护，
   防止生态升级后静默回退。

**报告产物**（沿用 `swe_lite` 风格）：生成 `EXTENSION_COMPAT.md`，列出每个 fixture
在各贡献点上的 ✅/❌/N/A，形成可公开的"兼容性矩阵"。

### 9.4 风险与边界

1. **不是 VS Code**：Monaco ≠ workbench，命令式 UI 插件不支持——文档/UI 须明确"仅支持贡献点子集"。
2. **许可证**：VS Code Marketplace ToS 限制非 VS Code 产品访问其市场；应让用户**自带 `.vsix`**
   或走 **Open VSX**（开放市场，Theia / VSCodium 在用），规避法律风险。
3. **WASM 体积**：`vscode-oniguruma` 为 WASM，需评估打包体积与启动成本。
4. **沙箱**：即使只取数据，解析 `.vsix` 也应限制路径/资源，防恶意包。

**结论**：小范围兼容可行，以 `LSP + 主题 + TextMate + Snippet` 为切入点，
以 conformance suite + 兼容性矩阵作为质量门禁；完整 `vscode.*` API 兼容不做。
