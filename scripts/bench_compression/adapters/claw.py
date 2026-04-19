"""claw-compactor adapters: re-export the 4 reference compressors."""

from __future__ import annotations

import os
import sys
import time

from .common import CLAW_BENCH_DIR, CompressorResult, get_qwen_runtime

_IMPORT_ONCE = {}


def _ensure_claw_imported():
    if "claw" in _IMPORT_ONCE:
        return _IMPORT_ONCE
    claw_root = str(CLAW_BENCH_DIR.parent)
    if claw_root not in sys.path:
        sys.path.insert(0, claw_root)
    # evaluate/estimate_tokens lives alongside
    from benchmark.compressors import (  # type: ignore
        NoCompressor,
        RandomDropCompressor,
        RuleCompressor,
        EngramCompressor,
    )
    from benchmark.evaluate import estimate_tokens, messages_to_text  # type: ignore

    _IMPORT_ONCE.update(
        {
            "claw": True,
            "NoCompressor": NoCompressor,
            "RandomDropCompressor": RandomDropCompressor,
            "RuleCompressor": RuleCompressor,
            "EngramCompressor": EngramCompressor,
            "estimate_tokens": estimate_tokens,
            "messages_to_text": messages_to_text,
        }
    )
    return _IMPORT_ONCE


def _run(compressor, name: str, sample_id: str, messages: list[dict]) -> CompressorResult:
    mods = _ensure_claw_imported()
    estimate_tokens = mods["estimate_tokens"]
    messages_to_text = mods["messages_to_text"]
    start = time.perf_counter()
    try:
        compressed_text, llm_calls = compressor.compress(messages)
    except Exception as e:
        return CompressorResult(
            sample_id=sample_id,
            compressor=name,
            compressed_text="",
            error=f"{type(e).__name__}: {e}",
            latency_ms=(time.perf_counter() - start) * 1000.0,
        )
    latency_ms = (time.perf_counter() - start) * 1000.0
    original_tokens = estimate_tokens(messages_to_text(messages))
    compressed_tokens = estimate_tokens(compressed_text)
    return CompressorResult(
        sample_id=sample_id,
        compressor=name,
        compressed_text=compressed_text,
        compressed_tokens=compressed_tokens,
        original_tokens=original_tokens,
        latency_ms=latency_ms,
        llm_calls=llm_calls,
    )


def compress_no(sample_id: str, messages: list[dict]) -> CompressorResult:
    mods = _ensure_claw_imported()
    return _run(mods["NoCompressor"](), "NoCompression", sample_id, messages)


def compress_random_drop(sample_id: str, messages: list[dict]) -> CompressorResult:
    mods = _ensure_claw_imported()
    return _run(mods["RandomDropCompressor"](target_ratio=0.4, seed=42), "RandomDrop", sample_id, messages)


def compress_rule(sample_id: str, messages: list[dict]) -> CompressorResult:
    mods = _ensure_claw_imported()
    return _run(mods["RuleCompressor"](), "RuleCompressor", sample_id, messages)


def compress_engram(sample_id: str, messages: list[dict]) -> CompressorResult:
    """Engram rerouted to our Qwen endpoint (instead of localhost:8403 proxy).

    claw-compactor's EngramCompressor posts directly to `{base_url}/v1/chat/completions`,
    so we point it at Qwen's OpenAI-compatible endpoint and pass the API key via
    OPENAI_API_KEY env var.
    """
    mods = _ensure_claw_imported()
    rt = get_qwen_runtime()
    # Engram's _call_llm uses urllib.request — no /v1 suffix needed because
    # claw's constructor appends "/v1/chat/completions". Qwen's base already
    # ends with "/compatible-mode/v1", so we strip one.
    base = rt["base_url"].rstrip("/")
    if base.endswith("/v1"):
        base = base[:-3]
    os.environ["OPENAI_API_KEY"] = rt["api_key"]
    compressor = mods["EngramCompressor"](
        base_url=base,
        model=rt["model"],
        max_tokens=1024,
        timeout=180,
        use_reflector=True,
    )
    return _run(compressor, "Engram", sample_id, messages)
