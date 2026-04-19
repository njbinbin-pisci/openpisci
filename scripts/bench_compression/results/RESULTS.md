# Pisci 跨框架上下文压缩基准

> 运行时间：2026-04-19T20:08:21Z

## 选手

| 选手 | 类型 | 说明 |
|---|---|---|
| **Pisci-L1** | 规则（零 LLM） | 旧 ToolResult → minimal receipt，走 `build_request_messages` |
| **Pisci-L1+** | 规则（零 LLM） | L1 规则预处理（RLE/stack/ANSI/base64/table/path）+ receipt 降档 |
| **Pisci-L2** | 语义（1 LLM） | 走 `compact_summarise` 生成滚动摘要 |
| **Pisci-Harness** | 规则（零 LLM） | 完整 `ContextBuilder::finalize` 流水线，分层 token 归因 |
| **Hermes** | 语义（1+ LLM） | `hermes-agent/agent.context_compressor.ContextCompressor.compress` |
| **Engram** | 语义（2 LLM） | `claw-compactor` Observer + Reflector（重路由到 Qwen） |
| **RuleCompressor** | 规则（零 LLM） | `claw-compactor` 5 层确定性规则 |
| **RandomDrop** | 对抗基线 | `claw-compactor` 40% 保留率随机丢 token（seed=42） |
| **NoCompression** | 地面真值 | 原始对话文本 |

## 汇总（全样本平均）

| Compressor | Ratio↓ | Saved% | ROUGE-L↑ | IR-F1↑ | MI≈(b/tok)↑ | H(X|Y)↓ | ChUtil | Latency(ms) | LLM Calls |
|---|---|---|---|---|---|---|---|---|---|
| **Pisci-L1** | 0.939 | 6.2 | 0.903 | 0.992 | 6.429 | 0.060 | 0.08 | 4 | 0.0 |
| **Pisci-L1+** | 0.917 | 8.3 | 0.879 | 0.974 | 6.302 | 0.187 | 0.08 | 6 | 0.0 |
| **Pisci-Harness** | 0.939 | 6.2 | 0.903 | 0.992 | 6.429 | 0.060 | 0.08 | 5 | 0.0 |
| **RuleCompressor** | 0.960 | 4.0 | 0.912 | 0.983 | 6.366 | 0.123 | 0.08 | 6 | 0.0 |
| **RandomDrop** | 0.710 | 29.0 | 0.814 | 0.983 | 6.366 | 0.123 | 0.06 | 1 | 0.0 |

## 每样本明细

### hard-02-schema-retry-chain

| Compressor | Ratio | Saved% | ROUGE-L | IR-F1 | Lat(ms) | LLM |
|---|---|---|---|---|---|---|
| Pisci-L1 | 0.920 | 8.0 | 0.840 | 1.000 | 3 | 0 |
| Pisci-L1+ | 0.920 | 8.0 | 0.840 | 1.000 | 5 | 0 |
| Pisci-Harness | 0.920 | 8.0 | 0.840 | 1.000 | 4 | 0 |
| RuleCompressor | 0.980 | 2.0 | 0.907 | 1.000 | 7 | 0 |
| RandomDrop | 0.802 | 19.8 | 0.907 | 1.000 | 1 | 0 |

### hard-03-tool-result-flood

| Compressor | Ratio | Saved% | ROUGE-L | IR-F1 | Lat(ms) | LLM |
|---|---|---|---|---|---|---|
| Pisci-L1 | 0.957 | 4.3 | 0.967 | 0.983 | 4 | 0 |
| Pisci-L1+ | 0.914 | 8.6 | 0.918 | 0.947 | 6 | 0 |
| Pisci-Harness | 0.957 | 4.3 | 0.967 | 0.983 | 5 | 0 |
| RuleCompressor | 0.941 | 5.9 | 0.917 | 0.966 | 5 | 0 |
| RandomDrop | 0.618 | 38.2 | 0.720 | 0.966 | 0 | 0 |
