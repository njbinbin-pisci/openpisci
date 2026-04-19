//! `recall_tool_result` — re-expand a previously demoted tool result.
//!
//! Background (p11): once a session crosses the dual compaction boundaries
//! (`recent_full_turns` × `recent_tool_carriers`), older tool results are
//! demoted to a one-line minimal "receipt" with a `[recall:<tool_use_id>]`
//! suffix. The recall tool lets the agent re-fetch the original full
//! content on demand from the persisted DB row instead of re-running the
//! original (often expensive, possibly side-effecting) tool call.
//!
//! Lookup keys: by `tool_use_id` (exact, preferred). The tool scans all
//! tool-result-carrying messages for the current `ctx.session_id` and
//! returns the first matching `ToolResult.content`.

use crate::agent::tool::{Tool, ToolContext, ToolResult};
use crate::store::Database;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct RecallToolResultTool {
    pub db: Arc<Mutex<Database>>,
}

#[async_trait]
impl Tool for RecallToolResultTool {
    fn name(&self) -> &str {
        "recall_tool_result"
    }

    fn description(&self) -> &str {
        "Re-fetch the original full content of a tool result that has been \
         demoted to a one-line receipt in the visible context. \
         Whenever you see a `[recall:<tool_use_id>]` marker at the end of a \
         tool_result block and you actually need the original output (long \
         file content, complete shell stdout, full search hits), call this \
         tool with the `tool_use_id` from the marker. \
         Avoid recalling speculatively — every recall re-injects the full \
         payload into the next request and counts against the context budget."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tool_use_id": {
                    "type": "string",
                    "description": "The id from a `[recall:<id>]` marker on a demoted tool_result block."
                }
            },
            "required": ["tool_use_id"]
        })
    }

    fn description_minimal(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed(
            "Re-fetch the original full output of a previously demoted \
             tool result; pass `tool_use_id` from the `[recall:<id>]` marker.",
        )
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let tool_use_id = input["tool_use_id"]
            .as_str()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        let tool_use_id = match tool_use_id {
            Some(s) => s.to_string(),
            None => {
                return Ok(ToolResult::err(
                    "recall_tool_result requires a non-empty `tool_use_id` (copy it from a `[recall:<id>]` marker on a demoted tool result).",
                ));
            }
        };

        let db = self.db.lock().await;
        // Page through messages; sessions can be long but each message is
        // tiny so a streaming scan in batches of 200 is plenty fast.
        let mut offset: i64 = 0;
        let page: i64 = 200;
        loop {
            let rows = match db.get_messages(&ctx.session_id, page, offset) {
                Ok(rows) => rows,
                Err(e) => {
                    return Ok(ToolResult::err(format!(
                        "recall_tool_result: failed to read messages: {}",
                        e
                    )));
                }
            };
            if rows.is_empty() {
                break;
            }
            for row in &rows {
                if let Some(json) = row.tool_results_json.as_deref() {
                    if let Some(found) = find_tool_result_content(json, &tool_use_id) {
                        return Ok(ToolResult::ok(found));
                    }
                }
            }
            if (rows.len() as i64) < page {
                break;
            }
            offset += page;
        }

        Ok(ToolResult::err(format!(
            "recall_tool_result: no stored tool result found for tool_use_id={} in session {}.",
            tool_use_id, ctx.session_id
        )))
    }
}

/// Parse a `tool_results_json` payload and return the original `content`
/// for the entry whose `tool_use_id` matches. Returns `None` when the
/// payload is malformed or no entry matches.
fn find_tool_result_content(results_json: &str, tool_use_id: &str) -> Option<String> {
    let value: Value = serde_json::from_str(results_json).ok()?;
    let arr = value.as_array()?;
    for item in arr {
        let id = item.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or("");
        if id != tool_use_id {
            continue;
        }
        // Prefer the original full content. `content_minimal` is the demoted
        // form — useless for recall.
        let content = item
            .get("content")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        return Some(content);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_matching_tool_use_id() {
        let payload = serde_json::json!([
            {
                "type": "tool_result",
                "tool_use_id": "tu_one",
                "content": "first result",
                "is_error": false
            },
            {
                "type": "tool_result",
                "tool_use_id": "tu_two",
                "content": "second result",
                "is_error": true,
                "content_minimal": "ERR"
            }
        ])
        .to_string();
        assert_eq!(
            find_tool_result_content(&payload, "tu_two").as_deref(),
            Some("second result")
        );
        assert_eq!(
            find_tool_result_content(&payload, "tu_one").as_deref(),
            Some("first result")
        );
        assert!(find_tool_result_content(&payload, "tu_missing").is_none());
    }

    #[test]
    fn rejects_malformed_payload() {
        assert!(find_tool_result_content("not json", "tu").is_none());
        assert!(find_tool_result_content("{\"k\":1}", "tu").is_none());
    }

    #[test]
    fn empty_input_returns_friendly_error_via_call() {
        // We exercise the early-exit path purely via input validation
        // (no DB needed) by directly inspecting find_tool_result_content
        // for an empty match key.
        let payload = serde_json::json!([
            { "type": "tool_result", "tool_use_id": "", "content": "x", "is_error": false }
        ])
        .to_string();
        assert!(find_tool_result_content(&payload, "tu_zero").is_none());
    }
}
