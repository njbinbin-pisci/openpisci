# OpenPisci Headless CLI

OpenPisci 面向自动化与 benchmark 只发布一个 headless 入口：

| 二进制               | 所属 crate  | 用途                                      |
|----------------------|-------------|-------------------------------------------|
| `openpisci-headless` | `pisci-cli` | 无需 Tauri UI 的 kernel 驱动 CLI / 评测 / 自动化宿主 |

`openpisci-headless` 支持 `chat`、`run`、`rpc`、`capabilities`、`version` 等子命令。它不是桌面 GUI 的必需 sidecar；桌面主聊天与默认 Koi 协同运行在 `pisci-desktop` 主进程内。

## 构建

在仓库 `src-tauri/` 目录下：

```powershell
cargo build -p pisci-cli --bin openpisci-headless
```

构建产物写入 `target/{debug,release}/`。GUI 打包不会强制复制或绑定该二进制；需要 CLI / CI / 评测资产时可以单独发布它。

## 运行模式

### `pisci`

单代理基线模式，适合作为 benchmark 的稳定基线：

- 禁用 `call_koi`
- 禁用 `pool_org`
- 禁用 `pool_chat`
- 禁用 `chat_ui`
- 在非 Windows 平台额外禁用 `office`、`powershell_query`、`wmi`、`uia`、`screen_capture`、`com`、`com_invoke`

### `pool`

协作模式，Pisci 作为项目协调者：

- 保留 `pool_org`
- 保留 `pool_chat`
- 允许在项目池内协调 Koi 工作
- 仍禁用 `chat_ui`
- 在非 Windows 平台同样裁剪 Windows-only 工具

建议 benchmark 分两档统计：

- `baseline-single-agent` -> `--mode pisci`
- `experimental-pool` -> `--mode pool`

## CLI 速查

### 查看能力矩阵

```powershell
target\debug\openpisci-headless.exe capabilities --mode pisci
target\debug\openpisci-headless.exe capabilities --mode pool
```

### 直接运行

```powershell
target\debug\openpisci-headless.exe run --prompt "请总结当前仓库结构" --workspace C:\repo --mode pisci
```

### 从 JSON 文件运行

```powershell
target\debug\openpisci-headless.exe run --input request.json --output result.json
```

## `run` 请求协议

`run --input` 接收一个 JSON 对象，字段如下：

```json
{
  "prompt": "string, required",
  "workspace": "string, optional",
  "mode": "pisci | pool, optional, default=pisci",
  "session_id": "string, optional",
  "session_title": "string, optional",
  "channel": "string, optional",
  "config_dir": "string, optional",
  "pool_id": "string, optional",
  "pool_name": "string, optional",
  "pool_size": "number, optional",
  "koi_ids": ["string"],
  "task_timeout_secs": "number, optional",
  "wait_for_completion": "boolean, optional",
  "wait_timeout_secs": "number, optional",
  "extra_system_context": "string, optional",
  "context_toggles": {
    "disable_memory_context": "boolean, optional",
    "disable_task_state_context": "boolean, optional",
    "disable_pool_context": "boolean, optional",
    "disable_project_instructions": "boolean, optional",
    "disable_rolling_summary": "boolean, optional",
    "disable_state_frame": "boolean, optional"
  },
  "output": "string, optional"
}
```

字段说明：

- `workspace`: 临时覆写本次运行的 workspace root，不改用户持久设置
- `config_dir`: 本次运行使用的隔离 app-data 目录；其中会读取/写入 `config.json`、`pisci.db`
- `pool_id`: 复用已有 pool
- `pool_name`: `pool_id` 不提供时用于创建新 pool
- `pool_size`: 给协调提示词的目标规模提示，不直接强制限制运行时线程数
- `koi_ids`: 给协调提示词的优先候选 Koi 列表
- `wait_for_completion`: 仅在 `pool` 模式下有意义；为 `true` 时等待 pool 收敛
- `wait_timeout_secs`: pool 等待超时，默认 900 秒
- `context_toggles`: benchmark / ablation 用的上下文开关；用于关闭 memory、task state、project instructions、rolling summary、state frame 等注入来源，便于比较不同 harness 配置

## `run` 响应协议

`run` 会输出如下 JSON：

```json
{
  "ok": true,
  "mode": "pisci",
  "session_id": "headless_cli_123",
  "pool_id": null,
  "response_text": "assistant final response",
  "disabled_tools": [
    {
      "name": "call_koi",
      "reason": "Disabled in headless pisci mode: single-agent baseline should not delegate to Koi."
    }
  ],
  "pool_wait": null
}
```

当 `mode=pool` 且 `wait_for_completion=true` 时，`pool_wait` 形如：

```json
{
  "completed": true,
  "timed_out": false,
  "active_todos": 0,
  "done_todos": 3,
  "cancelled_todos": 0,
  "blocked_todos": 0,
  "latest_messages": [
    "#41 pisci (text): 已完成整体验收"
  ]
}
```

## Benchmark 最小示例

### 单代理基线

```json
{
  "prompt": "修复当前仓库中的 failing test，并给出最终说明。",
  "workspace": "/workspace/repo",
  "mode": "pisci",
  "session_id": "swebench_case_001",
  "session_title": "SWE-bench Case 001",
  "channel": "benchmark",
  "config_dir": "/tmp/openpisci-swebench-case-001"
}
```

### Pool 实验档

```json
{
  "prompt": "修复当前仓库中的 failing test，并协调合适的 Koi 分工处理。",
  "workspace": "/workspace/repo",
  "mode": "pool",
  "session_id": "swebench_case_001_pool",
  "session_title": "SWE-bench Case 001 Pool",
  "channel": "benchmark",
  "config_dir": "/tmp/openpisci-swebench-case-001-pool",
  "pool_name": "SWE-bench Case 001",
  "pool_size": 3,
  "wait_for_completion": true,
  "wait_timeout_secs": 1800
}
```

## 说明

- `capabilities` 适合在 benchmark harness 启动前记录当前平台可用工具矩阵
- `disabled_tools` 应被视为本次运行的真实能力边界，而不是仅供展示
- 当前 Linux/macOS 目标是 headless/CLI 运行，不包含完整桌面 GUI 等价能力
