from __future__ import annotations

import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[2]

SCHEMA_ERROR_RE = re.compile(
    r"(unknown argument|did you mean|required keys?|full schema|invalid argument|missing required)",
    re.IGNORECASE,
)


def _exe_name(stem: str) -> str:
    return f"{stem}.exe" if os.name == "nt" else stem


def find_binary(stem: str, explicit: str | None = None) -> Path:
    if explicit:
        p = Path(explicit).expanduser().resolve()
        if p.exists():
            return p
        raise FileNotFoundError(f"Binary override not found: {p}")

    # Kernel refactor note:
    # The Rust workspace root is `src-tauri/` so cargo writes binaries
    # under `src-tauri/target/{debug,release}/`. `openpisci` still lives
    # in the `pisci-desktop` crate (full Tauri-linked CLI runner). The new
    # `openpisci-headless` bin belongs to `pisci-cli` and produces the same
    # output directory. Both are discovered by this helper.
    candidates = [
        REPO_ROOT / "src-tauri" / "target" / "release" / _exe_name(stem),
        REPO_ROOT / "src-tauri" / "target" / "debug" / _exe_name(stem),
        REPO_ROOT / "target" / "release" / _exe_name(stem),
        REPO_ROOT / "target" / "debug" / _exe_name(stem),
    ]
    existing = [p for p in candidates if p.exists()]
    if not existing:
        raise FileNotFoundError(
            f"{_exe_name(stem)} not found; build it under src-tauri/ first."
        )
    existing.sort(key=lambda p: p.stat().st_mtime, reverse=True)
    return existing[0]


def ensure_clean_dir(path: Path) -> None:
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True, exist_ok=True)


def default_user_config_path() -> Path | None:
    candidates: list[Path] = []
    if os.name == "nt":
        local = os.environ.get("LOCALAPPDATA")
        roaming = os.environ.get("APPDATA")
        if local:
            candidates.append(Path(local) / "com.pisci.desktop" / "config.json")
        if roaming:
            candidates.append(Path(roaming) / "com.pisci.desktop" / "config.json")
    else:
        xdg = os.environ.get("XDG_DATA_HOME")
        home = Path.home()
        if xdg:
            candidates.append(Path(xdg) / "com.pisci.desktop" / "config.json")
        candidates.append(home / ".local" / "share" / "com.pisci.desktop" / "config.json")
        candidates.append(home / ".config" / "com.pisci.desktop" / "config.json")
    for candidate in candidates:
        if candidate.exists():
            return candidate
    return None


def prepare_config_dir(config_dir: Path, config_template: str | None = None) -> Path:
    config_dir.mkdir(parents=True, exist_ok=True)
    template = (
        Path(config_template).expanduser().resolve()
        if config_template
        else default_user_config_path()
    )
    if template is None or not template.exists():
        raise FileNotFoundError(
            "No config.json template found. Pass --config-template or ensure the user config exists."
        )
    target = config_dir / "config.json"
    shutil.copy2(template, target)
    return target


def write_text(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def materialize_repo(task: dict[str, Any], workspace: Path) -> None:
    ensure_clean_dir(workspace)
    repo_source = task.get("repo_source")
    if repo_source:
        src = Path(str(repo_source)).expanduser().resolve()
        if not src.exists():
            raise FileNotFoundError(f"repo_source does not exist: {src}")
        shutil.copytree(src, workspace, dirs_exist_ok=True)
    else:
        seed_files = task.get("seed_files") or []
        if not seed_files:
            raise ValueError(f"Task {task.get('id')} has neither repo_source nor seed_files")
        for item in seed_files:
            write_text(workspace / item["path"], item["content"])
    init_git_repo(workspace)


def init_git_repo(workspace: Path) -> None:
    def _run(args: list[str]) -> None:
        subprocess.run(
            args,
            cwd=workspace,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

    git_dir = workspace / ".git"
    if git_dir.exists():
        shutil.rmtree(git_dir)
    _run(["git", "init"])
    _run(["git", "config", "user.email", "bench@openpisci.local"])
    _run(["git", "config", "user.name", "OpenPisci Bench"])
    _run(["git", "add", "."])
    _run(["git", "commit", "-m", "seed"])


def quote_python() -> str:
    return subprocess.list2cmdline([sys.executable])


def substitute_placeholders(text: str, workspace: Path) -> str:
    return (
        text.replace("{python}", quote_python())
        .replace("{workspace}", str(workspace))
        .replace("{repo_root}", str(REPO_ROOT))
    )


def run_shell(
    command: str,
    cwd: Path,
    timeout: int | None = None,
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env.setdefault("PYTHONDONTWRITEBYTECODE", "1")
    return subprocess.run(
        command,
        cwd=cwd,
        shell=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        env=env,
    )


def safe_json_loads(raw: str | None) -> list[dict[str, Any]]:
    if not raw:
        return []
    try:
        data = json.loads(raw)
    except json.JSONDecodeError:
        return []
    return data if isinstance(data, list) else []


def export_transcript(db_path: Path, session_id: str) -> list[dict[str, Any]]:
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    try:
        rows = conn.execute(
            """
            SELECT role, content, tool_calls_json, tool_results_json
            FROM messages
            WHERE session_id = ?
            ORDER BY created_at ASC, rowid ASC
            """,
            (session_id,),
        ).fetchall()
    finally:
        conn.close()

    messages: list[dict[str, Any]] = []
    for row in rows:
        role = row["role"]
        content = row["content"] or ""
        blocks: list[dict[str, Any]] = []
        if content.strip():
            blocks.append({"type": "text", "text": content})
        blocks.extend(safe_json_loads(row["tool_calls_json"]))
        blocks.extend(safe_json_loads(row["tool_results_json"]))
        if blocks:
            messages.append({"role": role, "content": content, "blocks": blocks})
        else:
            messages.append({"role": role, "content": content})
    return messages


def _tool_name_by_use_id(messages: list[sqlite3.Row]) -> dict[str, str]:
    mapping: dict[str, str] = {}
    for row in messages:
        for call in safe_json_loads(row["tool_calls_json"]):
            tool_use_id = call.get("id")
            name = call.get("name")
            if tool_use_id and name:
                mapping[str(tool_use_id)] = str(name)
    return mapping


def collect_session_telemetry(
    config_dir: Path,
    session_id: str,
    compact_bin: Path | None = None,
) -> dict[str, Any]:
    db_path = config_dir / "pisci.db"
    if not db_path.exists():
        return {"db_path": str(db_path), "error": "pisci.db not found"}

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    try:
        session = conn.execute(
            """
            SELECT id, message_count, rolling_summary, rolling_summary_version,
                   total_input_tokens, total_output_tokens, status
            FROM sessions
            WHERE id = ?
            """,
            (session_id,),
        ).fetchone()
        rows = conn.execute(
            """
            SELECT role, content, tool_calls_json, tool_results_json, turn_index
            FROM messages
            WHERE session_id = ?
            ORDER BY created_at ASC, rowid ASC
            """,
            (session_id,),
        ).fetchall()
    finally:
        conn.close()

    if session is None:
        return {"db_path": str(db_path), "error": "session not found", "session_id": session_id}

    tool_name_by_id = _tool_name_by_use_id(rows)
    tool_call_count = 0
    tool_result_count = 0
    tool_error_count = 0
    schema_error_count = 0
    recovered_schema_error_count = 0
    unmatched_schema_failures: list[str] = []

    real_user_messages = 0
    assistant_messages = 0

    for row in rows:
        role = row["role"]
        if role == "assistant":
            assistant_messages += 1
        if role == "user" and not row["tool_results_json"]:
            real_user_messages += 1

        for call in safe_json_loads(row["tool_calls_json"]):
            if call.get("type") == "tool_use":
                tool_call_count += 1

        for result in safe_json_loads(row["tool_results_json"]):
            if result.get("type") != "tool_result":
                continue
            tool_result_count += 1
            is_error = bool(result.get("is_error"))
            content = str(result.get("content", ""))
            tool_name = tool_name_by_id.get(str(result.get("tool_use_id", "")), "")
            if is_error:
                tool_error_count += 1
                if SCHEMA_ERROR_RE.search(content):
                    schema_error_count += 1
                    if tool_name:
                        unmatched_schema_failures.append(tool_name)
            elif tool_name and tool_name in unmatched_schema_failures:
                recovered_schema_error_count += 1
                unmatched_schema_failures.remove(tool_name)

    transcript = export_transcript(db_path, session_id)
    harness: dict[str, Any] | None = None
    if compact_bin and transcript:
        request = {
            "mode": "HARNESS",
            "keep_tokens": 2000,
            "messages": transcript,
        }
        proc = subprocess.run(
            [str(compact_bin)],
            input=json.dumps(request, ensure_ascii=False).encode("utf-8"),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=REPO_ROOT,
            timeout=90,
        )
        if proc.returncode == 0:
            try:
                payload = json.loads(proc.stdout.decode("utf-8"))
                harness = {
                    "layered": payload.get("layered"),
                    "compressed_tokens": payload.get("compressed_tokens"),
                    "original_tokens": payload.get("original_tokens"),
                }
            except json.JSONDecodeError:
                harness = {"error": "invalid HARNESS JSON"}
        else:
            harness = {
                "error": proc.stderr.decode("utf-8", errors="replace")[:400],
            }

    return {
        "db_path": str(db_path),
        "session_id": session_id,
        "status": session["status"],
        "message_count": int(session["message_count"] or 0),
        "real_user_message_count": real_user_messages,
        "assistant_message_count": assistant_messages,
        "turn_count": real_user_messages,
        "rolling_summary_version": int(session["rolling_summary_version"] or 0),
        "rolling_summary_chars": len(session["rolling_summary"] or ""),
        "total_input_tokens": int(session["total_input_tokens"] or 0),
        "total_output_tokens": int(session["total_output_tokens"] or 0),
        "tool_call_count": tool_call_count,
        "tool_result_count": tool_result_count,
        "tool_error_count": tool_error_count,
        "schema_error_count": schema_error_count,
        "recovered_schema_error_count": recovered_schema_error_count,
        "harness": harness,
        "transcript": transcript,
    }


def git_artifacts(workspace: Path) -> dict[str, Any]:
    diff = run_shell("git diff --binary", workspace)
    diff_stat = run_shell("git diff --stat", workspace)
    status = run_shell("git status --short", workspace)
    return {
        "patch": diff.stdout,
        "patch_present": bool(diff.stdout.strip()),
        "diff_stat": diff_stat.stdout.strip(),
        "status_short": status.stdout.strip(),
    }
