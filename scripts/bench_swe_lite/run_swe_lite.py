from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import tomllib
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from common import (
    REPO_ROOT,
    collect_session_telemetry,
    find_binary,
    git_artifacts,
    materialize_repo,
    prepare_config_dir,
    run_shell,
    substitute_placeholders,
    write_text,
)
from review_harness import render_review
from score_swe_lite import render_summary


def load_manifest(path: Path) -> dict[str, Any]:
    with path.open("rb") as fh:
        return tomllib.load(fh)


def resolve_tasks(
    manifest: dict[str, Any],
    only_tasks: set[str] | None,
) -> list[dict[str, Any]]:
    tasks = manifest.get("tasks") or []
    if only_tasks:
        tasks = [task for task in tasks if task["id"] in only_tasks]
    return tasks


def resolve_profiles(
    manifest: dict[str, Any],
    only_profiles: set[str] | None,
) -> list[dict[str, Any]]:
    profiles = manifest.get("profiles") or []
    if only_profiles:
        profiles = [profile for profile in profiles if profile["name"] in only_profiles]
    return profiles


def build_request(
    task: dict[str, Any],
    profile: dict[str, Any],
    workspace: Path,
    config_dir: Path,
    session_id: str,
    phase_prompt: str,
    phase_index: int,
    response_path: Path,
) -> dict[str, Any]:
    extra_context_parts = []
    if task.get("extra_system_context"):
        extra_context_parts.append(str(task["extra_system_context"]).strip())
    if profile.get("extra_system_context"):
        extra_context_parts.append(str(profile["extra_system_context"]).strip())

    request: dict[str, Any] = {
        "prompt": phase_prompt,
        "workspace": str(workspace),
        "mode": profile.get("mode", "pisci"),
        "session_id": session_id,
        "session_title": f"{task['title']} [{profile['name']}]",
        "channel": task.get("channel", "benchmark"),
        "config_dir": str(config_dir),
        "task_timeout_secs": int(task.get("task_timeout_secs", 900)),
        "output": str(response_path),
    }
    if extra_context_parts:
        request["extra_system_context"] = "\n\n".join(extra_context_parts)
    if request["mode"] == "pool" or profile.get("wait_for_completion"):
        request["wait_for_completion"] = bool(profile.get("wait_for_completion", True))
        request["wait_timeout_secs"] = int(profile.get("wait_timeout_secs", 1800))
        request["pool_name"] = f"{task['id']}-{profile['name']}"
        request["pool_size"] = int(profile.get("pool_size", 3))
    if profile.get("context_toggles"):
        request["context_toggles"] = profile["context_toggles"]
    if phase_index > 0 and "phase_followup_context" in task:
        request["extra_system_context"] = (
            (request.get("extra_system_context", "") + "\n\n" if request.get("extra_system_context") else "")
            + str(task["phase_followup_context"]).strip()
        )
    return request


def run_case_profile(
    task: dict[str, Any],
    profile: dict[str, Any],
    task_dir: Path,
    openpisci_bin: Path,
    compact_bin: Path | None,
    config_template: str | None,
) -> dict[str, Any]:
    workspace = task_dir / "workspace"
    config_dir = task_dir / "config"
    materialize_repo(task, workspace)
    config_template_path = prepare_config_dir(config_dir, config_template)

    session_id = f"{task['id']}__{profile['name']}"
    phases = task.get("phases") or [{"prompt": task["prompt"]}]
    phase_records: list[dict[str, Any]] = []
    started = time.perf_counter()
    final_response_text = ""
    agent_exit_code = 0

    for idx, phase in enumerate(phases, start=1):
        phase_dir = task_dir / f"phase_{idx:02d}"
        phase_dir.mkdir(parents=True, exist_ok=True)
        response_path = phase_dir / "response.json"
        request = build_request(
            task=task,
            profile=profile,
            workspace=workspace,
            config_dir=config_dir,
            session_id=session_id,
            phase_prompt=str(phase["prompt"]),
            phase_index=idx - 1,
            response_path=response_path,
        )
        request_path = phase_dir / "request.json"
        write_text(request_path, json.dumps(request, ensure_ascii=False, indent=2))

        timeout = int(task.get("task_timeout_secs", 900)) + 60
        proc = subprocess.run(
            [str(openpisci_bin), "run", "--input", str(request_path)],
            cwd=REPO_ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout,
        )
        if proc.returncode == 0 and not response_path.exists():
            proc = subprocess.run(
                [
                    "cargo",
                    "run",
                    "--bin",
                    "openpisci",
                    "--manifest-path",
                    "src-tauri/Cargo.toml",
                    "--",
                    "run",
                    "--input",
                    str(request_path),
                ],
                cwd=REPO_ROOT,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=timeout,
            )
        write_text(phase_dir / "stdout.log", proc.stdout)
        write_text(phase_dir / "stderr.log", proc.stderr)
        agent_exit_code = proc.returncode

        response_payload: dict[str, Any] = {}
        if response_path.exists():
            try:
                response_payload = json.loads(response_path.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                response_payload = {"ok": False, "error": "invalid response json"}

        final_response_text = response_payload.get("response_text", final_response_text)
        phase_records.append(
            {
                "phase_index": idx,
                "prompt": phase["prompt"],
                "request_path": str(request_path),
                "response_path": str(response_path),
                "stdout_path": str(phase_dir / "stdout.log"),
                "stderr_path": str(phase_dir / "stderr.log"),
                "agent_exit_code": proc.returncode,
                "response": response_payload,
            }
        )
        if proc.returncode != 0:
            break

    wall_clock_secs = time.perf_counter() - started

    test_command = substitute_placeholders(str(task["test_command"]), workspace)
    test_proc = run_shell(
        test_command,
        cwd=workspace,
        timeout=int(task.get("test_timeout_secs", 180)),
    )
    write_text(task_dir / "test.stdout.log", test_proc.stdout)
    write_text(task_dir / "test.stderr.log", test_proc.stderr)

    git_meta = git_artifacts(workspace)
    write_text(task_dir / "changes.patch", git_meta["patch"])
    telemetry = collect_session_telemetry(config_dir, session_id, compact_bin)
    write_text(
        task_dir / "telemetry.json",
        json.dumps(telemetry, ensure_ascii=False, indent=2),
    )

    result = {
        "task_id": task["id"],
        "task_title": task["title"],
        "profile_name": profile["name"],
        "profile_mode": profile.get("mode", "pisci"),
        "phase_count": len(phases),
        "session_id": session_id,
        "wall_clock_secs": round(wall_clock_secs, 3),
        "agent_exit_code": agent_exit_code,
        "response_text": final_response_text,
        "disabled_tools": phase_records[-1]["response"].get("disabled_tools", [])
        if phase_records
        else [],
        "test_command": test_command,
        "test_exit": test_proc.returncode,
        "tests_passed": test_proc.returncode == 0,
        "resolved": test_proc.returncode == 0,
        "patch_present": git_meta["patch_present"],
        "git_diff_stats": git_meta["diff_stat"],
        "git_status_short": git_meta["status_short"],
        "workspace": str(workspace),
        "config_dir": str(config_dir),
        "config_template": str(config_template_path),
        "task_dir": str(task_dir),
        "phase_records": phase_records,
        "telemetry": telemetry,
    }
    return result


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--manifest",
        default="cases/manifest.toml",
        help="Path to the SWE-lite manifest (default: cases/manifest.toml)",
    )
    parser.add_argument("--only-tasks", nargs="*", help="Run only selected task ids")
    parser.add_argument("--profiles", nargs="*", help="Run only selected profile names")
    parser.add_argument("--results-dir", default="results", help="Output directory")
    parser.add_argument("--openpisci-bin", help="Override openpisci binary path")
    parser.add_argument(
        "--pisci-compact-bin",
        help="Override pisci_compact_one binary path used for HARNESS analysis",
    )
    parser.add_argument(
        "--config-template",
        help="Path to a config.json copied into each isolated config_dir before a run",
    )
    args = parser.parse_args()

    base_dir = Path(__file__).resolve().parent
    manifest_path = (base_dir / args.manifest).resolve()
    results_root = (base_dir / args.results_dir).resolve()
    results_root.mkdir(parents=True, exist_ok=True)

    manifest = load_manifest(manifest_path)
    tasks = resolve_tasks(manifest, set(args.only_tasks) if args.only_tasks else None)
    profiles = resolve_profiles(manifest, set(args.profiles) if args.profiles else None)

    openpisci_bin = find_binary("openpisci", args.openpisci_bin)
    compact_bin = find_binary("pisci_compact_one", args.pisci_compact_bin)
    run_id = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    run_dir = results_root / run_id
    run_dir.mkdir(parents=True, exist_ok=True)

    all_results: list[dict[str, Any]] = []
    for task in tasks:
        for profile in profiles:
            task_dir = run_dir / task["id"] / profile["name"]
            task_dir.mkdir(parents=True, exist_ok=True)
            result = run_case_profile(
                task=task,
                profile=profile,
                task_dir=task_dir,
                openpisci_bin=openpisci_bin,
                compact_bin=compact_bin,
                config_template=args.config_template,
            )
            all_results.append(result)
            print(
                f"[{task['id']}/{profile['name']}] resolved={result['resolved']} "
                f"test_exit={result['test_exit']} time={result['wall_clock_secs']:.1f}s"
            )

    payload = {
        "run_id": run_id,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "manifest_path": str(manifest_path),
        "openpisci_bin": str(openpisci_bin),
        "pisci_compact_bin": str(compact_bin),
        "results": all_results,
    }
    run_json = run_dir / "run_results.json"
    run_json.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    (run_dir / "RESULTS.md").write_text(render_summary(payload), encoding="utf-8")
    (run_dir / "HARNESS_REVIEW.md").write_text(render_review(payload), encoding="utf-8")
    print(run_json)


if __name__ == "__main__":
    main()
