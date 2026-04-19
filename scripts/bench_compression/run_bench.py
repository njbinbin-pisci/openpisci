"""run_bench.py — Cross-framework context-compression benchmark orchestrator.

Runs 6 compressors against 8 samples (5 reference from claw-compactor +
3 tool-heavy Pisci-native) and collects:

  • compression_ratio / space_saving_pct
  • ROUGE-L F1 (LCS-based text fidelity)
  • Info Retention F1 (top-30 keyword overlap)
  • latency_ms
  • llm_calls
  • judge_score (0–5)  ← optional via --with-judge

Writes results/{benchmark_results.json, RESULTS.md, per_sample/*.md}.
"""

from __future__ import annotations

import argparse
import json
import logging
import sys
import time
from collections import defaultdict
from pathlib import Path

# Make the adapters package importable regardless of cwd
sys.path.insert(0, str(Path(__file__).resolve().parent))

from adapters.common import BENCH_DIR, CLAW_BENCH_DIR, CompressorResult, find_pisci_bin
from adapters import claw as claw_adp
from adapters import hermes as hermes_adp
from adapters import pisci as pisci_adp

# Make claw's benchmark package importable (for evaluate)
if str(CLAW_BENCH_DIR.parent) not in sys.path:
    sys.path.insert(0, str(CLAW_BENCH_DIR.parent))

from benchmark.evaluate import rouge_l, information_retention_f1, messages_to_text, estimate_tokens  # type: ignore
from info_theory import summarise as info_theory_summarise  # type: ignore


def flatten_full(messages: list[dict]) -> str:
    """Block-aware flattener: preserves tool_use / tool_result payload so
    tool-heavy samples are not under-counted (claw's messages_to_text only
    looks at `content`)."""
    parts = []
    for m in messages:
        role = (m.get("role") or "?").upper()
        ts = m.get("ts") or ""
        payload_parts = []
        if m.get("content"):
            payload_parts.append(str(m["content"]))
        for b in m.get("blocks", []) or []:
            t = b.get("type")
            if t == "text":
                payload_parts.append(b.get("text", ""))
            elif t == "tool_use":
                payload_parts.append(
                    f"[tool_use:{b.get('name','?')}] "
                    + json.dumps(b.get("input", {}), ensure_ascii=False)
                )
            elif t == "tool_result":
                payload_parts.append("[tool_result] " + str(b.get("content", "")))
        payload = "\n".join(p for p in payload_parts if p)
        if ts:
            parts.append(f"[{ts}] {role}: {payload}")
        else:
            parts.append(f"{role}: {payload}")
    return "\n\n".join(parts)


def _messages_for_text_compressors(messages: list[dict]) -> list[dict]:
    """Flatten tool_use/tool_result blocks into `content` so claw's text-only
    compressors (which ignore `blocks`) can still see the tool payload. This
    makes the comparison fair — otherwise they'd produce fake high ROUGE-L
    on tool-heavy samples because they effectively saw an empty conversation.
    """
    out = []
    for m in messages:
        new = {"role": m.get("role", "?")}
        if m.get("ts"):
            new["ts"] = m["ts"]
        payload_parts = []
        if m.get("content"):
            payload_parts.append(str(m["content"]))
        for b in m.get("blocks", []) or []:
            t = b.get("type")
            if t == "text":
                payload_parts.append(b.get("text", ""))
            elif t == "tool_use":
                payload_parts.append(
                    f"[tool_use:{b.get('name','?')}] "
                    + json.dumps(b.get("input", {}), ensure_ascii=False)
                )
            elif t == "tool_result":
                payload_parts.append("[tool_result] " + str(b.get("content", "")))
        new["content"] = "\n".join(p for p in payload_parts if p)
        out.append(new)
    return out

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
)
log = logging.getLogger("bench")

SAMPLES_CLAW = [
    "sample_01_devops.json",
    "sample_02_trading.json",
    "sample_03_ml_short.json",
    "sample_04_mixed_long.json",
    "sample_05_sysadmin.json",
]
SAMPLES_TOOL = [
    "tool_01_bug_hunt.json",
    "tool_02_data_pipeline.json",
    "tool_03_browser_research.json",
]

SAMPLES_HARD = [
    "hard_01_multi_day_project.json",
    "hard_02_schema_retry_chain.json",
    "hard_03_tool_result_flood.json",
    "hard_04_cross_session_recall.json",
]


def load_sample(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def collect_compressed_result(
    result: CompressorResult,
    original_text: str,
    with_judge: bool,
    messages: list[dict],
    sample: dict | None = None,
) -> dict:
    """Augment a CompressorResult with the evaluate.py text-quality metrics."""
    # Re-compute tokens in Python with the *same* estimator for every
    # compressor, using the block-aware flattened original_text as the
    # reference. This kills cross-compressor accounting drift.
    orig_tokens_py = estimate_tokens(original_text)
    comp_tokens_py = estimate_tokens(result.compressed_text) if not result.error else 0

    out: dict = {
        "sample_id": result.sample_id,
        "compressor": result.compressor,
        "error": result.error,
        "original_tokens": orig_tokens_py,
        "compressed_tokens": comp_tokens_py,
        "original_tokens_rust": result.original_tokens,
        "compressed_tokens_rust": result.compressed_tokens,
        "latency_ms": round(result.latency_ms, 1),
        "llm_calls": result.llm_calls,
        "llm_input_tokens": result.llm_input_tokens,
        "llm_output_tokens": result.llm_output_tokens,
        "notes": result.notes,
    }

    if result.error:
        out.update(
            compression_ratio=1.0,
            space_saving_pct=0.0,
            rouge_l_f1=0.0,
            info_retention_f1=0.0,
        )
        return out

    ratio = comp_tokens_py / orig_tokens_py if orig_tokens_py else 1.0
    out["compression_ratio"] = round(ratio, 4)
    out["space_saving_pct"] = round((1.0 - ratio) * 100, 1)
    # Persist the full compressed text so the judge pass can reuse it
    # without re-running compression.
    out["compressed_text"] = result.compressed_text

    rl = rouge_l(original_text, result.compressed_text)
    ir = information_retention_f1(original_text, result.compressed_text)
    out["rouge_l_f1"] = rl.get("f1", 0.0)
    out["info_retention_f1"] = ir.get("f1", 0.0)
    out["compressed_preview"] = result.compressed_text[:240].replace("\n", " ⏎ ")

    judge_avg_for_fano: float | None = None
    if with_judge:
        from judge import score_compression

        try:
            log.info("  judge scoring %s/%s ...", result.sample_id, result.compressor)
            scored = score_compression(
                result.sample_id, messages, result.compressed_text, sample
            )
            out["judge_score"] = round(scored["avg_score"], 2)
            out["judge_per_question"] = scored["per_question"]
            if scored.get("per_kind"):
                out["judge_per_kind"] = scored["per_kind"]
            judge_avg_for_fano = scored["avg_score"]
        except Exception as e:
            out["judge_score"] = 0.0
            out["judge_error"] = f"{type(e).__name__}: {e}"

    # Information-theory surrogates (always computed; Fano needs judge).
    try:
        out["info_theory"] = info_theory_summarise(
            original_text=original_text,
            compressed_text=result.compressed_text,
            ir_f1=out["info_retention_f1"],
            judge_score_0_5=judge_avg_for_fano,
            compressed_tokens=comp_tokens_py,
            budget_tokens=8192,
        )
    except Exception as e:
        out["info_theory_error"] = f"{type(e).__name__}: {e}"

    return out


def render_summary(results: list[dict], with_judge: bool) -> str:
    """Render a markdown summary table like claw-compactor's RESULTS.md."""
    by_comp: dict[str, list[dict]] = defaultdict(list)
    for r in results:
        if "error" in r and r["error"]:
            continue
        by_comp[r["compressor"]].append(r)

    lines = [
        "# Pisci 跨框架上下文压缩基准",
        "",
        f"> 运行时间：{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}",
        "",
        "## 选手",
        "",
        "| 选手 | 类型 | 说明 |",
        "|---|---|---|",
        "| **Pisci-L1** | 规则（零 LLM） | 旧 ToolResult → minimal receipt，走 `build_request_messages` |",
        "| **Pisci-L1+** | 规则（零 LLM） | L1 规则预处理（RLE/stack/ANSI/base64/table/path）+ receipt 降档 |",
        "| **Pisci-L2** | 语义（1 LLM） | 走 `compact_summarise` 生成滚动摘要 |",
        "| **Pisci-Harness** | 规则（零 LLM） | 完整 `ContextBuilder::finalize` 流水线，分层 token 归因 |",
        "| **Hermes** | 语义（1+ LLM） | `hermes-agent/agent.context_compressor.ContextCompressor.compress` |",
        "| **Engram** | 语义（2 LLM） | `claw-compactor` Observer + Reflector（重路由到 Qwen） |",
        "| **RuleCompressor** | 规则（零 LLM） | `claw-compactor` 5 层确定性规则 |",
        "| **RandomDrop** | 对抗基线 | `claw-compactor` 40% 保留率随机丢 token（seed=42） |",
        "| **NoCompression** | 地面真值 | 原始对话文本 |",
        "",
        "## 汇总（全样本平均）",
        "",
    ]

    cols = [
        "Ratio↓",
        "Saved%",
        "ROUGE-L↑",
        "IR-F1↑",
        "MI≈(b/tok)↑",
        "H(X|Y)↓",
        "ChUtil",
        "Latency(ms)",
        "LLM Calls",
    ]
    if with_judge:
        cols.append("Judge↑")
        cols.append("Fano≤MI")
        cols.append("Critical↑")
        cols.append("Distractor↑")
    lines.append("| Compressor | " + " | ".join(cols) + " |")
    lines.append("|" + "---|" * (len(cols) + 1))

    PREFERRED_ORDER = [
        "Pisci-L1",
        "Pisci-L1+",
        "Pisci-L2",
        "Pisci-Harness",
        "Hermes",
        "Engram",
        "RuleCompressor",
        "RandomDrop",
        "NoCompression",
    ]

    def sort_key(name: str) -> int:
        try:
            return PREFERRED_ORDER.index(name)
        except ValueError:
            return 999

    def _it(r: dict, key: str) -> float:
        it = r.get("info_theory") or {}
        v = it.get(key)
        return float(v) if isinstance(v, (int, float)) else 0.0

    def _kind(r: dict, kind: str) -> float | None:
        pk = r.get("judge_per_kind") or {}
        v = pk.get(kind)
        if isinstance(v, dict) and isinstance(v.get("avg"), (int, float)):
            return float(v["avg"])
        return None

    for name in sorted(by_comp.keys(), key=sort_key):
        runs = by_comp[name]
        avg_ratio = sum(r["compression_ratio"] for r in runs) / len(runs)
        avg_saved = sum(r["space_saving_pct"] for r in runs) / len(runs)
        avg_rl = sum(r["rouge_l_f1"] for r in runs) / len(runs)
        avg_ir = sum(r["info_retention_f1"] for r in runs) / len(runs)
        avg_lat = sum(r["latency_ms"] for r in runs) / len(runs)
        avg_llm = sum(r["llm_calls"] for r in runs) / len(runs)
        avg_mi = sum(_it(r, "mi_approx_bits_per_tok") for r in runs) / len(runs)
        avg_hcond = sum(_it(r, "h_cond_bits_per_tok") for r in runs) / len(runs)
        avg_util = sum(_it(r, "channel_utilization") for r in runs) / len(runs)
        row = [
            f"{avg_ratio:.3f}",
            f"{avg_saved:.1f}",
            f"{avg_rl:.3f}",
            f"{avg_ir:.3f}",
            f"{avg_mi:.3f}",
            f"{avg_hcond:.3f}",
            f"{avg_util:.2f}",
            f"{avg_lat:.0f}",
            f"{avg_llm:.1f}",
        ]
        if with_judge:
            scored_runs = [r for r in runs if "judge_score" in r]
            avg_j = (
                sum(r["judge_score"] for r in scored_runs) / len(scored_runs)
                if scored_runs
                else 0.0
            )
            fano_runs = [
                _it(r, "fano_lower_bound_mi_bits")
                for r in scored_runs
                if (r.get("info_theory") or {}).get("fano_lower_bound_mi_bits") is not None
            ]
            avg_fano = sum(fano_runs) / len(fano_runs) if fano_runs else 0.0
            crit_vals = [v for v in (_kind(r, "critical") for r in scored_runs) if v is not None]
            dist_vals = [v for v in (_kind(r, "distractor") for r in scored_runs) if v is not None]
            avg_crit = sum(crit_vals) / len(crit_vals) if crit_vals else None
            avg_dist = sum(dist_vals) / len(dist_vals) if dist_vals else None
            row.append(f"{avg_j:.2f}")
            row.append(f"{avg_fano:.3f}")
            row.append(f"{avg_crit:.2f}" if avg_crit is not None else "-")
            row.append(f"{avg_dist:.2f}" if avg_dist is not None else "-")
        lines.append(f"| **{name}** | " + " | ".join(row) + " |")

    # Per-sample breakdowns
    lines += ["", "## 每样本明细", ""]
    by_sample: dict[str, list[dict]] = defaultdict(list)
    for r in results:
        by_sample[r["sample_id"]].append(r)
    for sid in sorted(by_sample.keys()):
        lines += [f"### {sid}", ""]
        sample_runs = by_sample[sid]
        cols = ["Compressor", "Ratio", "Saved%", "ROUGE-L", "IR-F1", "Lat(ms)", "LLM"]
        if with_judge:
            cols.append("Judge")
        lines.append("| " + " | ".join(cols) + " |")
        lines.append("|" + "---|" * len(cols))
        for r in sorted(sample_runs, key=lambda x: sort_key(x["compressor"])):
            if r.get("error"):
                line = [
                    r["compressor"],
                    "ERR",
                    "-",
                    "-",
                    "-",
                    f"{r['latency_ms']:.0f}",
                    f"{r['llm_calls']}",
                ]
            else:
                line = [
                    r["compressor"],
                    f"{r['compression_ratio']:.3f}",
                    f"{r['space_saving_pct']:.1f}",
                    f"{r['rouge_l_f1']:.3f}",
                    f"{r['info_retention_f1']:.3f}",
                    f"{r['latency_ms']:.0f}",
                    f"{r['llm_calls']}",
                ]
            if with_judge:
                line.append(f"{r.get('judge_score', '-')}")
            lines.append("| " + " | ".join(str(x) for x in line) + " |")
        lines.append("")

    # Errors
    errors = [r for r in results if r.get("error")]
    if errors:
        lines += ["## 失败条目（errors）", ""]
        for r in errors:
            lines.append(f"- `{r['sample_id']}` / `{r['compressor']}`: {r['error']}")

    return "\n".join(lines)


def run_all(with_judge: bool, limit_compressors: list[str] | None, limit_samples: list[str] | None):
    bench_bin = find_pisci_bin()
    log.info("pisci bin: %s", bench_bin)

    # Resolve samples
    sample_paths: list[Path] = []
    for name in SAMPLES_CLAW:
        sample_paths.append(CLAW_BENCH_DIR / "data" / name)
    for name in SAMPLES_TOOL:
        sample_paths.append(BENCH_DIR / "samples" / name)
    for name in SAMPLES_HARD:
        p = BENCH_DIR / "samples" / name
        if p.exists():
            sample_paths.append(p)
    if limit_samples:
        sample_paths = [p for p in sample_paths if p.stem in limit_samples]

    # Compressor list
    def all_compressors():
        return [
            ("NoCompression", claw_adp.compress_no),
            ("RuleCompressor", claw_adp.compress_rule),
            ("RandomDrop", claw_adp.compress_random_drop),
            ("Pisci-L1", pisci_adp.compress_pisci_l1),
            ("Pisci-L1+", pisci_adp.compress_pisci_l1_plus),
            ("Pisci-L2", pisci_adp.compress_pisci_l2),
            ("Pisci-Harness", pisci_adp.compress_pisci_harness),
            ("Hermes", hermes_adp.compress_hermes),
            ("Engram", claw_adp.compress_engram),
        ]

    compressors = all_compressors()
    if limit_compressors:
        compressors = [(n, f) for n, f in compressors if n in limit_compressors]

    all_results: list[dict] = []

    for sp in sample_paths:
        sample = load_sample(sp)
        sid = sample.get("session_id", sp.stem)
        messages = sample.get("messages", [])
        original_text = flatten_full(messages)

        # Text-projection variant for claw compressors (they only read
        # `content` and would otherwise silently drop tool_use/tool_result
        # blocks, producing misleadingly high ROUGE-L).
        messages_text = _messages_for_text_compressors(messages)

        log.info("")
        log.info("=" * 60)
        log.info("Sample: %s  (%d msgs, %d chars)", sid, len(messages), len(original_text))
        log.info("  Description: %s", sample.get("description", "-"))

        for name, fn in compressors:
            log.info("  → %s", name)
            t0 = time.perf_counter()
            if name.startswith("Pisci-"):
                # Pisci adapters understand blocks natively.
                result = fn(sid, messages)
            else:
                # All other adapters only read `content` — give them the
                # text-projected messages so they see the tool payload.
                result = fn(sid, messages_text)
            dt = (time.perf_counter() - t0) * 1000
            augmented = collect_compressed_result(
                result, original_text, with_judge, messages, sample
            )
            status = (
                "ERR"
                if result.error
                else f"ratio={augmented['compression_ratio']:.3f} "
                f"IR-F1={augmented['info_retention_f1']:.3f} "
                f"LLM={augmented['llm_calls']}"
            )
            log.info("    done in %.0fms (%s)", dt, status)
            if result.error:
                log.warning("    error: %s", result.error)
            all_results.append(augmented)
            # Flush per-sample cache incrementally
            out_dir = BENCH_DIR / "results"
            out_dir.mkdir(parents=True, exist_ok=True)
            (out_dir / "benchmark_results.partial.json").write_text(
                json.dumps({"results": all_results}, ensure_ascii=False, indent=2),
                encoding="utf-8",
            )

    return all_results


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--with-judge",
        action="store_true",
        help="Enable LLM-as-judge downstream fidelity scoring (adds ~4 Qwen calls per pair)",
    )
    parser.add_argument(
        "--only",
        nargs="*",
        help="Limit to specific compressor names (e.g. Pisci-L2 Hermes)",
    )
    parser.add_argument(
        "--samples",
        nargs="*",
        help="Limit to specific sample stems (without .json)",
    )
    args = parser.parse_args()

    results = run_all(args.with_judge, args.only, args.samples)

    out_dir = BENCH_DIR / "results"
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "benchmark_results.json").write_text(
        json.dumps(
            {
                "run_timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "with_judge": args.with_judge,
                "results": results,
            },
            ensure_ascii=False,
            indent=2,
        ),
        encoding="utf-8",
    )
    (out_dir / "RESULTS.md").write_text(render_summary(results, args.with_judge), encoding="utf-8")
    log.info("")
    log.info("✓ %d results collected.", len(results))
    log.info("  %s", out_dir / "benchmark_results.json")
    log.info("  %s", out_dir / "RESULTS.md")


if __name__ == "__main__":
    main()
