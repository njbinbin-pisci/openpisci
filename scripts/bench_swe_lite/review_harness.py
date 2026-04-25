from __future__ import annotations

import argparse
import json
from collections import defaultdict
from pathlib import Path
from typing import Any


def load_run(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _profile_rows(run: dict[str, Any]) -> dict[str, list[dict[str, Any]]]:
    grouped: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for row in run.get("results") or []:
        grouped[row["profile_name"]].append(row)
    return grouped


def render_review(run: dict[str, Any]) -> str:
    grouped = _profile_rows(run)
    baseline = grouped.get("baseline_pisci", [])
    context_lite = grouped.get("context_lite", [])
    pool = grouped.get("experimental_pool", [])

    def resolved_rate(rows: list[dict[str, Any]]) -> float:
        return (
            len([r for r in rows if r.get("resolved")]) / len(rows) if rows else 0.0
        )

    def avg_tool_errors(rows: list[dict[str, Any]]) -> float:
        if not rows:
            return 0.0
        return sum((r.get("telemetry") or {}).get("tool_error_count", 0) for r in rows) / len(rows)

    def avg_chutil(rows: list[dict[str, Any]]) -> float | None:
        values = []
        for row in rows:
            layered = (((row.get("telemetry") or {}).get("harness") or {}).get("layered") or {})
            if isinstance(layered.get("channel_utilization"), (int, float)):
                values.append(float(layered["channel_utilization"]))
        if not values:
            return None
        return sum(values) / len(values)

    lines = [
        "# OpenPisci Harness Review",
        "",
        "## Summary",
        "",
        f"- `baseline_pisci` resolved rate: `{resolved_rate(baseline):.2%}`",
        f"- `context_lite` resolved rate: `{resolved_rate(context_lite):.2%}`",
        f"- `experimental_pool` resolved rate: `{resolved_rate(pool):.2%}`",
        f"- baseline average tool errors: `{avg_tool_errors(baseline):.2f}`",
        f"- context-lite average tool errors: `{avg_tool_errors(context_lite):.2f}`",
        f"- pool average tool errors: `{avg_tool_errors(pool):.2f}`",
        f"- baseline average channel utilization: `{avg_chutil(baseline):.2f}`"
        if avg_chutil(baseline) is not None
        else "- baseline average channel utilization: `-`",
        f"- context-lite average channel utilization: `{avg_chutil(context_lite):.2f}`"
        if avg_chutil(context_lite) is not None
        else "- context-lite average channel utilization: `-`",
        "",
        "## Interpretation Rules",
        "",
        "- If `context_lite` beats `baseline_pisci`, the runtime context path is likely over-injecting or preserving low-value context.",
        "- If `baseline_pisci` beats `context_lite` while rolling-summary versions are non-zero, structured summary / memory / project hints are helping complex tasks.",
        "- If `experimental_pool` is slower and resolves fewer tasks, pool coordination overhead is dominating task execution.",
        "- If tool errors stay high but recovered-schema errors stay low, schema correction is not closing the loop well enough in end-to-end tasks.",
        "- If channel utilization is already high on failed tasks, failures are more likely due to crowding / wrong prioritization than to missing context.",
        "- If channel utilization is low on failed tasks, the harness may be under-injecting task-critical state.",
        "",
        "## Runtime Caveat",
        "",
        "This review uses telemetry from the real `openpisci-headless run` path plus optional post-hoc `HARNESS` breakdown on persisted session transcripts. That means the layered token numbers are diagnostic, not proof that the runtime request path and `ContextBuilder::finalize` are perfectly identical.",
        "",
        "## Next Checks",
        "",
        "1. Compare failing tasks where `context_lite` outperforms `baseline_pisci`; inspect whether memory / project instructions / old history were noisy.",
        "2. Compare failing tasks with high tool error counts; inspect tool schema retries and whether errors were recovered later in the same session.",
        "3. Compare baseline vs pool on multi-phase tasks; verify whether pool adds resolution or only extra latency.",
        "4. For tasks with low resolved rate and low channel utilization, inspect whether important context never entered the prompt path at all.",
        "5. For tasks with high rolling-summary version but poor outcomes, inspect whether the summary schema preserved the wrong facts / open items.",
        "",
    ]
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("run_json", help="Path to run_results.json")
    parser.add_argument(
        "--output",
        help="Optional markdown output path. Defaults to HARNESS_REVIEW.md next to the input.",
    )
    args = parser.parse_args()

    run_json = Path(args.run_json).resolve()
    review = render_review(load_run(run_json))
    output = (
        Path(args.output).resolve()
        if args.output
        else run_json.with_name("HARNESS_REVIEW.md")
    )
    output.write_text(review, encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
