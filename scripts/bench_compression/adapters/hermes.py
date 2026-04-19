"""Hermes adapter: imports references/hermes-agent and runs ContextCompressor."""

from __future__ import annotations

import os
import sys
import time
from typing import Any

from .common import CompressorResult, HERMES_DIR, get_qwen_runtime

_IMPORT_ONCE: dict[str, Any] = {}


def _ensure_hermes_imported():
    """Lazy-import hermes ContextCompressor with the right sys.path."""
    if "ContextCompressor" in _IMPORT_ONCE:
        return _IMPORT_ONCE

    hermes_root = str(HERMES_DIR)
    if hermes_root not in sys.path:
        sys.path.insert(0, hermes_root)

    # Route Hermes's auxiliary_client to our Qwen endpoint via env vars
    # (belt-and-suspenders with the explicit main_runtime we pass in).
    rt = get_qwen_runtime()
    os.environ.setdefault("OPENAI_API_KEY", rt["api_key"])
    os.environ.setdefault("OPENAI_BASE_URL", rt["base_url"])

    from agent.context_compressor import ContextCompressor  # type: ignore
    from agent.context_engine import ContextEngine  # type: ignore

    _IMPORT_ONCE["ContextCompressor"] = ContextCompressor
    _IMPORT_ONCE["ContextEngine"] = ContextEngine
    return _IMPORT_ONCE


def _count_llm_calls(compressor) -> int:
    for attr in ("compression_count", "n_llm_calls", "llm_calls"):
        v = getattr(compressor, attr, None)
        if isinstance(v, int):
            return v
    return 1  # safe default — every compress() triggers at least one


def _flatten_messages(messages: list[dict]) -> str:
    parts = []
    for m in messages:
        role = (m.get("role") or "?").upper()
        content = m.get("content") or ""
        ts = m.get("ts") or ""
        if ts:
            parts.append(f"[{ts}] {role}: {content}")
        else:
            parts.append(f"{role}: {content}")
    return "\n\n".join(parts)


def compress_hermes(sample_id: str, messages: list[dict], keep_tokens: int = 2000) -> CompressorResult:
    """Run Hermes's ContextCompressor.compress on the sample messages.

    We initialise the compressor with a *small* context_length so the internal
    threshold logic always decides to compress the middle section.
    """
    start = time.perf_counter()

    try:
        mods = _ensure_hermes_imported()
        ContextCompressor = mods["ContextCompressor"]
        rt = get_qwen_runtime()

        # Force a tiny context window → threshold is tiny → compression always
        # fires. This makes the test *apples to apples* with other compressors
        # that also always compress.
        # We set context_length ≈ 2 × keep_tokens so threshold fires immediately.
        config_context_length = max(4_096, keep_tokens * 2)

        compressor = ContextCompressor(
            model=rt["model"],
            threshold_percent=0.50,
            protect_first_n=3,
            protect_last_n=3,                # keep head short, squeeze the tail too
            summary_target_ratio=0.20,
            quiet_mode=True,
            base_url=rt["base_url"],
            api_key=rt["api_key"],
            config_context_length=config_context_length,
            provider="custom",
            api_mode="chat_completions",
        )

        # Hermes uses messages dicts with role+content.
        # Our sample schema uses role+content+ts (ts is harmless extra field).
        compressed_msgs = compressor.compress(messages, current_tokens=None, focus_topic=None)

        # Flatten to comparable text
        compressed_text = _flatten_messages(compressed_msgs)

        # Rough CJK-aware token count (same estimator we use globally)
        from benchmark.evaluate import estimate_tokens as _estimate  # type: ignore
        original_tokens = _estimate(_flatten_messages(messages))
        compressed_tokens = _estimate(compressed_text)

        latency_ms = (time.perf_counter() - start) * 1000.0

        return CompressorResult(
            sample_id=sample_id,
            compressor="Hermes",
            compressed_text=compressed_text,
            compressed_tokens=compressed_tokens,
            original_tokens=original_tokens,
            latency_ms=latency_ms,
            llm_calls=_count_llm_calls(compressor),
            notes={
                "provider": "custom→qwen",
                "model": rt["model"],
                "context_length": config_context_length,
                "n_input": len(messages),
                "n_output": len(compressed_msgs),
            },
        )
    except Exception as e:
        return CompressorResult(
            sample_id=sample_id,
            compressor="Hermes",
            compressed_text="",
            error=f"{type(e).__name__}: {e}",
            latency_ms=(time.perf_counter() - start) * 1000.0,
        )
