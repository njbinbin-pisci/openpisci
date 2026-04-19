"""Pragmatic information-theory instrumentation for the bench harness.

We do not claim to measure true Shannon mutual information — that would require
a ground-truth joint distribution we don't have. Instead we report four
*operational surrogates* that correlate with the quantities referenced in the
Pisci 压缩内核 v2 plan:

  • H_token_bits_per_tok(text)           — Shannon entropy of whitespace tokens
  • h_conditional_bits_per_tok(orig,comp) — H(orig) × (1 − IR_F1), "what IR-F1
                                            says is still unexplained after
                                            reading the compressed text"
  • mi_approx_bits(orig, ir_f1)          — H(orig) × IR_F1, the symmetric
                                            bound: we never claim to preserve
                                            more bits than IR-F1 admits
  • fano_lower_bound_mi_bits(            — Fano-style lower bound on MI given
      judge_success_rate, n_answer_classes
    )                                       the judge's 0–5 score collapsed to
                                            success/failure. Useful because it
                                            uses downstream task success, not
                                            text overlap.
  • channel_utilization(compressed_tok,  — compressed_tokens / budget, with a
      budget)                               sane default of 8k.

All functions return plain floats and never raise on empty input.
"""

from __future__ import annotations

import math
import re
from collections import Counter

_TOK_RE = re.compile(r"[A-Za-z0-9_]+|[\u4e00-\u9fff]", flags=re.UNICODE)


def _tokenize(text: str) -> list[str]:
    if not text:
        return []
    return _TOK_RE.findall(text.lower())


def token_entropy_bits_per_tok(text: str) -> float:
    toks = _tokenize(text)
    if not toks:
        return 0.0
    total = len(toks)
    counts = Counter(toks)
    h = 0.0
    for c in counts.values():
        p = c / total
        h -= p * math.log2(p)
    return h


def h_conditional_bits_per_tok(orig: str, comp: str, ir_f1: float) -> float:
    """H(X|Y) surrogate using IR-F1 as the channel-fidelity knob.

    Interpretation: how much of X's uncertainty remains after reading Y.
    When IR-F1=1 perfect recall ⇒ 0 bits left; when IR-F1=0 ⇒ all of H(X).
    """
    h_x = token_entropy_bits_per_tok(orig)
    return h_x * (1.0 - max(0.0, min(1.0, ir_f1)))


def mi_approx_bits_per_tok(orig: str, ir_f1: float) -> float:
    h_x = token_entropy_bits_per_tok(orig)
    return h_x * max(0.0, min(1.0, ir_f1))


def fano_lower_bound_mi_bits(
    judge_success_rate: float,
    n_answer_classes: int = 6,
) -> float:
    """Lower-bound on MI from downstream task success, via Fano inequality.

    Fano: H(X|Y) ≤ H(P_e) + P_e · log(N − 1)
    So MI(X;Y) = H(X) − H(X|Y) ≥ log(N) − [H(P_e) + P_e · log(N − 1)]
    assuming a uniform prior over N answer categories.

    - judge_success_rate: probability of correct downstream answer, in [0,1]
    - n_answer_classes: number of distinguishable answer categories (we use 6
      to mirror the judge's 0–5 score scale; caller can override).
    """
    s = max(0.0, min(1.0, judge_success_rate))
    n = max(2, int(n_answer_classes))
    p_e = 1.0 - s
    if p_e <= 0.0:
        h_pe = 0.0
    elif p_e >= 1.0:
        h_pe = 0.0
    else:
        h_pe = -(p_e * math.log2(p_e) + (1.0 - p_e) * math.log2(1.0 - p_e))
    fano = h_pe + p_e * math.log2(n - 1)
    mi_lower = math.log2(n) - fano
    return max(0.0, mi_lower)


def channel_utilization(compressed_tokens: int, budget_tokens: int = 8192) -> float:
    if budget_tokens <= 0:
        return 0.0
    return compressed_tokens / budget_tokens


def summarise(
    original_text: str,
    compressed_text: str,
    ir_f1: float,
    judge_score_0_5: float | None,
    compressed_tokens: int,
    budget_tokens: int = 8192,
) -> dict:
    h_x = token_entropy_bits_per_tok(original_text)
    h_y = token_entropy_bits_per_tok(compressed_text)
    h_cond = h_conditional_bits_per_tok(original_text, compressed_text, ir_f1)
    mi = mi_approx_bits_per_tok(original_text, ir_f1)
    fano = None
    if judge_score_0_5 is not None:
        s = judge_score_0_5 / 5.0
        fano = fano_lower_bound_mi_bits(s)
    util = channel_utilization(compressed_tokens, budget_tokens)
    return {
        "h_orig_bits_per_tok": round(h_x, 4),
        "h_comp_bits_per_tok": round(h_y, 4),
        "h_cond_bits_per_tok": round(h_cond, 4),
        "mi_approx_bits_per_tok": round(mi, 4),
        "fano_lower_bound_mi_bits": round(fano, 4) if fano is not None else None,
        "channel_utilization": round(util, 4),
        "budget_tokens": budget_tokens,
    }
