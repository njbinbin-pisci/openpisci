"""Pisci adapter: drives pisci_compact_one.exe via stdin/stdout JSON."""

from __future__ import annotations

import json
import subprocess
import time
from typing import Any

from .common import CompressorResult, find_pisci_bin


def _invoke(mode: str, messages: list[dict], keep_tokens: int = 2000, timeout: int = 240) -> dict:
    bin_path = find_pisci_bin()
    req = {
        "mode": mode,
        "keep_tokens": keep_tokens,
        "messages": messages,
    }
    proc = subprocess.run(
        [str(bin_path)],
        input=json.dumps(req, ensure_ascii=False).encode("utf-8"),
        capture_output=True,
        timeout=timeout,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"pisci bin exit {proc.returncode}: "
            f"{proc.stderr.decode('utf-8', errors='replace')[:400]}"
        )
    out = proc.stdout.decode("utf-8").strip()
    return json.loads(out)


def compress_pisci_l1(sample_id: str, messages: list[dict]) -> CompressorResult:
    """Pisci Level-1: receipt demotion for old tool results. No LLM call.

    For pure-text samples this is essentially an identity pass. For tool-heavy
    samples the ToolResult blocks older than the preserved window are replaced
    with their minimal receipts.
    """
    start = time.perf_counter()
    try:
        r = _invoke("L1", messages)
    except Exception as e:
        return CompressorResult(
            sample_id=sample_id,
            compressor="Pisci-L1",
            compressed_text="",
            error=str(e),
            latency_ms=(time.perf_counter() - start) * 1000.0,
        )
    return CompressorResult(
        sample_id=sample_id,
        compressor="Pisci-L1",
        compressed_text=r["compressed_text"],
        compressed_tokens=r["compressed_tokens"],
        original_tokens=r["original_tokens"],
        latency_ms=r["latency_ms"],
        llm_calls=r["llm_calls"],
        llm_input_tokens=r["llm_input_tokens"],
        llm_output_tokens=r["llm_output_tokens"],
        notes={"provider": r["provider"], "model": r["model"]},
    )


def compress_pisci_l1_plus(sample_id: str, messages: list[dict]) -> CompressorResult:
    """Pisci L1+: rule_preprocess (L1 level) + receipt demotion. No LLM call.

    Adds the deterministic, zero-LLM preprocessing layer introduced in
    Pisci 压缩内核 v2 Phase 1 (RLE, stack folding, ANSI stripping, base64
    placeholders, table compression, path normalization). Only in-message
    rules — no cross-message dedup at this level.
    """
    start = time.perf_counter()
    try:
        r = _invoke("L1+", messages)
    except Exception as e:
        return CompressorResult(
            sample_id=sample_id,
            compressor="Pisci-L1+",
            compressed_text="",
            error=str(e),
            latency_ms=(time.perf_counter() - start) * 1000.0,
        )
    return CompressorResult(
        sample_id=sample_id,
        compressor="Pisci-L1+",
        compressed_text=r["compressed_text"],
        compressed_tokens=r["compressed_tokens"],
        original_tokens=r["original_tokens"],
        latency_ms=r["latency_ms"],
        llm_calls=r["llm_calls"],
        llm_input_tokens=r["llm_input_tokens"],
        llm_output_tokens=r["llm_output_tokens"],
        notes={"provider": r["provider"], "model": r["model"]},
    )


def compress_pisci_harness(sample_id: str, messages: list[dict]) -> CompressorResult:
    """Pisci HARNESS: full ContextBuilder::finalize pipeline, no LLM call.

    Exercises the production context-assembly path (demotion + supersede
    cleanup + pair-integrity sanitization + layered token accounting) and
    reports per-layer attribution via `notes["layered"]`.
    """
    start = time.perf_counter()
    try:
        r = _invoke("HARNESS", messages)
    except Exception as e:
        return CompressorResult(
            sample_id=sample_id,
            compressor="Pisci-Harness",
            compressed_text="",
            error=str(e),
            latency_ms=(time.perf_counter() - start) * 1000.0,
        )
    notes = {"provider": r.get("provider"), "model": r.get("model")}
    if "layered" in r and r["layered"] is not None:
        notes["layered"] = r["layered"]
    return CompressorResult(
        sample_id=sample_id,
        compressor="Pisci-Harness",
        compressed_text=r["compressed_text"],
        compressed_tokens=r["compressed_tokens"],
        original_tokens=r["original_tokens"],
        latency_ms=r["latency_ms"],
        llm_calls=r["llm_calls"],
        llm_input_tokens=r["llm_input_tokens"],
        llm_output_tokens=r["llm_output_tokens"],
        notes=notes,
    )


def compress_pisci_l2(sample_id: str, messages: list[dict], keep_tokens: int = 2000) -> CompressorResult:
    """Pisci Level-2: rolling summary via compact_summarise. One LLM call."""
    start = time.perf_counter()
    try:
        r = _invoke("L2", messages, keep_tokens=keep_tokens, timeout=300)
    except Exception as e:
        return CompressorResult(
            sample_id=sample_id,
            compressor="Pisci-L2",
            compressed_text="",
            error=str(e),
            latency_ms=(time.perf_counter() - start) * 1000.0,
        )
    return CompressorResult(
        sample_id=sample_id,
        compressor="Pisci-L2",
        compressed_text=r["compressed_text"],
        compressed_tokens=r["compressed_tokens"],
        original_tokens=r["original_tokens"],
        latency_ms=r["latency_ms"],
        llm_calls=r["llm_calls"],
        llm_input_tokens=r["llm_input_tokens"],
        llm_output_tokens=r["llm_output_tokens"],
        notes={"provider": r["provider"], "model": r["model"], "keep_tokens": keep_tokens},
    )
