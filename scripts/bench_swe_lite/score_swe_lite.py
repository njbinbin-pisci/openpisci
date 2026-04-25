from __future__ import annotations

import argparse
import json
import statistics as stats
from collections import defaultdict
from pathlib import Path
from typing import Any


def load_run(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _median(values: list[float]) -> float | None:
    if not values:
        return None
    return float(stats.median(values))


def aggregate_by_profile(results: list[dict[str, Any]]) -> list[dict[str, Any]]:
    grouped: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for row in results:
        grouped[row["profile_name"]].append(row)

    out: list[dict[str, Any]] = []
    for profile_name, rows in sorted(grouped.items()):
        resolved = [r for r in rows if r.get("resolved")]
        resume_rows = [r for r in rows if r.get("phase_count", 1) > 1]
        tool_error_rates = []
        channel_utils = []
        total_input_tokens = 0
        total_output_tokens = 0
        summary_versions = []
        for row in rows:
            telemetry = row.get("telemetry") or {}
            total_input_tokens += int(telemetry.get("total_input_tokens") or 0)
            total_output_tokens += int(telemetry.get("total_output_tokens") or 0)
            tool_results = max(int(telemetry.get("tool_result_count") or 0), 1)
            tool_error_rates.append((telemetry.get("tool_error_count") or 0) / tool_results)
            summary_versions.append(int(telemetry.get("rolling_summary_version") or 0))
            harness = telemetry.get("harness") or {}
            layered = harness.get("layered") or {}
            if isinstance(layered.get("channel_utilization"), (int, float)):
                channel_utils.append(float(layered["channel_utilization"]))

        out.append(
            {
                "profile_name": profile_name,
                "tasks": len(rows),
                "resolved_rate": len(resolved) / len(rows) if rows else 0.0,
                "test_pass_rate": len([r for r in rows if r.get("test_exit") == 0]) / len(rows)
                if rows
                else 0.0,
                "patch_apply_rate": len([r for r in rows if r.get("patch_present")]) / len(rows)
                if rows
                else 0.0,
                "median_time_to_resolve": _median(
                    [float(r["wall_clock_secs"]) for r in resolved]
                ),
                "median_agent_turns_to_resolve": _median(
                    [
                        float((r.get("telemetry") or {}).get("assistant_message_count") or 0)
                        for r in resolved
                    ]
                ),
                "tool_error_rate": sum(tool_error_rates) / len(tool_error_rates)
                if tool_error_rates
                else 0.0,
                "resume_success_rate": len([r for r in resume_rows if r.get("resolved")])
                / len(resume_rows)
                if resume_rows
                else None,
                "token_cost_per_resolved_task": (
                    (total_input_tokens + total_output_tokens) / len(resolved)
                    if resolved
                    else None
                ),
                "avg_channel_utilization": sum(channel_utils) / len(channel_utils)
                if channel_utils
                else None,
                "avg_rolling_summary_version": sum(summary_versions) / len(summary_versions)
                if summary_versions
                else 0.0,
            }
        )
    return out


def render_summary(run: dict[str, Any]) -> str:
    rows = run.get("results") or []
    by_profile = aggregate_by_profile(rows)

    lines = [
        "# OpenPisci SWE-lite Results",
        "",
        f"- Run ID: `{run.get('run_id', '-')}`",
        f"- Manifest: `{run.get('manifest_path', '-')}`",
        f"- Headless CLI: `{run.get('headless_bin', '-')}`",
        f"- HARNESS Binary: `{run.get('pisci_compact_bin', '-')}`",
        "",
        "## Profile Summary",
        "",
        "| Profile | Tasks | Resolved | Test Pass | Patch Apply | Median Time(s) | Median Agent Turns | Tool Error Rate | Resume Success | Token Cost/Resolved | ChUtil | Summary Ver |",
        "|---|---|---|---|---|---|---|---|---|---|---|---|",
    ]

    for row in by_profile:
        lines.append(
            "| {profile_name} | {tasks} | {resolved_rate:.2%} | {test_pass_rate:.2%} | {patch_apply_rate:.2%} | {median_time} | {median_turns} | {tool_error_rate:.2%} | {resume_rate} | {token_cost} | {chutil} | {summary_ver:.2f} |".format(
                profile_name=row["profile_name"],
                tasks=row["tasks"],
                resolved_rate=row["resolved_rate"],
                test_pass_rate=row["test_pass_rate"],
                patch_apply_rate=row["patch_apply_rate"],
                median_time=f"{row['median_time_to_resolve']:.1f}"
                if row["median_time_to_resolve"] is not None
                else "-",
                median_turns=f"{row['median_agent_turns_to_resolve']:.1f}"
                if row["median_agent_turns_to_resolve"] is not None
                else "-",
                tool_error_rate=row["tool_error_rate"],
                resume_rate=f"{row['resume_success_rate']:.2%}"
                if row["resume_success_rate"] is not None
                else "-",
                token_cost=f"{row['token_cost_per_resolved_task']:.0f}"
                if row["token_cost_per_resolved_task"] is not None
                else "-",
                chutil=f"{row['avg_channel_utilization']:.2f}"
                if row["avg_channel_utilization"] is not None
                else "-",
                summary_ver=row["avg_rolling_summary_version"],
            )
        )

    lines.extend(["", "## Task Detail", ""])
    lines.append(
        "| Task | Profile | Resolved | Test Exit | Patch | Time(s) | Input Tok | Output Tok | Tool Errors | Recovered Schema | Summary Ver | ChUtil |"
    )
    lines.append("|---|---|---|---|---|---|---|---|---|---|---|---|")

    for row in sorted(rows, key=lambda r: (r["task_id"], r["profile_name"])):
        telemetry = row.get("telemetry") or {}
        layered = ((telemetry.get("harness") or {}).get("layered") or {})
        lines.append(
            "| {task} | {profile} | {resolved} | {test_exit} | {patch} | {time:.1f} | {in_tok} | {out_tok} | {tool_err} | {schema} | {summary_ver} | {chutil} |".format(
                task=row["task_id"],
                profile=row["profile_name"],
                resolved="yes" if row.get("resolved") else "no",
                test_exit=row.get("test_exit", "-"),
                patch="yes" if row.get("patch_present") else "no",
                time=float(row.get("wall_clock_secs") or 0),
                in_tok=int(telemetry.get("total_input_tokens") or 0),
                out_tok=int(telemetry.get("total_output_tokens") or 0),
                tool_err=int(telemetry.get("tool_error_count") or 0),
                schema=int(telemetry.get("recovered_schema_error_count") or 0),
                summary_ver=int(telemetry.get("rolling_summary_version") or 0),
                chutil=f"{float(layered.get('channel_utilization')):.2f}"
                if isinstance(layered.get("channel_utilization"), (int, float))
                else "-",
            )
        )

    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("run_json", help="Path to run_results.json")
    parser.add_argument(
        "--output",
        help="Optional markdown output path. Defaults to RESULTS.md next to the input.",
    )
    args = parser.parse_args()

    run_json = Path(args.run_json).resolve()
    run = load_run(run_json)
    markdown = render_summary(run)
    output = (
        Path(args.output).resolve()
        if args.output
        else run_json.with_name("RESULTS.md")
    )
    output.write_text(markdown, encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
