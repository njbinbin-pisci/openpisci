# Cross-Framework Context Compression Benchmark

在**同一批样本**、**同一把尺**、**同一个 LLM（Qwen via Pisci 的 config）** 下，对比 7 种压缩策略：

| 选手 | 类型 | LLM 调用 |
|---|---|---|
| Pisci-L1 | 规则：receipt demotion | 0 |
| Pisci-L2 | 语义：滚动摘要 | 1 |
| Hermes | 语义：token-budget tail + 迭代摘要 | 1+ |
| Engram | 语义：Observer + Reflector | 2 |
| RuleCompressor | 规则：5 层确定性 | 0 |
| RandomDrop | 对抗：40% 保留率随机丢 token | 0 |
| NoCompression | 地面真值 | 0 |

## 依赖

- 已构建好的 `target/debug/pisci_compact_one.exe`（`cargo build -p pisci-cli --features bench-compact-cli --bin pisci_compact_one --manifest-path src-tauri/Cargo.toml`）
- Python 3.9+：`pip install --user openai pyyaml tiktoken`
- Pisci 的 `config.json` 里已配好 Qwen key（用于所有 LLM 压缩器和 judge）

## 运行

```powershell
cd scripts\bench_compression
py -3 run_bench.py                            # 纯规则 + 文本相似度指标
py -3 run_bench.py --with-judge               # 加 LLM-as-judge 下游保真度
py -3 run_bench.py --only Pisci-L2 Hermes     # 只跑两家对比
py -3 run_bench.py --samples sample_03_ml_short tool_01_bug_hunt    # 小范围
```

## 样本

- `references/claw-compactor/benchmark/data/sample_0[1-5]*.json` — 5 个长对话（纯文本，1.8k–4.6k tokens，合成）
- `scripts/bench_compression/samples/tool_0[1-3]*.json` — 3 个工具密集会话（含 `tool_use` / `tool_result`，用于触发 Pisci-L1）

## 指标

| 指标 | 定义 | 方向 |
|---|---|---|
| `compression_ratio` | compressed_tokens / original_tokens | ↓ 越小越压得狠 |
| `space_saving_pct` | (1 − ratio) × 100 | ↑ 越大越好 |
| `rouge_l_f1` | LCS 字符串重叠 F1 | ↑ 文本保真 |
| `info_retention_f1` | top-30 关键词召回 F1 | ↑ 信息保留 |
| `latency_ms` | 压缩墙钟 | ↓ |
| `llm_calls` | LLM 调用次数 | ↓ |
| `judge_score` | Qwen judge 0–5 分 | ↑ **下游真实保真度** |

## 架构

```
src-tauri/src/bin/pisci_compact_one.rs           ← Rust 一次性压缩 CLI
src-tauri/src/agent/bench_compact.rs             ← Pisci compaction 的 JSON facade
scripts/bench_compression/
├─ run_bench.py                                   ← 编排器
├─ judge.py                                       ← LLM-as-judge
├─ adapters/
│  ├─ pisci.py                                    ← subprocess → Rust bin
│  ├─ hermes.py                                   ← import hermes-agent
│  ├─ claw.py                                     ← reuse claw-compactor (含 Engram reroute)
│  └─ common.py                                   ← 从 --print-runtime 自动提取 Qwen runtime
├─ samples/tool_0[1-3]*.json                      ← 工具密集样本
└─ results/
   ├─ benchmark_results.json                      ← 全量原始数据
   ├─ RESULTS.md                                  ← 自动生成汇总
   └─ per_sample/                                 ← （可扩展）按样本详单
```
