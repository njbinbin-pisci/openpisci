# Pisci 跨框架上下文压缩基准

> 运行时间：2026-04-20T08:38:02Z

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

| Compressor | Ratio↓ | Saved% | ROUGE-L↑ | IR-F1↑ | MI≈(b/tok)↑ | H(X|Y)↓ | ChUtil | Latency(ms) | LLM Calls | Judge↑ | Fano≤MI | Critical↑ | Distractor↑ |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Pisci-L1** | 0.901 | 10.0 | 0.942 | 0.936 | 7.350 | 0.538 | 0.24 | 5 | 0.0 | 4.21 | 1.947 | 4.12 | 3.75 |
| **Pisci-L1+** | 0.894 | 10.6 | 0.936 | 0.930 | 7.305 | 0.583 | 0.24 | 5 | 0.0 | 4.21 | 1.969 | 4.15 | 3.75 |
| **Pisci-L2** | 0.552 | 44.8 | 0.535 | 0.704 | 5.461 | 2.427 | 0.14 | 53549 | 0.9 | 3.43 | 1.621 | 3.42 | 3.75 |
| **Pisci-Harness** | 0.901 | 10.0 | 0.942 | 0.936 | 7.350 | 0.538 | 0.24 | 7 | 0.0 | 4.42 | 2.174 | 4.35 | 3.75 |
| **Hermes** | 1.179 | -17.9 | 0.264 | 0.652 | 5.082 | 2.806 | 0.24 | 81912 | 1.0 | 3.93 | 1.713 | 4.00 | 2.50 |
| **Engram** | 0.133 | 86.7 | 0.027 | 0.263 | 2.048 | 5.815 | 0.03 | 128357 | 2.0 | 2.69 | 0.978 | 2.58 | 5.00 |
| **RuleCompressor** | 0.913 | 8.7 | 0.872 | 0.962 | 7.580 | 0.308 | 0.24 | 12 | 0.0 | 4.38 | 2.067 | 4.33 | 5.00 |
| **RandomDrop** | 0.754 | 24.7 | 0.824 | 0.913 | 7.184 | 0.704 | 0.20 | 1 | 0.0 | 4.32 | 1.865 | 4.46 | 3.75 |
| **NoCompression** | 1.000 | 0.0 | 1.000 | 1.000 | 7.888 | 0.000 | 0.27 | 0 | 0.0 | 4.39 | 2.142 | 4.29 | 5.00 |

## 每样本明细

### hard-01-multi-day-project

| Compressor | Ratio | Saved% | ROUGE-L | IR-F1 | Lat(ms) | LLM | Judge |
|---|---|---|---|---|---|---|---|
| Pisci-L1 | 0.874 | 12.6 | 0.915 | 0.966 | 6 | 0 | 4.67 |
| Pisci-L1+ | 0.874 | 12.6 | 0.915 | 0.966 | 5 | 0 | 4.67 |
| Pisci-L2 | 0.330 | 67.1 | 0.249 | 0.732 | 54491 | 1 | 4.5 |
| Pisci-Harness | 0.874 | 12.6 | 0.915 | 0.966 | 4 | 0 | 4.33 |
| Hermes | 1.425 | -42.5 | 0.222 | 0.660 | 70712 | 1 | 2.67 |
| Engram | ERR | - | - | - | 2920 | 0 | - |
| RuleCompressor | 0.955 | 4.5 | 0.860 | 0.983 | 10 | 0 | 4.67 |
| RandomDrop | 0.617 | 38.3 | 0.745 | 0.800 | 1 | 0 | 5.0 |
| NoCompression | 1.000 | 0.0 | 1.000 | 1.000 | 0 | 0 | 4.67 |

### hard-02-schema-retry-chain

| Compressor | Ratio | Saved% | ROUGE-L | IR-F1 | Lat(ms) | LLM | Judge |
|---|---|---|---|---|---|---|---|
| Pisci-L1 | 0.920 | 8.0 | 0.840 | 1.000 | 4 | 0 | 4.33 |
| Pisci-L1+ | 0.920 | 8.0 | 0.840 | 1.000 | 3 | 0 | 4.83 |
| Pisci-L2 | 0.698 | 30.2 | 0.374 | 0.845 | 51313 | 1 | 5.0 |
| Pisci-Harness | 0.920 | 8.0 | 0.840 | 1.000 | 4 | 0 | 5.0 |
| Hermes | 2.429 | -143.0 | 0.348 | 0.800 | 45586 | 1 | 5.0 |
| Engram | 0.293 | 70.7 | 0.023 | 0.325 | 78925 | 2 | 2.67 |
| RuleCompressor | 0.980 | 2.0 | 0.907 | 1.000 | 3 | 0 | 3.67 |
| RandomDrop | 0.802 | 19.8 | 0.907 | 1.000 | 0 | 0 | 4.17 |
| NoCompression | 1.000 | 0.0 | 1.000 | 1.000 | 0 | 0 | 4.33 |

### hard-03-tool-result-flood

| Compressor | Ratio | Saved% | ROUGE-L | IR-F1 | Lat(ms) | LLM | Judge |
|---|---|---|---|---|---|---|---|
| Pisci-L1 | 0.957 | 4.3 | 0.967 | 0.983 | 5 | 0 | 4.0 |
| Pisci-L1+ | 0.914 | 8.6 | 0.918 | 0.947 | 4 | 0 | 4.0 |
| Pisci-L2 | 0.904 | 9.6 | 0.852 | 0.832 | 40155 | 1 | 2.67 |
| Pisci-Harness | 0.957 | 4.3 | 0.967 | 0.983 | 4 | 0 | 4.17 |
| Hermes | 2.009 | -100.9 | 0.460 | 0.846 | 77791 | 1 | 4.0 |
| Engram | 0.141 | 85.9 | 0.033 | 0.250 | 85480 | 2 | 4.0 |
| RuleCompressor | 0.941 | 5.9 | 0.917 | 0.966 | 9 | 0 | 4.0 |
| RandomDrop | 0.618 | 38.2 | 0.720 | 0.966 | 1 | 0 | 4.33 |
| NoCompression | 1.000 | 0.0 | 1.000 | 1.000 | 0 | 0 | 3.83 |

### hard-04-cross-session-recall

| Compressor | Ratio | Saved% | ROUGE-L | IR-F1 | Lat(ms) | LLM | Judge |
|---|---|---|---|---|---|---|---|
| Pisci-L1 | 0.826 | 17.4 | 0.884 | 0.967 | 5 | 0 | 0.0 |
| Pisci-L1+ | 0.804 | 19.6 | 0.875 | 0.967 | 3 | 0 | 0.0 |
| Pisci-L2 | 0.826 | 17.4 | 0.884 | 0.967 | 5018 | 0 | 0.0 |
| Pisci-Harness | 0.826 | 17.4 | 0.884 | 0.967 | 4 | 0 | 0.0 |
| Hermes | 1.256 | -25.6 | 0.497 | 0.625 | 71318 | 1 | 0.0 |
| Engram | ERR | - | - | - | 5031 | 0 | - |
| RuleCompressor | 0.950 | 5.0 | 0.649 | 1.000 | 3 | 0 | 4.67 |
| RandomDrop | 0.818 | 18.2 | 0.866 | 0.842 | 0 | 0 | 2.33 |
| NoCompression | 1.000 | 0.0 | 1.000 | 1.000 | 0 | 0 | 4.83 |

### sample-01-devops

| Compressor | Ratio | Saved% | ROUGE-L | IR-F1 | Lat(ms) | LLM | Judge |
|---|---|---|---|---|---|---|---|
| Pisci-L1 | 0.903 | 9.7 | 0.981 | 0.867 | 6 | 0 | 5.0 |
| Pisci-L1+ | 0.903 | 9.7 | 0.981 | 0.867 | 8 | 0 | 5.0 |
| Pisci-L2 | 0.492 | 50.8 | 0.587 | 0.570 | 53192 | 1 | 5.0 |
| Pisci-Harness | 0.903 | 9.7 | 0.981 | 0.867 | 31 | 0 | 5.0 |
| Hermes | 0.554 | 44.6 | 0.160 | 0.659 | 114689 | 1 | 5.0 |
| Engram | ERR | - | - | - | 26240 | 0 | - |
| RuleCompressor | 0.882 | 11.8 | 0.904 | 0.966 | 29 | 0 | 5.0 |
| RandomDrop | 0.847 | 15.3 | 0.907 | 0.947 | 1 | 0 | 5.0 |
| NoCompression | 1.000 | 0.0 | 1.000 | 1.000 | 0 | 0 | 5.0 |

### sample-02-trading

| Compressor | Ratio | Saved% | ROUGE-L | IR-F1 | Lat(ms) | LLM | Judge |
|---|---|---|---|---|---|---|---|
| Pisci-L1 | 0.923 | 7.7 | 0.985 | 0.900 | 4 | 0 | 3.5 |
| Pisci-L1+ | 0.923 | 7.7 | 0.985 | 0.900 | 6 | 0 | 5.0 |
| Pisci-L2 | 0.640 | 36.0 | 0.688 | 0.670 | 56381 | 1 | 0.0 |
| Pisci-Harness | 0.923 | 7.7 | 0.985 | 0.900 | 5 | 0 | 5.0 |
| Hermes | 0.777 | 22.3 | 0.179 | 0.566 | 100562 | 1 | 3.5 |
| Engram | 0.107 | 89.3 | 0.019 | 0.167 | 159691 | 2 | 0.0 |
| RuleCompressor | 0.862 | 13.8 | 0.887 | 0.947 | 20 | 0 | 0.5 |
| RandomDrop | 0.763 | 23.7 | 0.847 | 0.846 | 1 | 0 | 3.0 |
| NoCompression | 1.000 | 0.0 | 1.000 | 1.000 | 0 | 0 | 0.0 |

### sample-03-ml-short

| Compressor | Ratio | Saved% | ROUGE-L | IR-F1 | Lat(ms) | LLM | Judge |
|---|---|---|---|---|---|---|---|
| Pisci-L1 | 0.907 | 9.3 | 0.982 | 0.900 | 3 | 0 | 5.0 |
| Pisci-L1+ | 0.907 | 9.3 | 0.982 | 0.900 | 6 | 0 | 5.0 |
| Pisci-L2 | 0.238 | 76.2 | 0.228 | 0.377 | 56152 | 1 | 0.5 |
| Pisci-Harness | 0.907 | 9.3 | 0.982 | 0.900 | 4 | 0 | 5.0 |
| Hermes | 1.021 | -2.1 | 0.231 | 0.554 | 76926 | 1 | 5.0 |
| Engram | 0.130 | 87.0 | 0.020 | 0.200 | 140729 | 2 | 1.5 |
| RuleCompressor | 0.940 | 6.0 | 0.941 | 0.947 | 11 | 0 | 5.0 |
| RandomDrop | 0.784 | 21.6 | 0.836 | 0.889 | 1 | 0 | 5.0 |
| NoCompression | 1.000 | 0.0 | 1.000 | 1.000 | 0 | 0 | 5.0 |

### sample-04-mixed-long

| Compressor | Ratio | Saved% | ROUGE-L | IR-F1 | Lat(ms) | LLM | Judge |
|---|---|---|---|---|---|---|---|
| Pisci-L1 | 0.923 | 7.7 | 0.984 | 0.867 | 4 | 0 | 4.0 |
| Pisci-L1+ | 0.923 | 7.7 | 0.984 | 0.867 | 8 | 0 | 4.0 |
| Pisci-L2 | 0.510 | 49.0 | 0.551 | 0.562 | 72216 | 1 | 5.0 |
| Pisci-Harness | 0.923 | 7.7 | 0.984 | 0.867 | 5 | 0 | 4.5 |
| Hermes | 0.638 | 36.2 | 0.146 | 0.500 | 84086 | 1 | 5.0 |
| Engram | 0.088 | 91.2 | 0.024 | 0.210 | 122914 | 2 | 2.5 |
| RuleCompressor | 0.933 | 6.7 | 0.939 | 0.966 | 22 | 0 | 5.0 |
| RandomDrop | 0.740 | 26.0 | 0.826 | 0.889 | 1 | 0 | 4.0 |
| NoCompression | 1.000 | 0.0 | 1.000 | 1.000 | 0 | 0 | 5.0 |

### sample-05-sysadmin

| Compressor | Ratio | Saved% | ROUGE-L | IR-F1 | Lat(ms) | LLM | Judge |
|---|---|---|---|---|---|---|---|
| Pisci-L1 | 0.915 | 8.5 | 0.984 | 0.900 | 4 | 0 | 5.0 |
| Pisci-L1+ | 0.915 | 8.5 | 0.984 | 0.900 | 6 | 0 | 5.0 |
| Pisci-L2 | 0.656 | 34.4 | 0.717 | 0.709 | 82554 | 1 | 5.0 |
| Pisci-Harness | 0.915 | 8.5 | 0.984 | 0.900 | 4 | 0 | 5.0 |
| Hermes | 0.734 | 26.6 | 0.174 | 0.598 | 96951 | 1 | 4.5 |
| Engram | 0.092 | 90.8 | 0.027 | 0.348 | 235459 | 2 | 0.0 |
| RuleCompressor | 0.932 | 6.8 | 0.944 | 0.966 | 16 | 0 | 5.0 |
| RandomDrop | 0.793 | 20.7 | 0.843 | 0.983 | 1 | 0 | 4.5 |
| NoCompression | 1.000 | 0.0 | 1.000 | 1.000 | 0 | 0 | 5.0 |

### tool-01-bug-hunt

| Compressor | Ratio | Saved% | ROUGE-L | IR-F1 | Lat(ms) | LLM | Judge |
|---|---|---|---|---|---|---|---|
| Pisci-L1 | 0.872 | 12.9 | 0.914 | 0.950 | 4 | 0 | 5.0 |
| Pisci-L1+ | 0.872 | 12.9 | 0.914 | 0.950 | 5 | 0 | 3.0 |
| Pisci-L2 | 0.406 | 59.4 | 0.402 | 0.800 | 59543 | 1 | 3.5 |
| Pisci-Harness | 0.872 | 12.9 | 0.914 | 0.950 | 5 | 0 | 5.0 |
| Hermes | 0.902 | 9.8 | 0.200 | 0.629 | 71440 | 1 | 3.0 |
| Engram | 0.092 | 90.8 | 0.019 | 0.205 | 107804 | 2 | 4.5 |
| RuleCompressor | 0.849 | 15.1 | 0.836 | 0.947 | 10 | 0 | 5.0 |
| RandomDrop | 0.740 | 26.0 | 0.784 | 0.929 | 1 | 0 | 5.0 |
| NoCompression | 1.000 | 0.0 | 1.000 | 1.000 | 0 | 0 | 5.0 |

### tool-02-data-pipeline

| Compressor | Ratio | Saved% | ROUGE-L | IR-F1 | Lat(ms) | LLM | Judge |
|---|---|---|---|---|---|---|---|
| Pisci-L1 | 0.857 | 14.3 | 0.907 | 0.967 | 5 | 0 | 5.0 |
| Pisci-L1+ | 0.857 | 14.3 | 0.907 | 0.967 | 4 | 0 | 5.0 |
| Pisci-L2 | 0.456 | 54.4 | 0.466 | 0.800 | 55307 | 1 | 5.0 |
| Pisci-Harness | 0.857 | 14.3 | 0.907 | 0.967 | 6 | 0 | 5.0 |
| Hermes | 1.232 | -23.2 | 0.274 | 0.696 | 96502 | 1 | 5.0 |
| Engram | 0.122 | 87.8 | 0.032 | 0.295 | 104810 | 2 | 5.0 |
| RuleCompressor | 0.809 | 19.0 | 0.779 | 0.909 | 7 | 0 | 5.0 |
| RandomDrop | 0.821 | 17.9 | 0.886 | 0.929 | 0 | 0 | 5.0 |
| NoCompression | 1.000 | 0.0 | 1.000 | 1.000 | 0 | 0 | 5.0 |

### tool-03-browser-research

| Compressor | Ratio | Saved% | ROUGE-L | IR-F1 | Lat(ms) | LLM | Judge |
|---|---|---|---|---|---|---|---|
| Pisci-L1 | 0.929 | 7.1 | 0.966 | 0.967 | 3 | 0 | 5.0 |
| Pisci-L1+ | 0.912 | 8.8 | 0.944 | 0.932 | 4 | 0 | 5.0 |
| Pisci-L2 | 0.466 | 53.4 | 0.427 | 0.579 | 56260 | 1 | 5.0 |
| Pisci-Harness | 0.929 | 7.1 | 0.966 | 0.967 | 5 | 0 | 5.0 |
| Hermes | 1.170 | -17.0 | 0.281 | 0.687 | 76376 | 1 | 4.5 |
| Engram | 0.133 | 86.7 | 0.043 | 0.365 | 119406 | 2 | 4.0 |
| RuleCompressor | 0.928 | 7.2 | 0.901 | 0.947 | 7 | 0 | 5.0 |
| RandomDrop | 0.700 | 30.1 | 0.723 | 0.932 | 1 | 0 | 4.5 |
| NoCompression | 1.000 | 0.0 | 1.000 | 1.000 | 0 | 0 | 5.0 |

## 失败条目（errors）

- `sample-01-devops` / `Engram`: RemoteDisconnected: Remote end closed connection without response
- `hard-01-multi-day-project` / `Engram`: RuntimeError: LLM proxy unavailable: <urlopen error [SSL: UNEXPECTED_EOF_WHILE_READING] EOF occurred in violation of protocol (_ssl.c:1006)>
- `hard-04-cross-session-recall` / `Engram`: RuntimeError: LLM proxy unavailable: <urlopen error [SSL: UNEXPECTED_EOF_WHILE_READING] EOF occurred in violation of protocol (_ssl.c:1006)>