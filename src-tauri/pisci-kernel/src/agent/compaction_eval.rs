//! Offline evaluation harness for the dual-version tool-result compaction
//! pipeline. Reads a real SQLite database (populated by an actual running
//! session), reconstructs the in-memory `LlmMessage` log as the agent loop
//! would have seen it, then runs it through `build_request_messages` and
//! reports token / receipt statistics.
//!
//! Enable with:
//! ```text
//! $env:PISCI_EVAL_DB = "C:\path\to\pisci-copy.db"
//! $env:PISCI_EVAL_TOP_N = "5"            # optional, default 5
//! $env:PISCI_EVAL_MIN_MESSAGES = "40"    # optional, default 40
//! $env:PISCI_EVAL_MIN_REDUCTION_PCT = "30"
//!     # optional, enables the p9 acceptance assertion — aggregate
//!     # request/full ratio must save ≥ this percent vs the raw full
//!     # log. Absent => no assertion, the test only prints.
//! cargo test --manifest-path src-tauri/Cargo.toml --lib -- \
//!     --ignored --nocapture agent::compaction_eval
//! ```
//!
//! Never runs in CI (marked `#[ignore]`). Always operate on a *copy* of the
//! production DB — `Database::open` runs migrations and enables WAL mode.

#![cfg(test)]
#![allow(clippy::too_many_lines, clippy::manual_checked_ops)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde_json::Value as Json;

use crate::agent::compaction::CTX_KEEP_RECENT_TOOL_CARRIERS;
use crate::agent::loop_::{build_request_messages, CTX_FULL_TURNS};
use crate::agent::message_utils::{
    collapse_superseded_tool_failures, sanitize_tool_use_result_pairing,
};
use crate::agent::tool_receipt::{render_receipt, RECEIPT_MAX_CHARS};
use crate::llm::{estimate_message_tokens, ContentBlock, MessageContent};
use crate::store::db::{ChatMessage, Database, Session};

/// Fully reconstructed view of a single DB row as seen by the agent loop.
struct Reconstructed {
    llm_msg: crate::llm::LlmMessage,
    /// For assistant rows carrying tool_calls_json: map of tool_use_id → tool_name.
    tool_names_here: Vec<(String, String)>,
    /// For user rows carrying tool_results_json: per tool_result block info
    /// (tool_use_id, content, is_error, content_minimal_in_db, tool_name_in_db).
    tool_results_here: Vec<ToolResultRow>,
}

struct ToolResultRow {
    tool_use_id: String,
    full_content: String,
    is_error: bool,
    content_minimal_in_db: Option<String>,
    tool_name_in_db: Option<String>,
}

fn reconstruct_row(msg: &ChatMessage) -> Reconstructed {
    let mut tool_names_here: Vec<(String, String)> = Vec::new();
    let mut tool_results_here: Vec<ToolResultRow> = Vec::new();

    if let Some(ref json_str) = msg.tool_calls_json {
        if let Ok(Json::Array(arr)) = serde_json::from_str::<Json>(json_str) {
            for item in &arr {
                if item.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
                    continue;
                }
                let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if !id.is_empty() {
                    tool_names_here.push((id.to_string(), name.to_string()));
                }
            }
        }
    }

    if let Some(ref json_str) = msg.tool_results_json {
        if let Ok(Json::Array(arr)) = serde_json::from_str::<Json>(json_str) {
            for item in &arr {
                if item.get("type").and_then(|v| v.as_str()) != Some("tool_result") {
                    continue;
                }
                tool_results_here.push(ToolResultRow {
                    tool_use_id: item
                        .get("tool_use_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    full_content: item
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    is_error: item
                        .get("is_error")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    content_minimal_in_db: item
                        .get("content_minimal")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string()),
                    tool_name_in_db: item
                        .get("tool_name")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string()),
                });
            }
        }
    }

    // Compose the LlmMessage the same way the agent loop accumulates it in
    // memory during a live run:
    //   - tool_results_json set  → Blocks(results) (user role, tool-result carrier)
    //   - tool_calls_json set    → Blocks([Text? + ToolUse+]) (assistant with tool_use)
    //   - otherwise              → MessageContent::Text(content) (plain text turn)
    //
    // Getting this last case right is critical: `build_request_messages`
    // identifies user-turn boundaries by `role == "user"` AND
    // `MessageContent::Text(..)`, so wrapping a plain user message in
    // `Blocks([Text])` would silently defeat all Level-1 demotion.
    let llm_msg = if let Some(ref json) = msg.tool_results_json {
        let results = serde_json::from_str::<Vec<ContentBlock>>(json).unwrap_or_default();
        crate::llm::LlmMessage {
            role: msg.role.clone(),
            content: MessageContent::Blocks(results),
        }
    } else if let Some(ref json) = msg.tool_calls_json {
        let mut blocks: Vec<ContentBlock> = Vec::new();
        if !msg.content.is_empty() {
            blocks.push(ContentBlock::Text {
                text: msg.content.clone(),
            });
        }
        if let Ok(calls) = serde_json::from_str::<Vec<ContentBlock>>(json) {
            blocks.extend(calls);
        }
        let content = if blocks.is_empty() {
            MessageContent::text(&msg.content)
        } else {
            MessageContent::Blocks(blocks)
        };
        crate::llm::LlmMessage {
            role: msg.role.clone(),
            content,
        }
    } else {
        crate::llm::LlmMessage {
            role: msg.role.clone(),
            content: MessageContent::text(&msg.content),
        }
    };

    Reconstructed {
        llm_msg,
        tool_names_here,
        tool_results_here,
    }
}

/// Stats collected per session.
#[derive(Default)]
struct SessionStats {
    id: String,
    title: String,
    message_count: usize,
    /// Number of user-text turn boundaries (what `build_request_messages`
    /// counts). Demotion only fires when this is > CTX_FULL_TURNS.
    user_text_turns: usize,
    total_tool_results: usize,
    /// Tool results whose DB row already had `content_minimal` persisted.
    minimal_in_db: usize,
    /// Tool results that needed a receipt backfill (legacy rows).
    needs_backfill: usize,
    /// Tool results that sit in the "recent window" and stay full.
    kept_full: usize,
    /// Tool results that get demoted to their minimal receipt on this request.
    demoted: usize,
    /// Tokens of the in-memory full log (what compact_summarise sees).
    full_log_tokens: usize,
    /// Tokens of the request payload after demotion (what LLM sees).
    request_tokens: usize,
    /// If we force-demote EVERY tool_result older than the last
    /// CTX_FULL_TURNS × 5 (= 15) tool_result messages regardless of user-turn
    /// boundaries, what would the request size be? Sanity-check upper bound on
    /// the theoretical maximum savings.
    forced_demote_request_tokens: usize,
    /// Anomalies.
    empty_receipts: usize,
    receipts_over_limit: usize,
    empty_tool_use_ids: usize,
    duplicate_tool_use_ids: usize,
    tool_use_id_orphans: usize, // tool_result referring to an id we never saw a tool_call for
    /// Receipt length stats.
    receipt_len_sum: usize,
    receipt_len_max: usize,
    /// Largest raw (pre-demotion) tool_result content seen in this session.
    max_full_result_chars: usize,
    max_full_result_tool: String,
    /// Tool-type distribution among demoted results (by tool_name).
    per_tool: HashMap<String, PerTool>,
}

#[derive(Default, Clone)]
struct PerTool {
    total: usize,
    demoted: usize,
    receipt_len_sum: usize,
    avg_full_len_sum: usize,
}

/// Token count of a hypothetical request where every tool_result block older
/// than the configured recent tool-carrier window is swapped for its receipt,
/// regardless of user-turn boundaries. This remains an upper-bound sanity
/// check for highly autonomous sessions, but now tracks the same carrier
/// count constant as production.
fn compute_forced_demote_tokens(
    messages: &[crate::llm::LlmMessage],
    minimals: &HashMap<String, String>,
) -> usize {
    let mut tool_carrier_indices: Vec<usize> = Vec::new();
    for (i, msg) in messages.iter().enumerate() {
        if let MessageContent::Blocks(blocks) = &msg.content {
            if blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
            {
                tool_carrier_indices.push(i);
            }
        }
    }
    let cutoff_index = if tool_carrier_indices.len() <= CTX_KEEP_RECENT_TOOL_CARRIERS {
        0
    } else {
        tool_carrier_indices[tool_carrier_indices.len() - CTX_KEEP_RECENT_TOOL_CARRIERS]
    };

    let mut out: Vec<crate::llm::LlmMessage> = Vec::with_capacity(messages.len());
    for (i, msg) in messages.iter().enumerate() {
        if i >= cutoff_index {
            out.push(msg.clone());
            continue;
        }
        match &msg.content {
            MessageContent::Blocks(blocks) => {
                let new_blocks: Vec<ContentBlock> = blocks
                    .iter()
                    .map(|b| match b {
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => ContentBlock::ToolResult {
                            tool_use_id: tool_use_id.clone(),
                            content: minimals
                                .get(tool_use_id)
                                .cloned()
                                .unwrap_or_else(|| content.clone()),
                            is_error: *is_error,
                        },
                        other => other.clone(),
                    })
                    .collect();
                out.push(crate::llm::LlmMessage {
                    role: msg.role.clone(),
                    content: MessageContent::Blocks(new_blocks),
                });
            }
            _ => out.push(msg.clone()),
        }
    }
    out.iter().map(estimate_message_tokens).sum()
}

fn eval_session(db: &Database, session: &Session) -> anyhow::Result<SessionStats> {
    let messages = db.get_messages(&session.id, 100_000, 0)?;
    let mut stats = SessionStats {
        id: session.id.clone(),
        title: session.title.clone().unwrap_or_default(),
        message_count: messages.len(),
        ..Default::default()
    };

    for m in &messages {
        let is_text_user = m.role == "user"
            && m.tool_results_json.is_none()
            && !m.content.is_empty()
            && !m.content.starts_with("[会话滚动摘要]")
            && !m.content.starts_with("[对话摘要]");
        if is_text_user {
            stats.user_text_turns += 1;
        }
    }

    // First pass: reconstruct all rows and build side-maps.
    let mut reconstructed: Vec<Reconstructed> = Vec::with_capacity(messages.len());
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut tool_minimals: HashMap<String, String> = HashMap::new();
    let mut seen_tool_use_ids: HashSet<String> = HashSet::new();
    let mut tool_call_ids_issued: HashSet<String> = HashSet::new();

    for msg in &messages {
        let r = reconstruct_row(msg);

        for (id, name) in &r.tool_names_here {
            tool_names.insert(id.clone(), name.clone());
            tool_call_ids_issued.insert(id.clone());
        }

        for row in &r.tool_results_here {
            stats.total_tool_results += 1;

            if row.tool_use_id.is_empty() {
                stats.empty_tool_use_ids += 1;
            } else if !seen_tool_use_ids.insert(row.tool_use_id.clone()) {
                stats.duplicate_tool_use_ids += 1;
            }
            if !row.tool_use_id.is_empty() && !tool_call_ids_issued.contains(&row.tool_use_id) {
                stats.tool_use_id_orphans += 1;
            }

            let tool_name = row
                .tool_name_in_db
                .clone()
                .or_else(|| tool_names.get(&row.tool_use_id).cloned())
                .unwrap_or_else(|| "unknown".to_string());

            let receipt = if let Some(ref m) = row.content_minimal_in_db {
                stats.minimal_in_db += 1;
                m.clone()
            } else {
                stats.needs_backfill += 1;
                render_receipt(
                    &tool_name,
                    &Json::Null,
                    &row.full_content,
                    row.is_error,
                    None,
                )
            };

            if receipt.is_empty() {
                stats.empty_receipts += 1;
            }
            if receipt.chars().count() > RECEIPT_MAX_CHARS {
                stats.receipts_over_limit += 1;
            }
            stats.receipt_len_sum += receipt.chars().count();
            stats.receipt_len_max = stats.receipt_len_max.max(receipt.chars().count());

            let full_chars = row.full_content.chars().count();
            if full_chars > stats.max_full_result_chars {
                stats.max_full_result_chars = full_chars;
                stats.max_full_result_tool = tool_name.clone();
            }

            let per = stats.per_tool.entry(tool_name.clone()).or_default();
            per.total += 1;
            per.receipt_len_sum += receipt.chars().count();
            per.avg_full_len_sum += full_chars;

            if !row.tool_use_id.is_empty() {
                tool_minimals.insert(row.tool_use_id.clone(), receipt);
            }
        }

        reconstructed.push(r);
    }

    // Build the full in-memory log, then apply the same pre-request cleanup
    // stages as production before demotion.
    let full_log: Vec<crate::llm::LlmMessage> =
        reconstructed.iter().map(|r| r.llm_msg.clone()).collect();

    stats.full_log_tokens = full_log.iter().map(estimate_message_tokens).sum();

    let pre_request_log =
        sanitize_tool_use_result_pairing(collapse_superseded_tool_failures(full_log.clone()));
    let request = build_request_messages(
        &pre_request_log,
        &tool_minimals,
        CTX_FULL_TURNS,
        CTX_KEEP_RECENT_TOOL_CARRIERS,
    );
    stats.request_tokens = request.iter().map(estimate_message_tokens).sum();

    // Hypothetical: what if we demoted every tool_result whose carrier row is
    // not among the last 15 tool_result-carrier rows, irrespective of user-turn
    // boundaries? Answers "how much could we save with a turn-agnostic
    // policy?".
    stats.forced_demote_request_tokens =
        compute_forced_demote_tokens(&pre_request_log, &tool_minimals);

    // Inspect the actual post-demotion request payload so eval stays in lockstep
    // with production even as boundary logic evolves.
    for msg in &request {
        let MessageContent::Blocks(blocks) = &msg.content else {
            continue;
        };
        for block in blocks {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } = block
            {
                if tool_use_id.is_empty() {
                    continue;
                }
                let tool_name = tool_names
                    .get(tool_use_id)
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                let expected_minimal = tool_minimals.get(tool_use_id);
                let is_demoted = expected_minimal.is_some_and(|m| m == content);
                if is_demoted {
                    stats.demoted += 1;
                } else {
                    stats.kept_full += 1;
                }
                if let Some(per) = stats.per_tool.get_mut(&tool_name) {
                    if is_demoted {
                        per.demoted += 1;
                    }
                }
            }
        }
    }

    Ok(stats)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

fn print_report(session: &Session, stats: &SessionStats) {
    let full = stats.full_log_tokens.max(1);
    let req = stats.request_tokens;
    let ratio = (req as f64) / (full as f64);
    let saved = full.saturating_sub(req);
    let avg_receipt = if stats.total_tool_results > 0 {
        stats.receipt_len_sum as f64 / stats.total_tool_results as f64
    } else {
        0.0
    };

    let short_id: String = stats.id.chars().take(20).collect();
    println!();
    println!("══════════════════════════════════════════════════════════════════════");
    println!(
        "session {:<20}  (msgs={}, tool_results={}, user_turns={}, rs_ver={})",
        short_id,
        stats.message_count,
        stats.total_tool_results,
        stats.user_text_turns,
        session.rolling_summary_version,
    );
    if !stats.title.is_empty() {
        let title: String = stats.title.chars().take(60).collect();
        println!("title    {}", title);
    }
    println!("──────────────────────────────────────────────────────────────────────");
    let forced_ratio = if full > 0 {
        (stats.forced_demote_request_tokens as f64) / (full as f64)
    } else {
        0.0
    };
    println!(
        "tokens   full_log={:>7}   request={:>7}   saved={:>7}   ratio={:.1}%",
        full,
        req,
        saved,
        ratio * 100.0
    );
    println!(
        "         forced_demote_request={:>7}   ratio={:.1}%   (hypothetical policy)",
        stats.forced_demote_request_tokens,
        forced_ratio * 100.0
    );
    println!(
        "results  demoted={:>4}   kept_full={:>3}   minimal_in_db={:>4}   backfilled={:>4}",
        stats.demoted, stats.kept_full, stats.minimal_in_db, stats.needs_backfill
    );
    println!(
        "receipts avg_len={:.0}  max_len={}  over_limit={}  empty={}",
        avg_receipt, stats.receipt_len_max, stats.receipts_over_limit, stats.empty_receipts
    );
    println!(
        "biggest  tool={} full_chars={}",
        if stats.max_full_result_tool.is_empty() {
            "n/a"
        } else {
            &stats.max_full_result_tool
        },
        stats.max_full_result_chars
    );
    if stats.empty_tool_use_ids + stats.duplicate_tool_use_ids + stats.tool_use_id_orphans > 0 {
        println!(
            "anomaly  empty_ids={}  dup_ids={}  orphan_ids={}",
            stats.empty_tool_use_ids, stats.duplicate_tool_use_ids, stats.tool_use_id_orphans
        );
    }

    // Per-tool breakdown, sorted by demoted desc.
    let mut rows: Vec<(&String, &PerTool)> = stats.per_tool.iter().collect();
    rows.sort_by(|a, b| {
        b.1.demoted
            .cmp(&a.1.demoted)
            .then(b.1.total.cmp(&a.1.total))
    });
    if !rows.is_empty() {
        println!("per-tool (top 10 by demoted count):");
        println!(
            "  {:<28}{:>7}{:>10}{:>12}{:>12}",
            "tool", "total", "demoted", "avg_full", "avg_recpt"
        );
        for (name, per) in rows.iter().take(10) {
            let avg_full = if per.total > 0 {
                per.avg_full_len_sum / per.total
            } else {
                0
            };
            let avg_recpt = if per.total > 0 {
                per.receipt_len_sum / per.total
            } else {
                0
            };
            let display: String = name.chars().take(28).collect();
            println!(
                "  {:<28}{:>7}{:>10}{:>12}{:>12}",
                display, per.total, per.demoted, avg_full, avg_recpt
            );
        }
    }
}

#[test]
#[ignore = "reads a real DB from $PISCI_EVAL_DB; run manually with --ignored --nocapture"]
fn eval_real_sessions() {
    let db_path = match std::env::var("PISCI_EVAL_DB") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            eprintln!("PISCI_EVAL_DB not set — skipping");
            return;
        }
    };
    let top_n = env_usize("PISCI_EVAL_TOP_N", 5);
    let min_messages = env_usize("PISCI_EVAL_MIN_MESSAGES", 40);

    let db = Database::open(&db_path).expect("open db");
    let mut sessions = db.list_sessions(10_000, 0).expect("list");
    sessions.retain(|s| s.message_count as usize >= min_messages);
    sessions.sort_by_key(|s| std::cmp::Reverse(s.message_count));
    sessions.truncate(top_n);

    if sessions.is_empty() {
        eprintln!(
            "no sessions with >= {} messages in {:?}",
            min_messages, db_path
        );
        return;
    }

    println!();
    println!(
        "evaluating top {} long sessions from {:?}",
        sessions.len(),
        db_path
    );

    let mut agg_full: usize = 0;
    let mut agg_request: usize = 0;
    let mut agg_forced: usize = 0;
    let mut agg_demoted: usize = 0;
    let mut agg_kept: usize = 0;
    let mut agg_backfill: usize = 0;
    let mut agg_minimal_in_db: usize = 0;
    let mut agg_empty_receipts: usize = 0;
    let mut agg_over_limit: usize = 0;
    let mut level1_never_fires: usize = 0;

    for session in &sessions {
        match eval_session(&db, session) {
            Ok(stats) => {
                print_report(session, &stats);
                agg_full += stats.full_log_tokens;
                agg_request += stats.request_tokens;
                agg_forced += stats.forced_demote_request_tokens;
                agg_demoted += stats.demoted;
                agg_kept += stats.kept_full;
                agg_backfill += stats.needs_backfill;
                agg_minimal_in_db += stats.minimal_in_db;
                agg_empty_receipts += stats.empty_receipts;
                agg_over_limit += stats.receipts_over_limit;
                if stats.demoted == 0 && stats.total_tool_results > 30 {
                    level1_never_fires += 1;
                }
            }
            Err(e) => {
                eprintln!("session {} failed: {}", session.id, e);
            }
        }
    }

    println!();
    println!("══════════════════════════════════════════════════════════════════════");
    println!("AGGREGATE over {} sessions", sessions.len());
    println!(
        "  tokens     full={} request={} saved={} ratio={:.1}%",
        agg_full,
        agg_request,
        agg_full.saturating_sub(agg_request),
        if agg_full > 0 {
            (agg_request as f64 * 100.0) / agg_full as f64
        } else {
            0.0
        }
    );
    println!(
        "  forced     request={} saved={} ratio={:.1}%  (upper-bound policy)",
        agg_forced,
        agg_full.saturating_sub(agg_forced),
        if agg_full > 0 {
            (agg_forced as f64 * 100.0) / agg_full as f64
        } else {
            0.0
        }
    );
    println!(
        "  level-1    never fires on {}/{} sessions (single-kickoff autonomous runs)",
        level1_never_fires,
        sessions.len()
    );
    println!(
        "  results    demoted={} kept_full={}",
        agg_demoted, agg_kept
    );
    println!(
        "  receipts   minimal_in_db={} ({:.0}%)  backfilled={}  empty={}  over_limit={}",
        agg_minimal_in_db,
        if agg_minimal_in_db + agg_backfill > 0 {
            agg_minimal_in_db as f64 * 100.0 / (agg_minimal_in_db + agg_backfill) as f64
        } else {
            0.0
        },
        agg_backfill,
        agg_empty_receipts,
        agg_over_limit
    );
    println!("══════════════════════════════════════════════════════════════════════");

    // p9 — optional acceptance gate. When `PISCI_EVAL_MIN_REDUCTION_PCT`
    // is set, fail the test if the aggregate request tokens do not shave
    // at least that percentage off the raw full-log tokens. This is the
    // hook long-session reviewers use to ratchet the compaction policy
    // forward without regressing (the `≥ 30%` top-5 target called out
    // in the unified-harness-context plan).
    if let Ok(raw) = std::env::var("PISCI_EVAL_MIN_REDUCTION_PCT") {
        let min_pct: f64 = raw
            .parse()
            .expect("PISCI_EVAL_MIN_REDUCTION_PCT must be a number, e.g. \"30\" for 30%");
        assert!(
            agg_full > 0,
            "no sessions evaluated — cannot assert reduction"
        );
        let saved_pct = (agg_full.saturating_sub(agg_request) as f64) * 100.0 / agg_full as f64;
        println!();
        println!(
            "p9 acceptance gate: min_required={:.1}%   observed={:.1}%",
            min_pct, saved_pct
        );
        assert!(
            saved_pct >= min_pct,
            "p9 acceptance failed: saved {:.1}% of full-log tokens (required ≥ {:.1}%)",
            saved_pct,
            min_pct
        );
    }
}
