//! Rule-based minimal "receipt" generator for tool results.
//!
//! Every tool result has two versions in our persistence layer:
//! - the *full* content (what the LLM sees on the current and next few turns),
//! - the *minimal* receipt rendered here (what the LLM sees once the turn falls
//!   out of the recent-turns window but before Level-2 summarisation kicks in).
//!
//! A receipt answers: "what did this tool do and what did it produce?" — enough
//! for the agent to reason about the session timeline, without burning tokens
//! on details it no longer needs. It is NOT a substitute for Level-2 summary;
//! Level-2 always restores the full content when it runs.
//!
//! All templates here must be:
//! - deterministic (same inputs → same output),
//! - cheap (no LLM calls, no IO),
//! - bounded (`RECEIPT_MAX_CHARS`),
//! - signal-preserving for errors (first error line is always included when
//!   `is_error == true`).

use serde_json::Value;

/// Hard upper bound on a receipt string. Receipts longer than this are truncated
/// with an ellipsis. Most receipts are well under 120 chars.
pub const RECEIPT_MAX_CHARS: usize = 200;

/// Render a minimal receipt for a tool invocation.
///
/// `image_artifact_id` is the vision-store artifact id when the tool attached
/// an image to its result, so the agent can still reference it via
/// `vision_context` after the full content has been swapped out.
pub fn render_receipt(
    tool_name: &str,
    input: &Value,
    full: &str,
    is_error: bool,
    image_artifact_id: Option<&str>,
) -> String {
    // Auto-detect an inline `[vision_artifact] id=...` marker if the caller did
    // not supply one explicitly. execute_single_tool appends such a marker to
    // the guarded content whenever the tool attaches an image, so this lets us
    // regenerate receipts from DB rows later with the same signal.
    let extracted_id;
    let image_artifact_id = match image_artifact_id {
        Some(id) => Some(id),
        None => {
            extracted_id = extract_vision_artifact_id(full);
            extracted_id.as_deref()
        }
    };

    let body = match tool_name {
        "shell" | "powershell" => render_shell(input, full, is_error),
        "powershell_query" | "wmi" => render_query(tool_name, input, full, is_error),
        "file_read" => render_file_read(input, full, is_error),
        "file_write" => render_file_write(input, full, is_error),
        "file_edit" | "file_diff" => render_file_edit(tool_name, input, full, is_error),
        "file_list" | "file_search" => render_file_listing(tool_name, input, full, is_error),
        "web_search" => render_web_search(input, full, is_error),
        "browser" => render_browser(input, full, is_error),
        "plan_update" | "plan_todo" => render_plan(input, full),
        "memory_store" | "memory_recall" | "memory_search" | "memory_list" => {
            render_memory(tool_name, input, full, is_error)
        }
        "skill_list" | "skills_list" => render_skill_list(full),
        "vision_context" => render_vision(input, full),
        "chat_ui" => render_chat_ui(is_error),
        "screen_capture" => render_screen_capture(input, full, is_error),
        "call_fish" | "call_koi" | "pool_chat" | "pool_org" => {
            render_subagent(tool_name, input, full, is_error)
        }
        other if other.starts_with("mcp_") || other.starts_with("user_tool:") => {
            render_external(other, full, is_error)
        }
        _ => render_fallback(tool_name, full, is_error),
    };

    let mut out = body;
    if let Some(id) = image_artifact_id {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&format!("+image(id={})", truncate(id, 16)));
    }
    truncate(&out, RECEIPT_MAX_CHARS)
}

// ─── Per-tool renderers ────────────────────────────────────────────────────

fn render_shell(input: &Value, full: &str, is_error: bool) -> String {
    let cmd = input["command"].as_str().unwrap_or("");
    let cmd_short = truncate(cmd.trim(), 60);
    let exit = parse_exit_code(full);
    let out_chars = full.chars().count();
    let status = match (is_error, exit) {
        (true, Some(c)) => format!("ERR exit={}", c),
        (true, None) => "ERR".to_string(),
        (false, Some(c)) => format!("exit={}", c),
        (false, None) => "ok".to_string(),
    };
    let base = format!("ran: {}; {}; out={} chars", cmd_short, status, out_chars);
    if is_error {
        append_error_hint(&base, full)
    } else {
        base
    }
}

fn render_query(tool_name: &str, input: &Value, full: &str, is_error: bool) -> String {
    let q = input["query"]
        .as_str()
        .or_else(|| input["command"].as_str())
        .unwrap_or("");
    let q_short = truncate(q.trim(), 60);
    let out_chars = full.chars().count();
    let base = format!(
        "{}: {}; {}; out={} chars",
        tool_name,
        q_short,
        if is_error { "ERR" } else { "ok" },
        out_chars
    );
    if is_error {
        append_error_hint(&base, full)
    } else {
        base
    }
}

fn render_file_read(input: &Value, full: &str, is_error: bool) -> String {
    let path = input["path"].as_str().unwrap_or("");
    let path_tail = tail_path(path, 50);
    if is_error {
        return append_error_hint(&format!("read file FAIL: {}", path_tail), full);
    }
    let lines = full.lines().count();
    let chars = full.chars().count();
    format!("read file: {} ({} lines, {} chars)", path_tail, lines, chars)
}

fn render_file_write(input: &Value, full: &str, is_error: bool) -> String {
    let path = input["path"].as_str().unwrap_or("");
    let path_tail = tail_path(path, 50);
    let content_chars = input["content"]
        .as_str()
        .map(|s| s.chars().count())
        .unwrap_or(0);
    if is_error {
        return append_error_hint(&format!("write file FAIL: {}", path_tail), full);
    }
    format!("wrote file: {} ({} chars)", path_tail, content_chars)
}

fn render_file_edit(tool_name: &str, input: &Value, full: &str, is_error: bool) -> String {
    let path = input["path"].as_str().unwrap_or("");
    let path_tail = tail_path(path, 45);
    if is_error {
        return append_error_hint(&format!("{} FAIL: {}", tool_name, path_tail), full);
    }
    let (plus, minus) = parse_diff_counts(full);
    match (plus, minus) {
        (Some(p), Some(m)) => format!("edited {}: +{}/-{}", path_tail, p, m),
        _ => format!("edited {}", path_tail),
    }
}

fn render_file_listing(tool_name: &str, input: &Value, full: &str, is_error: bool) -> String {
    let target = input["path"]
        .as_str()
        .or_else(|| input["query"].as_str())
        .or_else(|| input["pattern"].as_str())
        .unwrap_or("");
    let target_short = truncate(target, 50);
    if is_error {
        return append_error_hint(&format!("{} FAIL: {}", tool_name, target_short), full);
    }
    let entries = full.lines().filter(|l| !l.trim().is_empty()).count();
    format!("{}: {} ({} entries)", tool_name, target_short, entries)
}

fn render_web_search(input: &Value, full: &str, is_error: bool) -> String {
    let q = input["query"].as_str().unwrap_or("");
    let q_short = truncate(q.trim(), 60);
    if is_error {
        return append_error_hint(&format!("searched FAIL: {}", q_short), full);
    }
    let results = count_search_results(full);
    format!("searched: {}; {} results", q_short, results)
}

fn render_browser(input: &Value, full: &str, is_error: bool) -> String {
    let action = input["action"].as_str().unwrap_or("?");
    let target = input["url"]
        .as_str()
        .or_else(|| input["selector"].as_str())
        .unwrap_or("");
    let target_short = truncate(target, 55);
    if is_error {
        return append_error_hint(&format!("browser {} FAIL: {}", action, target_short), full);
    }
    let chars = full.chars().count();
    format!("browser {} {}: {} chars", action, target_short, chars)
}

fn render_plan(input: &Value, _full: &str) -> String {
    let n = input["items"]
        .as_array()
        .map(|a| a.len())
        .or_else(|| input["todos"].as_array().map(|a| a.len()))
        .unwrap_or(0);
    format!("planned {} todos", n)
}

fn render_memory(tool_name: &str, input: &Value, full: &str, is_error: bool) -> String {
    if is_error {
        return append_error_hint(&format!("{} FAIL", tool_name), full);
    }
    let hint = input["key"]
        .as_str()
        .or_else(|| input["query"].as_str())
        .or_else(|| input["id"].as_str())
        .unwrap_or("");
    if hint.is_empty() {
        format!("{}: {} chars", tool_name, full.chars().count())
    } else {
        format!("{}: {}", tool_name, truncate(hint, 60))
    }
}

fn render_skill_list(full: &str) -> String {
    let entries = full.lines().filter(|l| !l.trim().is_empty()).count();
    format!("skill_list: {} entries", entries)
}

fn render_vision(input: &Value, full: &str) -> String {
    let action = input["action"].as_str().unwrap_or("list");
    let count = input["ids"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or_else(|| full.lines().filter(|l| l.trim().starts_with("- ")).count());
    format!("vision_context {}: {}", action, count)
}

fn render_chat_ui(is_error: bool) -> String {
    if is_error {
        "chat_ui FAIL".to_string()
    } else {
        "opened chat_ui form (awaits user response)".to_string()
    }
}

fn render_screen_capture(input: &Value, _full: &str, is_error: bool) -> String {
    let mode = input["mode"].as_str().unwrap_or("fullscreen");
    if is_error {
        format!("screen_capture FAIL ({})", mode)
    } else {
        format!("screen_capture {} → image", mode)
    }
}

fn render_subagent(tool_name: &str, input: &Value, full: &str, is_error: bool) -> String {
    let target = input["fish_id"]
        .as_str()
        .or_else(|| input["koi_id"].as_str())
        .or_else(|| input["name"].as_str())
        .or_else(|| input["target"].as_str())
        .unwrap_or("");
    let head = first_non_empty_line(full);
    let head_short = truncate(&head, 80);
    if is_error {
        return append_error_hint(
            &format!("{} {} FAIL", tool_name, truncate(target, 30)),
            full,
        );
    }
    format!(
        "{} {}: {}",
        tool_name,
        truncate(target, 30),
        head_short
    )
}

fn render_external(tool_name: &str, full: &str, is_error: bool) -> String {
    let chars = full.chars().count();
    let base = format!(
        "invoked {}: {} chars",
        truncate(tool_name, 60),
        chars
    );
    if is_error {
        append_error_hint(&base, full)
    } else {
        base
    }
}

fn render_fallback(tool_name: &str, full: &str, is_error: bool) -> String {
    let base = format!(
        "called {}: {} chars{}",
        truncate(tool_name, 50),
        full.chars().count(),
        if is_error { " (ERR)" } else { "" }
    );
    if is_error {
        append_error_hint(&base, full)
    } else {
        base
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────────

/// Truncate to at most `n` characters, appending an ellipsis when clipped.
fn truncate(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= n {
        return s.to_string();
    }
    let keep = n.saturating_sub(1).max(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

/// Keep the last `n` characters of a path-like string with a leading ellipsis.
fn tail_path(path: &str, n: usize) -> String {
    let count = path.chars().count();
    if count <= n {
        return path.to_string();
    }
    let keep = n.saturating_sub(3);
    let start = count - keep;
    let tail: String = path.chars().skip(start).collect();
    format!("...{}", tail)
}

/// Scan `full` for an exit-code marker emitted by our shell runner.
/// Recognised forms: `exit code: N`, `exit=N`, `ExitCode: N`.
fn parse_exit_code(full: &str) -> Option<i32> {
    for line in full.lines().rev().take(10) {
        let l = line.trim();
        for pat in ["exit code:", "exitcode:", "exit=", "ExitCode:"] {
            if let Some(idx) = l.to_ascii_lowercase().find(&pat.to_ascii_lowercase()) {
                let rest = &l[idx + pat.len()..];
                let n: String = rest
                    .trim_start_matches(|c: char| c.is_whitespace() || c == '=')
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '-')
                    .collect();
                if let Ok(parsed) = n.parse::<i32>() {
                    return Some(parsed);
                }
            }
        }
    }
    None
}

/// Parse a unified-diff-ish output for added/removed line counts.
/// Looks for lines starting with `+` (not `+++`) and `-` (not `---`).
fn parse_diff_counts(full: &str) -> (Option<usize>, Option<usize>) {
    let mut plus = 0usize;
    let mut minus = 0usize;
    let mut seen_any = false;
    for line in full.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            if !rest.is_empty() || line == "+" {
                plus += 1;
                seen_any = true;
            }
        } else if let Some(rest) = line.strip_prefix('-') {
            if !rest.is_empty() || line == "-" {
                minus += 1;
                seen_any = true;
            }
        }
    }
    if seen_any {
        (Some(plus), Some(minus))
    } else {
        (None, None)
    }
}

/// Count result entries in a web-search result. The existing web_search tool
/// renders each result as a block with a blank line between entries; we count
/// headings or double newlines as a conservative proxy.
fn count_search_results(full: &str) -> usize {
    // Prefer counting markdown-style headings `###` or numbered-list entries.
    let heading_count = full.lines().filter(|l| l.trim_start().starts_with("###")).count();
    if heading_count > 0 {
        return heading_count;
    }
    let list_count = full
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            (t.starts_with(char::is_numeric) && t.contains(". "))
                || t.starts_with("- ")
                || t.starts_with("* ")
        })
        .count();
    if list_count > 0 {
        return list_count;
    }
    // Fallback: count blank-separated blocks.
    full.split("\n\n").filter(|b| !b.trim().is_empty()).count()
}

/// Parse a `[vision_artifact] id=<id>` marker out of a full tool-result body.
///
/// `execute_single_tool` appends this marker whenever the tool attached an
/// image; extracting it here means callers can regenerate a receipt from the
/// persisted full content without having to track the artifact id separately.
fn extract_vision_artifact_id(full: &str) -> Option<String> {
    for line in full.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("[vision_artifact]") {
            let rest = rest.trim_start();
            if let Some(after_id) = rest.strip_prefix("id=") {
                let id: String = after_id
                    .chars()
                    .take_while(|c| !c.is_whitespace())
                    .collect();
                if !id.is_empty() {
                    return Some(id);
                }
            }
        }
    }
    None
}

fn first_non_empty_line(full: &str) -> String {
    full.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// Append the first non-empty error-looking line from `full` onto `base`,
/// kept to a bounded length so the overall receipt stays under RECEIPT_MAX_CHARS.
fn append_error_hint(base: &str, full: &str) -> String {
    // Prefer a line that starts with "Error" / "ERR" / "Exception" / "panic" /
    // non-zero exit message — fall back to the first non-empty line.
    let hint = full
        .lines()
        .map(str::trim)
        .find(|l| {
            let lower = l.to_ascii_lowercase();
            !l.is_empty()
                && (lower.starts_with("error")
                    || lower.starts_with("err ")
                    || lower.starts_with("err:")
                    || lower.starts_with("exception")
                    || lower.starts_with("panic")
                    || lower.starts_with("fatal")
                    || lower.starts_with("failed"))
        })
        .or_else(|| full.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("");
    if hint.is_empty() {
        return base.to_string();
    }
    let available = RECEIPT_MAX_CHARS
        .saturating_sub(base.chars().count())
        .saturating_sub(6); // " ERR: " prefix
    if available < 10 {
        return base.to_string();
    }
    format!("{} ERR: {}", base, truncate(hint, available))
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truncate_respects_char_boundaries_and_ellipsis() {
        assert_eq!(truncate("abcdef", 10), "abcdef");
        let clipped = truncate("abcdef", 4);
        assert_eq!(clipped.chars().count(), 4);
        assert!(clipped.ends_with('…'));
        // CJK: 6 chars, not 18 bytes.
        assert_eq!(truncate("你好世界啊哈", 10), "你好世界啊哈");
        assert!(truncate("你好世界啊哈", 3).chars().count() == 3);
    }

    #[test]
    fn tail_path_keeps_suffix() {
        assert_eq!(tail_path("/a/b/c.txt", 100), "/a/b/c.txt");
        let t = tail_path("C:/very/long/path/to/some/file.rs", 20);
        assert!(t.starts_with("..."));
        assert!(t.ends_with("file.rs"));
        assert_eq!(t.chars().count(), 20);
    }

    #[test]
    fn shell_success_receipt() {
        let r = render_receipt(
            "shell",
            &json!({"command": "ls -la"}),
            "total 0\nexit code: 0",
            false,
            None,
        );
        assert!(r.starts_with("ran: ls -la;"));
        assert!(r.contains("exit=0"));
        assert!(r.contains("out="));
    }

    #[test]
    fn shell_error_receipt_includes_hint() {
        let r = render_receipt(
            "shell",
            &json!({"command": "bad-cmd"}),
            "Error: command not found: bad-cmd\nexit code: 127",
            true,
            None,
        );
        assert!(r.contains("ERR"));
        assert!(r.contains("exit=127"));
        assert!(r.to_lowercase().contains("command not found"));
        assert!(r.chars().count() <= RECEIPT_MAX_CHARS);
    }

    #[test]
    fn file_read_receipt_counts_lines() {
        let r = render_receipt(
            "file_read",
            &json!({"path": "src/main.rs"}),
            "fn main() {\n    println!(\"hi\");\n}\n",
            false,
            None,
        );
        assert!(r.starts_with("read file: "));
        assert!(r.contains("src/main.rs"));
        assert!(r.contains("lines"));
    }

    #[test]
    fn file_write_receipt_counts_chars_from_input() {
        let r = render_receipt(
            "file_write",
            &json!({"path": "/tmp/x.md", "content": "hello world"}),
            "ok",
            false,
            None,
        );
        assert!(r.starts_with("wrote file: "));
        assert!(r.contains("11 chars"));
    }

    #[test]
    fn file_edit_parses_diff() {
        let r = render_receipt(
            "file_edit",
            &json!({"path": "a/b.rs"}),
            "--- old\n+++ new\n+added line\n-removed line\n+another",
            false,
            None,
        );
        assert!(r.contains("+2"));
        assert!(r.contains("-1"));
    }

    #[test]
    fn web_search_counts_headings() {
        let r = render_receipt(
            "web_search",
            &json!({"query": "rust"}),
            "### Result 1\nbody\n\n### Result 2\nbody\n\n### Result 3\nbody",
            false,
            None,
        );
        assert!(r.contains("3 results"));
    }

    #[test]
    fn fallback_used_for_unknown_tool() {
        let r = render_receipt(
            "weird_unknown_tool",
            &json!({}),
            "some output\n",
            false,
            None,
        );
        assert!(r.starts_with("called weird_unknown_tool: "));
    }

    #[test]
    fn image_artifact_appended() {
        let r = render_receipt(
            "screen_capture",
            &json!({"mode": "fullscreen"}),
            "",
            false,
            Some("img_abcdef1234567890"),
        );
        assert!(r.contains("+image(id="));
    }

    #[test]
    fn receipt_is_bounded() {
        let long: String = std::iter::repeat('x').take(10_000).collect();
        let r = render_receipt(
            "shell",
            &json!({"command": long.clone()}),
            &long,
            true,
            None,
        );
        assert!(r.chars().count() <= RECEIPT_MAX_CHARS);
    }

    #[test]
    fn count_search_results_numbered_list() {
        let s = "1. First\n2. Second\n3. Third";
        assert_eq!(count_search_results(s), 3);
    }

    #[test]
    fn extracts_vision_artifact_id_from_marker() {
        let body = "ok\n\n[vision_artifact] id=img_xyz label=\"...\" media_type=image/png";
        let r = render_receipt("screen_capture", &json!({"mode":"fullscreen"}), body, false, None);
        assert!(r.contains("+image(id=img_xyz"));
    }

    #[test]
    fn parse_exit_code_variants() {
        assert_eq!(parse_exit_code("exit code: 0"), Some(0));
        assert_eq!(parse_exit_code("ExitCode: 127"), Some(127));
        assert_eq!(parse_exit_code("foo bar"), None);
    }
}
