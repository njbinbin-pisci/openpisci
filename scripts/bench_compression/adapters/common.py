"""Shared utilities and the unified CompressorResult dataclass."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

# Paths
REPO_ROOT = Path(__file__).resolve().parents[3]
BENCH_DIR = Path(__file__).resolve().parents[1]
CLAW_BENCH_DIR = REPO_ROOT / "references" / "claw-compactor" / "benchmark"
HERMES_DIR = REPO_ROOT / "references" / "hermes-agent"
PISCI_BIN_CANDIDATES = [
    REPO_ROOT / "src-tauri" / "target" / "release" / "pisci_compact_one.exe",
    REPO_ROOT / "src-tauri" / "target" / "debug" / "pisci_compact_one.exe",
    REPO_ROOT / "target" / "release" / "pisci_compact_one.exe",
    REPO_ROOT / "target" / "debug" / "pisci_compact_one.exe",
]


def find_pisci_bin() -> Path:
    """Return the freshest pisci_compact_one.exe available.

    Cargo can place the binary under either `./target/` or `./src-tauri/target/`
    depending on whether the invocation used `--manifest-path`. Prefer the
    most recently modified one so stale outputs never mask fresh rebuilds.
    """
    existing = [p for p in PISCI_BIN_CANDIDATES if p.exists()]
    if not existing:
        raise FileNotFoundError(
            "pisci_compact_one.exe not found; build with "
            "`cargo build --bin pisci_compact_one --manifest-path src-tauri/Cargo.toml`"
        )
    existing.sort(key=lambda p: p.stat().st_mtime, reverse=True)
    return existing[0]


@dataclass
class CompressorResult:
    """Unified output from every compressor adapter."""

    sample_id: str
    compressor: str
    compressed_text: str
    compressed_tokens: int = 0
    original_tokens: int = 0
    latency_ms: float = 0.0
    llm_calls: int = 0
    llm_input_tokens: int = 0
    llm_output_tokens: int = 0
    error: str | None = None
    notes: dict[str, Any] = field(default_factory=dict)


# ---------------------------------------------------------------------------
# Runtime discovery: read the LLM runtime from Pisci's decrypted settings by
# calling `pisci_compact_one.exe --print-runtime`. This ensures every LLM-
# using compressor (Pisci-L2, Hermes, Engram, judge) hits the SAME backend.
# ---------------------------------------------------------------------------

_RUNTIME_CACHE: dict | None = None


def get_qwen_runtime() -> dict:
    global _RUNTIME_CACHE
    if _RUNTIME_CACHE is not None:
        return _RUNTIME_CACHE
    bin_path = find_pisci_bin()
    try:
        out = subprocess.check_output(
            [str(bin_path), "--print-runtime"],
            stderr=subprocess.DEVNULL,
            timeout=30,
        )
    except subprocess.CalledProcessError as e:
        raise RuntimeError(f"pisci_compact_one --print-runtime failed: {e}") from e
    rt = json.loads(out.decode("utf-8").strip())
    # Hermes expects provider="custom" to force the custom-endpoint path.
    if rt["provider"] != "custom":
        rt["hermes_provider"] = "custom"
    else:
        rt["hermes_provider"] = "custom"
    _RUNTIME_CACHE = rt
    return rt


# ---------------------------------------------------------------------------
# OpenAI-compatible chat completion helper (used by the judge, Engram reroute)
# ---------------------------------------------------------------------------


def qwen_chat(
    messages: list[dict],
    max_tokens: int = 512,
    temperature: float = 0.2,
    timeout: int = 180,
) -> tuple[str, int, int]:
    """Call Qwen via the OpenAI-compatible endpoint. Returns (content, in_toks, out_toks)."""
    rt = get_qwen_runtime()
    payload = json.dumps(
        {
            "model": rt["model"],
            "max_tokens": max_tokens,
            "temperature": temperature,
            "messages": messages,
        }
    ).encode("utf-8")
    req = urllib.request.Request(
        rt["base_url"].rstrip("/") + "/chat/completions",
        data=payload,
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {rt['api_key']}",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            data = json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace")[:400]
        raise RuntimeError(f"Qwen HTTP {e.code}: {body}") from e
    content = data["choices"][0]["message"]["content"].strip()
    usage = data.get("usage", {}) or {}
    return (
        content,
        int(usage.get("prompt_tokens") or 0),
        int(usage.get("completion_tokens") or 0),
    )
