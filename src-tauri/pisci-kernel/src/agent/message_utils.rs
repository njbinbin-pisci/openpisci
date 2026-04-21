//! Message-manipulation helpers shared between the agent loop and the
//! desktop's chat commands.
//!
//! Prior to the kernel extraction these functions lived in
//! `commands/chat.rs` and were called from the agent loop via
//! `crate::commands::chat::...`. That path no longer exists inside
//! `pisci-kernel`, so the pure helpers (tool-failure coalescing, rolling
//! summary message construction, `SEND_*` marker stripping, tool-use /
//! tool-result pairing sanitisation) are hosted here. The desktop crate
//! re-exports them from `commands::chat` for backward compatibility.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use crate::llm::{ContentBlock, LlmMessage, MessageContent};

/// Number of messages in which a post-failure retry has to succeed for the
/// earlier failed attempt to be considered "superseded" and safely dropped.
pub const SUPERSEDE_RETRY_WINDOW_MSGS: usize = 6;

/// Strip `SEND_FILE:` / `SEND_IMAGE:` directive lines from assistant text.
pub fn strip_send_markers(text: &str) -> Cow<'_, str> {
    if !text.contains("SEND_FILE:") && !text.contains("SEND_IMAGE:") {
        return Cow::Borrowed(text);
    }
    let cleaned: String = text
        .lines()
        .filter(|line| {
            let t = line.trim();
            !t.starts_with("SEND_FILE:") && !t.starts_with("SEND_IMAGE:")
        })
        .collect::<Vec<_>>()
        .join("\n");
    Cow::Owned(cleaned.trim().to_string())
}

/// Build a synthetic `user`-role message that carries the rolling conversation
/// summary into the next LLM request.
pub fn rolling_summary_message(summary: &str) -> LlmMessage {
    LlmMessage {
        role: "user".into(),
        content: MessageContent::text(format!(
            "[\u{4F1A}\u{8BDD}\u{6EDA}\u{52A8}\u{6458}\u{8981}]\n{}\n\n[\u{7CFB}\u{7EDF}\u{63D0}\u{793A}] \u{4E0A}\u{8FF0}\u{6458}\u{8981}\u{8986}\u{76D6}\u{4E86}\u{66F4}\u{65E9}\u{7684}\u{5BF9}\u{8BDD}\u{5386}\u{53F2}\u{FF0C}\u{8BF7}\u{7ED3}\u{5408}\u{540E}\u{7EED}\u{771F}\u{5B9E}\u{6D88}\u{606F}\u{7EE7}\u{7EED}\u{4EFB}\u{52A1}\u{FF0C}\u{4E0D}\u{8981}\u{91CD}\u{590D}\u{5DF2}\u{5B8C}\u{6210}\u{7684}\u{5DE5}\u{4F5C}\u{3002}",
            summary.trim()
        )),
    }
}

fn tool_call_signature(name: &str, input: &serde_json::Value) -> String {
    let mut normalized = input.clone();
    if let Some(obj) = normalized.as_object_mut() {
        obj.remove("_trace_id");
    }
    let input_json = serde_json::to_string(&normalized).unwrap_or_default();
    format!("{}::{}", name, input_json)
}

fn is_tool_result_carrier(msg: &LlmMessage) -> bool {
    matches!(
        &msg.content,
        MessageContent::Blocks(blocks)
            if blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
    )
}

fn has_real_user_turn_between(msgs: &[LlmMessage], start_idx: usize, end_idx: usize) -> bool {
    msgs.iter()
        .enumerate()
        .skip(start_idx.saturating_add(1))
        .take(end_idx.saturating_sub(start_idx.saturating_add(1)))
        .any(|(_, msg)| msg.role == "user" && !is_tool_result_carrier(msg))
}

fn is_retryable_tool_failure(content: &str) -> bool {
    let lower = content.to_lowercase();
    content.contains("[schema_correction tool=")
        || lower.contains("schema \u{4E0D}\u{5339}\u{914D}")
        || lower.contains("missing field")
        || lower.contains("missing required")
        || lower.contains("invalid type")
        || lower.contains("invalid value")
        || lower.contains("unknown field")
        || lower.contains("unknown variant")
        || lower.contains("did not match any variant")
        || lower.contains("additional properties are not allowed")
        || lower.contains("tool '") && lower.contains("does not exist. available tools:")
        || lower.contains("\u{5DE5}\u{5177} '")
            && (lower.contains("\u{672A}\u{627E}\u{5230}")
                || lower.contains("\u{5F53}\u{524D}\u{53EF}\u{7528}\u{5DE5}\u{5177}"))
}

/// Collapse tool-call failures whose effect has been superseded by a later
/// successful retry. Also drops retryable failures that were followed by a
/// same-tool success within a small window.
pub fn collapse_superseded_tool_failures(mut msgs: Vec<LlmMessage>) -> Vec<LlmMessage> {
    let mut tool_use_meta: HashMap<String, (usize, String, String)> = HashMap::new();
    let mut last_success_pos: HashMap<String, usize> = HashMap::new();
    let mut success_by_tool_name: HashMap<String, Vec<usize>> = HashMap::new();

    for (msg_idx, msg) in msgs.iter().enumerate() {
        let MessageContent::Blocks(blocks) = &msg.content else {
            continue;
        };

        for block in blocks {
            if let ContentBlock::ToolUse { id, name, input } = block {
                tool_use_meta.insert(
                    id.clone(),
                    (msg_idx, tool_call_signature(name, input), name.clone()),
                );
            }
        }

        for block in blocks {
            if let ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                ..
            } = block
            {
                if *is_error {
                    continue;
                }
                if let Some((tool_msg_idx, signature, tool_name)) = tool_use_meta.get(tool_use_id) {
                    last_success_pos
                        .entry(signature.clone())
                        .and_modify(|pos| *pos = (*pos).max(*tool_msg_idx))
                        .or_insert(*tool_msg_idx);
                    success_by_tool_name
                        .entry(tool_name.clone())
                        .or_default()
                        .push(*tool_msg_idx);
                }
            }
        }
    }

    if last_success_pos.is_empty() {
        return msgs;
    }

    let mut superseded_tool_use_ids: HashSet<String> = HashSet::new();
    for msg in &msgs {
        let MessageContent::Blocks(blocks) = &msg.content else {
            continue;
        };
        for block in blocks {
            if let ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                content,
                ..
            } = block
            {
                if !*is_error {
                    continue;
                }
                if let Some((tool_msg_idx, signature, tool_name)) = tool_use_meta.get(tool_use_id) {
                    if last_success_pos.get(signature).is_some_and(|success_pos| {
                        tool_msg_idx < success_pos
                            && !has_real_user_turn_between(&msgs, *tool_msg_idx, *success_pos)
                    }) {
                        superseded_tool_use_ids.insert(tool_use_id.clone());
                        continue;
                    }

                    if !is_retryable_tool_failure(content) {
                        continue;
                    }

                    if let Some(success_positions) = success_by_tool_name.get(tool_name) {
                        let matched_retry = success_positions.iter().any(|success_pos| {
                            *success_pos > *tool_msg_idx
                                && success_pos.saturating_sub(*tool_msg_idx)
                                    <= SUPERSEDE_RETRY_WINDOW_MSGS
                                && !has_real_user_turn_between(&msgs, *tool_msg_idx, *success_pos)
                        });
                        if matched_retry {
                            superseded_tool_use_ids.insert(tool_use_id.clone());
                        }
                    }
                }
            }
        }
    }

    if superseded_tool_use_ids.is_empty() {
        return msgs;
    }

    for msg in msgs.iter_mut() {
        let MessageContent::Blocks(blocks) = &mut msg.content else {
            continue;
        };
        blocks.retain(|block| match block {
            ContentBlock::ToolUse { id, .. } => !superseded_tool_use_ids.contains(id),
            ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => !(*is_error && superseded_tool_use_ids.contains(tool_use_id)),
            _ => true,
        });
    }

    msgs.retain(|msg| match &msg.content {
        MessageContent::Blocks(blocks) => !blocks.is_empty(),
        MessageContent::Text(text) => !text.trim().is_empty(),
    });

    tracing::info!(
        "collapse_superseded_tool_failures: removed {} superseded failed tool attempt(s)",
        superseded_tool_use_ids.len()
    );
    msgs
}

/// Strip orphaned ToolUse blocks left over from a cancelled previous turn.
pub fn sanitize_tool_use_result_pairing(mut msgs: Vec<LlmMessage>) -> Vec<LlmMessage> {
    let mut i = 0;
    while i < msgs.len() {
        let has_tool_use = if msgs[i].role == "assistant" {
            match &msgs[i].content {
                MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolUse { .. })),
                _ => false,
            }
        } else {
            i += 1;
            continue;
        };

        if !has_tool_use {
            i += 1;
            continue;
        }

        let next_is_tool_result = msgs
            .get(i + 1)
            .map(|next| {
                next.role == "user"
                    && match &next.content {
                        MessageContent::Blocks(blocks) => blocks
                            .iter()
                            .any(|b| matches!(b, ContentBlock::ToolResult { .. })),
                        _ => false,
                    }
            })
            .unwrap_or(false);

        if !next_is_tool_result {
            tracing::warn!(
                "sanitize_tool_use_result_pairing: stripping orphaned ToolUse at index {}",
                i
            );
            if let MessageContent::Blocks(ref mut blocks) = msgs[i].content {
                blocks.retain(|b| !matches!(b, ContentBlock::ToolUse { .. }));
            }
            let is_empty = match &msgs[i].content {
                MessageContent::Blocks(blocks) => blocks.is_empty(),
                MessageContent::Text(t) => t.trim().is_empty(),
            };
            if is_empty {
                msgs.remove(i);
                continue;
            }
        }
        i += 1;
    }
    msgs
}
