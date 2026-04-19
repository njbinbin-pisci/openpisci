"""LLM-as-judge downstream fidelity scoring.

Protocol:
  1. From each sample, synthesize 2 "hidden questions" whose answers require
     task-critical information that appears in the conversation middle (the
     exact part compressors usually drop). We do this once offline via Qwen
     and cache the result next to the sample.
  2. For each (sample, compressor) pair, ask Qwen to answer the hidden
     questions using ONLY the compressed text. A separate judge call grades
     correctness 0–5 given the original conversation as ground truth.
"""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

from adapters.common import BENCH_DIR, qwen_chat

QUESTIONS_CACHE = BENCH_DIR / "samples" / "_questions_cache.json"
QUESTIONS_CACHE.parent.mkdir(parents=True, exist_ok=True)

QG_SYSTEM = """\
你是评测助手。请基于下面的对话，抽取 2 个 **事实性追问**，用于稍后检测任意压缩版本是否丢失关键信息。
要求：
- 问题必须有**明确唯一答案**（数字、名词、路径、决定、结论），不能是开放性问题
- 2 个问题尽量覆盖对话中段/前半段的关键信息
- 答案必须在原始对话里找得到
- 仅输出 JSON：[{"q":"...","a":"..."}, {"q":"...","a":"..."}]
"""

QG_NICE_SYSTEM = """\
你是评测助手。请基于下面的对话，抽取 1 个 **非关键但合理** 的事实追问（nice-to-have），
用于检验压缩版本是否还保留了次要但有用的上下文。
- 问题必须有明确答案（原对话里能找到）
- 答案不应是 critical_facts 中已有的内容
- 仅输出 JSON：[{"q":"...","a":"..."}]
"""

QG_DISTRACTOR_SYSTEM = """\
你是评测助手。请生成 1 个 **干扰性问题**（distractor），它的答案 **故意不在下方对话里**
（可能是一个与对话话题相关但对话没讨论的名词/数字/路径）。
- 参考答案统一为 "不知道"，用于检验压缩版本是否会产生幻觉（幻觉则应低分）
- 仅输出 JSON：[{"q":"...","a":"不知道"}]
"""

ANS_SYSTEM = """\
你是阅读理解助手。请仅基于下方"压缩上下文"回答问题，不要凭空推测。
若上下文中确实没有答案，请回答"不知道"。回答尽量简短（≤30字）。
"""

JUDGE_SYSTEM = """\
你是评测打分员。给定参考答案和候选答案，按 0–5 分打分：
- 5 = 完全正确，语义等价
- 4 = 基本正确，有少量非关键偏差
- 3 = 部分正确，缺失关键细节
- 2 = 沾边但主要信息错
- 1 = 基本错误或无相关信息
- 0 = 完全错误 / 幻觉 / "不知道"
仅输出一个整数（0-5），不要任何其他文字。
"""


def _qg_cache() -> dict[str, list[dict]]:
    if QUESTIONS_CACHE.exists():
        return json.loads(QUESTIONS_CACHE.read_text(encoding="utf-8"))
    return {}


def _qg_save(cache: dict[str, list[dict]]):
    QUESTIONS_CACHE.write_text(json.dumps(cache, ensure_ascii=False, indent=2), encoding="utf-8")


def _build_conv_text(messages: list[dict]) -> str:
    parts = []
    for m in messages:
        role = (m.get("role") or "?").upper()
        content = m.get("content") or ""
        if isinstance(content, list):
            content = " ".join(
                b.get("text", "") if isinstance(b, dict) and b.get("type") == "text"
                else ("[tool_use:" + b.get("name", "?") + "] " + json.dumps(b.get("input", {}), ensure_ascii=False))
                if isinstance(b, dict) and b.get("type") == "tool_use"
                else ("[tool_result] " + (b.get("content", "") if isinstance(b, dict) else ""))
                for b in content
            )
        parts.append(f"{role}: {content}")
    return "\n\n".join(parts)


def _llm_json_array(system: str, user: str, max_tokens: int = 400) -> list[dict]:
    content, _, _ = qwen_chat(
        [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        max_tokens=max_tokens,
        temperature=0.1,
    )
    m = re.search(r"\[.*\]", content, re.DOTALL)
    if not m:
        raise RuntimeError(f"question-gen: no JSON array in response: {content[:200]}")
    payload = json.loads(m.group(0))
    if not isinstance(payload, list) or not payload:
        raise RuntimeError(f"question-gen: bad payload: {content[:200]}")
    return [{"q": q["q"], "a": q["a"]} for q in payload if "q" in q and "a" in q]


def _critical_from_facts(facts: list[str]) -> list[dict]:
    """Turn a notes_for_judge.critical_facts[] list into critical-kind questions.

    Each fact becomes a (q, a) pair where the question is a factual recall prompt
    and the answer is the fact text itself. This is deterministic and avoids
    an LLM call per fact, making Phase-5 critical coverage cheap and stable.
    """
    out: list[dict] = []
    for idx, fact in enumerate(facts[:4]):
        out.append({
            "q": f"根据之前的对话，请精确回答：{fact.split('.')[0] if '.' in fact else fact[:80]}。 完整回答一句话。",
            "a": fact,
            "kind": "critical",
        })
    return out


def generate_questions(sample_id: str, messages: list[dict], sample: dict | None = None) -> list[dict]:
    """Return a list of {"q","a","kind"} dicts. Cached per sample_id.

    kind ∈ {"critical", "nice", "distractor"}. When the sample provides
    `notes_for_judge.critical_facts`, those facts become deterministic
    critical-kind questions and two LLM calls add 1 nice + 1 distractor.
    Otherwise falls back to the legacy 2-question LLM generator (kind=critical).
    """
    cache = _qg_cache()
    if sample_id in cache and cache[sample_id] and all("kind" in q for q in cache[sample_id]):
        return cache[sample_id]

    conv = _build_conv_text(messages)
    if len(conv) > 12000:
        conv = conv[:12000] + "\n...[截断]"

    qs: list[dict] = []
    facts = []
    if sample and isinstance(sample.get("notes_for_judge"), dict):
        facts = sample["notes_for_judge"].get("critical_facts") or []

    if facts:
        qs.extend(_critical_from_facts(facts))
        try:
            nice = _llm_json_array(QG_NICE_SYSTEM, conv, max_tokens=200)
            for q in nice[:1]:
                qs.append({"q": q["q"], "a": q["a"], "kind": "nice"})
        except Exception:
            pass
        try:
            dist = _llm_json_array(QG_DISTRACTOR_SYSTEM, conv, max_tokens=200)
            for q in dist[:1]:
                qs.append({"q": q["q"], "a": q["a"], "kind": "distractor"})
        except Exception:
            pass
    else:
        legacy = _llm_json_array(QG_SYSTEM, conv, max_tokens=400)
        for q in legacy[:2]:
            qs.append({"q": q["q"], "a": q["a"], "kind": "critical"})

    cache[sample_id] = qs
    _qg_save(cache)
    return qs


def answer_from_compressed(compressed_text: str, question: str) -> str:
    ctx = compressed_text
    if len(ctx) > 16000:
        ctx = ctx[:16000] + "\n...[截断]"
    content, _, _ = qwen_chat(
        [
            {"role": "system", "content": ANS_SYSTEM},
            {"role": "user", "content": f"压缩上下文：\n{ctx}\n\n问题：{question}"},
        ],
        max_tokens=150,
        temperature=0.0,
    )
    return content


def judge_answer(reference: str, candidate: str) -> int:
    content, _, _ = qwen_chat(
        [
            {"role": "system", "content": JUDGE_SYSTEM},
            {
                "role": "user",
                "content": f"参考答案：{reference}\n候选答案：{candidate}\n\n打分：",
            },
        ],
        max_tokens=10,
        temperature=0.0,
    )
    m = re.search(r"[0-5]", content)
    return int(m.group(0)) if m else 0


def score_compression(
    sample_id: str,
    original_messages: list[dict],
    compressed_text: str,
    sample: dict | None = None,
) -> dict:
    """Return {avg_score, per_kind, per_question, n} for one (sample, compressor) pair.

    When the sample provides `notes_for_judge.critical_facts`, questions are
    classified as critical / nice / distractor, and scores are reported
    separately per kind. For distractors, an "I don't know" answer scores 5
    and a confident-but-wrong answer scores 0 (hallucination penalty).
    """
    questions = generate_questions(sample_id, original_messages, sample)
    per_q = []
    for qa in questions:
        q, ref, kind = qa["q"], qa["a"], qa.get("kind", "critical")
        try:
            cand = answer_from_compressed(compressed_text, q)
            if kind == "distractor":
                if any(tok in cand for tok in ["不知道", "未提及", "没有", "无法", "not in", "unknown", "N/A"]):
                    score = 5
                else:
                    score = judge_answer(ref, cand)
            else:
                score = judge_answer(ref, cand)
        except Exception as e:
            cand = f"__error__: {e}"
            score = 0
        per_q.append({"q": q, "ref": ref, "cand": cand, "score": score, "kind": kind})

    avg = sum(p["score"] for p in per_q) / len(per_q) if per_q else 0.0
    per_kind: dict[str, dict[str, float]] = {}
    for kind in ("critical", "nice", "distractor"):
        ks = [p["score"] for p in per_q if p["kind"] == kind]
        if ks:
            per_kind[kind] = {
                "avg": round(sum(ks) / len(ks), 3),
                "n": len(ks),
            }
    return {
        "avg_score": avg,
        "per_kind": per_kind,
        "per_question": per_q,
        "n": len(per_q),
    }
