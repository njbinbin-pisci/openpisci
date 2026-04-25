# OpenPisci SWE-lite

`bench_swe_lite` 是 OpenPisci 面向复杂任务 / 真实仓库修复流程的本地评测器。

它的目标不是替代 `scripts/bench_compression/`，而是补上另一条评测线：

- `bench_compression` 评估上下文压缩质量
- `bench_swe_lite` 评估真实 agent 在代码任务上的完成率、成本与 harness 行为

## 目录

- `scripts/bench_swe_lite/cases/manifest.toml`
  - 本地 lite 任务集
- `scripts/bench_swe_lite/run_swe_lite.py`
  - 主 runner
- `scripts/bench_swe_lite/score_swe_lite.py`
  - 结果汇总与 Markdown 报告
- `scripts/bench_swe_lite/review_harness.py`
  - Harness 审查报告生成器

## 任务模型

每个任务至少包含：

- `id`
- `title`
- `prompt` 或 `phases`
- `test_command`
- `task_timeout_secs`
- `test_timeout_secs`
- `seed_files` 或 `repo_source`

当前默认 manifest 使用 `seed_files` 内联一个小型本地仓库，这样不依赖外部数据集也能稳定复现。

## 对比 profile

默认包含三档：

- `baseline_pisci`
  - `openpisci --mode pisci`
- `context_lite`
  - 单代理，但关闭一组上下文注入项（memory / task state / project instructions / rolling summary / state frame）
- `experimental_pool`
  - `openpisci --mode pool`

这三档不是官方 SWE-bench 的标准分组，而是为了先验证 OpenPisci 自身 harness 的收益来源。

## 运行

先构建二进制：

```powershell
cargo build --bin openpisci --manifest-path src-tauri/Cargo.toml
cargo build --bin pisci_compact_one --manifest-path src-tauri/Cargo.toml
```

然后执行：

```powershell
py -3 scripts/bench_swe_lite/run_swe_lite.py
```

`bench_swe_lite` 会把一份 `config.json` 复制到每个隔离 `config_dir` 中。
默认会尝试读取当前用户的 OpenPisci 配置；如果找不到，可显式传入：

```powershell
py -3 scripts/bench_swe_lite/run_swe_lite.py --config-template C:\path\to\config.json
```

可选参数：

```powershell
py -3 scripts/bench_swe_lite/run_swe_lite.py --only-tasks py001_sum_even py006_jsonl_resume
py -3 scripts/bench_swe_lite/run_swe_lite.py --profiles baseline_pisci context_lite
py -3 scripts/bench_swe_lite/run_swe_lite.py --openpisci-bin target/debug/openpisci-headless.exe
```

## 结果

每次运行会生成：

- `run_results.json`
- `RESULTS.md`
- `HARNESS_REVIEW.md`
- 每题每档单独的：
  - `request.json`
  - `response.json`
  - `stdout.log`
  - `stderr.log`
  - `telemetry.json`
  - `changes.patch`
  - 测试输出日志

## 当前遥测

Runner 会从隔离 `config_dir` 中的 `pisci.db` 回收：

- `message_count`
- `rolling_summary_version`
- `rolling_summary_chars`
- `total_input_tokens`
- `total_output_tokens`
- `tool_call_count`
- `tool_error_count`
- `schema_error_count`
- `recovered_schema_error_count`

另外会把会话 transcript 回放给 `pisci_compact_one` 的 `HARNESS` 模式，补充：

- 分层 token breakdown
- `channel_utilization`

这能帮助判断真实 agent 任务结果与上下文设计之间的关系。

## 重要说明

- 这是一套 **本地 lite** 任务，不是官方 SWE-bench Full。
- 它更适合先验证 OpenPisci 自己的 harness、上下文注入与恢复机制。
- `HARNESS` breakdown 来自运行后 transcript 的后验分析，不保证与运行时每一轮请求逐字完全一致，因此它是诊断信号，不是唯一真值。
