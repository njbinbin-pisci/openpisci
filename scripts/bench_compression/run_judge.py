"""run_judge.py — Add judge_score to already-collected benchmark results.

Reads results/benchmark_results.json, for each (sample, compressor) pair that
has a non-empty compressed_text, runs the LLM-as-judge loop and writes the
augmented scores back out. Does NOT re-run compression.

Usage:
  py -3 run_judge.py                       # score everything, overwrite RESULTS.md
  py -3 run_judge.py --skip-compressors NoCompression RandomDrop
"""

from __future__ import annotations

import argparse
import json
import logging
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from adapters.common import BENCH_DIR, CLAW_BENCH_DIR
from judge import score_compression

# Reuse run_bench renderer + sample loading
if str(CLAW_BENCH_DIR.parent) not in sys.path:
    sys.path.insert(0, str(CLAW_BENCH_DIR.parent))

from run_bench import render_summary, load_sample, SAMPLES_CLAW, SAMPLES_TOOL  # type: ignore

logging.basicConfig(level=logging.INFO, format="%(asctime)s [%(levelname)s] %(message)s")
log = logging.getLogger("judge")


def _load_all_samples() -> dict[str, list[dict]]:
    """Return a map of sample_id → messages[]."""
    out = {}
    for name in SAMPLES_CLAW:
        sp = CLAW_BENCH_DIR / "data" / name
        s = load_sample(sp)
        out[s.get("session_id", sp.stem)] = s.get("messages", [])
    for name in SAMPLES_TOOL:
        sp = BENCH_DIR / "samples" / name
        s = load_sample(sp)
        out[s.get("session_id", sp.stem)] = s.get("messages", [])
    return out


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--skip-compressors", nargs="*", default=["NoCompression"])
    parser.add_argument("--in-path", default=str(BENCH_DIR / "results" / "benchmark_results.json"))
    parser.add_argument("--out-path", default=str(BENCH_DIR / "results" / "benchmark_results.json"))
    parser.add_argument("--md-path", default=str(BENCH_DIR / "results" / "RESULTS.md"))
    args = parser.parse_args()

    src = Path(args.in_path)
    data = json.loads(src.read_text(encoding="utf-8"))
    results = data["results"]

    samples = _load_all_samples()
    skip = set(args.skip_compressors)

    total_pairs = sum(1 for r in results if not r.get("error") and r["compressor"] not in skip)
    log.info("scoring %d / %d pairs (skipping: %s)", total_pairs, len(results), list(skip))

    scored = 0
    for r in results:
        if r.get("error"):
            continue
        if r["compressor"] in skip:
            # Give NoCompression a ceiling score of 5.0 directly from the
            # original (it IS the ground truth). RandomDrop we skip similarly
            # only when the user asks us to.
            if r["compressor"] == "NoCompression":
                r["judge_score"] = 5.0
            continue
        compressed_text = r.get("compressed_preview")
        # We don't actually have compressed_text in the JSON (we only kept
        # preview). Need to recompute from the original compressor run. But
        # since compressed_text can be huge, we could re-invoke the adapter
        # — too slow. Instead, for the judge we use the preview+reference
        # answer; if preview is sufficient, use it, else re-run.
        # Quick workaround: store full compressed text in the results JSON.
        compressed_text = r.get("compressed_text")
        if not compressed_text:
            log.warning(
                "skipping %s/%s: no compressed_text in results (re-run run_bench.py to populate)",
                r["sample_id"], r["compressor"],
            )
            continue
        msgs = samples.get(r["sample_id"])
        if not msgs:
            log.warning("no messages for sample %s", r["sample_id"])
            continue
        scored += 1
        log.info(
            "[%d/%d] %s / %s",
            scored, total_pairs, r["sample_id"], r["compressor"],
        )
        t0 = time.perf_counter()
        try:
            j = score_compression(r["sample_id"], msgs, compressed_text)
            r["judge_score"] = round(j["avg_score"], 2)
            r["judge_per_question"] = j["per_question"]
        except Exception as e:
            log.exception("judge failed")
            r["judge_score"] = 0.0
            r["judge_error"] = f"{type(e).__name__}: {e}"
        log.info("  → judge=%.2f (%.1fs)", r.get("judge_score", 0.0), time.perf_counter() - t0)

        # Flush incrementally so a crash mid-way doesn't lose progress.
        Path(args.out_path).write_text(
            json.dumps(
                {
                    "run_timestamp": data.get("run_timestamp"),
                    "judge_timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                    "with_judge": True,
                    "results": results,
                },
                ensure_ascii=False,
                indent=2,
            ),
            encoding="utf-8",
        )

    Path(args.md_path).write_text(render_summary(results, with_judge=True), encoding="utf-8")
    log.info("wrote %s and %s", args.out_path, args.md_path)


if __name__ == "__main__":
    main()
