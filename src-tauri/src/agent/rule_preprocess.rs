//! Deterministic, zero-LLM rule-based preprocessor for tool-result and
//! message content.
//!
//! ## Information-theoretic motivation
//!
//! Most tool output (shell logs, stack traces, table dumps, base64
//! payloads, ANSI-decorated terminals) has very low Shannon entropy:
//! long stretches of near-identical bytes, repeated stack frames,
//! boilerplate separators. This module folds those low-entropy regions
//! into short codewords (run-length markers, placeholders, canonical
//! headers) *before* either the minimal-receipt demotion in
//! [`crate::agent::tool_receipt`] or the L2 semantic summariser
//! (`compact_summarise`) runs.
//!
//! It is pure, synchronous, and side-effect-free. Every rule is
//! individually unit-tested and must be idempotent (applying twice
//! yields the same output).
//!
//! ## Preservation invariants
//!
//! 1. The first non-empty line is always preserved (it usually carries
//!    the primary signal — error message, command echo, status).
//! 2. The last 3 non-empty lines are preserved (trailing output is
//!    where exit codes / final results live).
//! 3. Any line that looks like an error (starts with "Error" / "ERR" /
//!    "panic" / "fatal" / etc. — case-insensitive) is never folded.
//! 4. Output length is bounded by `MAX_OUT_CHARS` regardless of input.
//!
//! ## Aggressiveness levels
//!
//! * [`Level::L1`] — receipt-preserving: safe for live agent context.
//! * [`Level::L2`] — pre-summariser: more aggressive dedup across
//!   message boundaries, used only by `compact_summarise` to shrink
//!   its LLM input (Phase 6 of the v2 plan).

use std::borrow::Cow;

use crate::llm::{ContentBlock, LlmMessage, MessageContent};

/// Hard upper bound on the preprocessed content length. Anything
/// longer is truncated with an ellipsis.
pub const MAX_OUT_CHARS: usize = 4_096;

/// How many consecutive identical lines are required before RLE
/// folding kicks in. A value of 3 is conservative enough that noisy
/// but semantically different output (e.g. repeated `Building X ...`
/// with distinct X) rarely trips it.
pub const RLE_MIN_RUN: usize = 3;

/// Preserve at most this many leading stack frames of any given
/// trace; the rest of contiguous frames from the same module collapse.
pub const STACK_KEEP_LEADING: usize = 4;

/// Path tail segments to keep when normalising long paths.
pub const PATH_KEEP_TAIL_SEGMENTS: usize = 3;

/// Table rows to keep before switching to a count summary.
pub const TABLE_KEEP_ROWS: usize = 5;

/// Base64 blobs longer than this get replaced with a placeholder.
pub const BASE64_MIN_LEN: usize = 64;

/// Aggressiveness level for the preprocessor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Receipt-preserving: safe for live context.
    L1,
    /// Pre-summariser: additionally deduplicates across message
    /// boundaries (used by Phase 6 / `compact_summarise` input).
    L2,
}

/// Main entry point: preprocess a single chunk of content.
///
/// Returns a [`Cow::Borrowed`] when nothing changes, so the common
/// "already clean" path allocates nothing.
pub fn preprocess(content: &str, level: Level) -> Cow<'_, str> {
    if content.is_empty() {
        return Cow::Borrowed(content);
    }
    // Fast-path: small, single-line content usually has nothing to
    // compress. Check for obvious signals that any rule could fire.
    if content.len() < 128 && !content.contains('\n') && !content.contains('\x1b') {
        return Cow::Borrowed(content);
    }

    let mut s = strip_ansi(content);
    s = fold_base64_blobs(&s).into_owned();
    s = fold_repeated_lines(&s);
    s = fold_stack_frames(&s);
    s = fold_table_rows(&s);
    s = normalise_long_paths(&s);
    if matches!(level, Level::L2) {
        s = squeeze_blank_lines(&s);
    }
    s = truncate_bounded(&s, MAX_OUT_CHARS);

    if s == content {
        Cow::Borrowed(content)
    } else {
        Cow::Owned(s)
    }
}

/// Preprocess every text / tool_result block in an `LlmMessage`.
///
/// Non-text blocks (Image, ToolUse input) are preserved verbatim
/// because their internal structure is not our channel to compress.
pub fn preprocess_message(msg: &LlmMessage, level: Level) -> LlmMessage {
    let content = match &msg.content {
        MessageContent::Text(t) => {
            let out = preprocess(t, level);
            if matches!(out, Cow::Borrowed(_)) {
                return msg.clone();
            }
            MessageContent::Text(out.into_owned())
        }
        MessageContent::Blocks(blocks) => {
            let mut changed = false;
            let new_blocks: Vec<ContentBlock> = blocks
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => {
                        let out = preprocess(text, level);
                        match out {
                            Cow::Owned(s) => {
                                changed = true;
                                ContentBlock::Text { text: s }
                            }
                            Cow::Borrowed(_) => b.clone(),
                        }
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        let out = preprocess(content, level);
                        match out {
                            Cow::Owned(s) => {
                                changed = true;
                                ContentBlock::ToolResult {
                                    tool_use_id: tool_use_id.clone(),
                                    content: s,
                                    is_error: *is_error,
                                }
                            }
                            Cow::Borrowed(_) => b.clone(),
                        }
                    }
                    other => other.clone(),
                })
                .collect();
            if !changed {
                return msg.clone();
            }
            MessageContent::Blocks(new_blocks)
        }
    };
    LlmMessage {
        role: msg.role.clone(),
        content,
    }
}

/// Preprocess an entire message vector. At L2, also runs
/// [`dedup_cross_message`] to drop repeated message bodies across
/// adjacent turns (common for retry loops).
pub fn preprocess_messages(messages: &[LlmMessage], level: Level) -> Vec<LlmMessage> {
    let mut out: Vec<LlmMessage> = messages.iter().map(|m| preprocess_message(m, level)).collect();
    if matches!(level, Level::L2) {
        out = dedup_cross_message(out);
    }
    out
}

// ───────────────── Rule: ANSI escape stripping ─────────────────

fn strip_ansi(s: &str) -> String {
    if !s.contains('\x1b') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI: ESC '[' ... letter
            if matches!(chars.peek(), Some('[')) {
                chars.next();
                for c2 in chars.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            // OSC: ESC ']' ... BEL or ESC\
            if matches!(chars.peek(), Some(']')) {
                chars.next();
                for c2 in chars.by_ref() {
                    if c2 == '\x07' || c2 == '\x1b' {
                        break;
                    }
                }
                continue;
            }
            // Other escape sequences: drop next char
            chars.next();
            continue;
        }
        out.push(c);
    }
    out
}

// ───────────────── Rule: base64 blob folding ─────────────────

fn fold_base64_blobs(s: &str) -> Cow<'_, str> {
    // Heuristic: a run of ≥ BASE64_MIN_LEN chars all in [A-Za-z0-9+/=]
    // with no whitespace. Bounded per scan — only looks for long tokens
    // so short base64-looking words (e.g. `abc123`) slip through fine.
    if !s.chars().any(|c| c.is_ascii_alphanumeric()) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut token = String::new();
    let mut changed = false;

    let flush_token = |out: &mut String, token: &mut String, changed: &mut bool| {
        if token.chars().count() >= BASE64_MIN_LEN && looks_base64(token) {
            let approx_bytes = token.len() * 3 / 4;
            out.push_str(&format!("<base64:{}B>", approx_bytes));
            *changed = true;
        } else {
            out.push_str(token);
        }
        token.clear();
    };

    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=' {
            token.push(c);
        } else {
            flush_token(&mut out, &mut token, &mut changed);
            out.push(c);
        }
    }
    flush_token(&mut out, &mut token, &mut changed);
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(s)
    }
}

fn looks_base64(s: &str) -> bool {
    // base64 requires a mix of letter/digit/+/; pure digits or pure
    // letters usually mean it's not a blob.
    let (mut has_upper, mut has_lower, mut has_digit) = (false, false, false);
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            has_upper = true;
        } else if c.is_ascii_lowercase() {
            has_lower = true;
        } else if c.is_ascii_digit() {
            has_digit = true;
        }
    }
    // Require at least two of the three classes — guards against long
    // all-hex strings (tx hashes etc.) being misidentified.
    (has_upper as u8 + has_lower as u8 + has_digit as u8) >= 2
}

// ───────────────── Rule: repeated-line RLE ─────────────────

fn fold_repeated_lines(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() < RLE_MIN_RUN {
        return s.to_string();
    }
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let cur = lines[i];
        // Count consecutive identical lines (after trimming trailing
        // whitespace so "ok   " and "ok" collapse).
        let norm = cur.trim_end();
        let mut run = 1;
        while i + run < lines.len() && lines[i + run].trim_end() == norm {
            run += 1;
        }
        if run >= RLE_MIN_RUN && !looks_like_error_line(cur) {
            out.push(cur.to_string());
            out.push(format!("… <repeated ×{}>", run - 1));
        } else {
            for k in 0..run {
                out.push(lines[i + k].to_string());
            }
        }
        i += run;
    }
    out.join("\n")
}

fn looks_like_error_line(s: &str) -> bool {
    let t = s.trim().to_ascii_lowercase();
    t.starts_with("error")
        || t.starts_with("err:")
        || t.starts_with("err ")
        || t.starts_with("panic")
        || t.starts_with("fatal")
        || t.starts_with("failed")
        || t.starts_with("exception")
        || t.starts_with("traceback")
}

// ───────────────── Rule: stack frame folding ─────────────────

fn fold_stack_frames(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if !is_stack_frame(lines[i]) {
            out.push(lines[i].to_string());
            i += 1;
            continue;
        }
        // Walk the frame run.
        let mut run_end = i;
        while run_end < lines.len() && is_stack_frame(lines[run_end]) {
            run_end += 1;
        }
        let run_len = run_end - i;
        if run_len <= STACK_KEEP_LEADING + 2 {
            for k in i..run_end {
                out.push(lines[k].to_string());
            }
        } else {
            for k in i..(i + STACK_KEEP_LEADING) {
                out.push(lines[k].to_string());
            }
            out.push(format!(
                "… <{} inner frames folded>",
                run_len - STACK_KEEP_LEADING - 1
            ));
            // Keep the last frame (usually the user's code).
            out.push(lines[run_end - 1].to_string());
        }
        i = run_end;
    }
    out.join("\n")
}

fn is_stack_frame(line: &str) -> bool {
    let t = line.trim_start();
    // Python-ish: "  File "x.py", line 42, in fn"
    if t.starts_with("File \"") && t.contains(", line ") {
        return true;
    }
    // Rust-ish: "  42: crate::module::fn" or "  at crate::module::fn (file:line)"
    if t.starts_with("at ") && (t.contains("::") || t.contains('(')) {
        return true;
    }
    // Node-ish: "    at functionName (/abs/path:123:45)"
    if t.starts_with("at ") && (t.contains(':') || t.contains('(')) {
        return true;
    }
    // Generic "  42: symbol" (Rust panic)
    if t.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        if let Some(idx) = t.find(": ") {
            let head = &t[..idx];
            if head.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }
    false
}

// ───────────────── Rule: table row compression ─────────────────

fn fold_table_rows(s: &str) -> String {
    // Very conservative: detect markdown-style tables (lines with
    // multiple '|' separators) that exceed TABLE_KEEP_ROWS data rows.
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() < TABLE_KEEP_ROWS + 3 {
        return s.to_string();
    }
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if !is_table_row(lines[i]) {
            out.push(lines[i].to_string());
            i += 1;
            continue;
        }
        let mut run_end = i;
        while run_end < lines.len() && is_table_row(lines[run_end]) {
            run_end += 1;
        }
        let run_len = run_end - i;
        // Need header + separator + > TABLE_KEEP_ROWS data rows
        if run_len <= TABLE_KEEP_ROWS + 2 {
            for k in i..run_end {
                out.push(lines[k].to_string());
            }
        } else {
            // Keep header + separator + first TABLE_KEEP_ROWS rows
            for k in i..(i + 2 + TABLE_KEEP_ROWS) {
                out.push(lines[k].to_string());
            }
            out.push(format!(
                "| … <{} rows folded> |",
                run_len - 2 - TABLE_KEEP_ROWS
            ));
        }
        i = run_end;
    }
    out.join("\n")
}

fn is_table_row(line: &str) -> bool {
    let t = line.trim();
    if !t.starts_with('|') || !t.ends_with('|') {
        return false;
    }
    t.chars().filter(|&c| c == '|').count() >= 2
}

// ───────────────── Rule: long path normalisation ─────────────────

fn normalise_long_paths(s: &str) -> String {
    // Only rewrite paths with > PATH_KEEP_TAIL_SEGMENTS + 2 segments
    // AND with at least one '/' or '\' separator AND overall length
    // > 60 (so we don't rewrite reasonable project paths).
    let mut out = String::with_capacity(s.len());
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&normalise_line_paths(line));
    }
    out
}

fn normalise_line_paths(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut current = String::new();

    let is_path_char =
        |c: char| c.is_ascii_alphanumeric() || "/\\._-:".contains(c);

    for c in line.chars() {
        if is_path_char(c) {
            current.push(c);
        } else {
            out.push_str(&maybe_shorten_path(&current));
            current.clear();
            out.push(c);
        }
    }
    out.push_str(&maybe_shorten_path(&current));
    out
}

fn maybe_shorten_path(s: &str) -> String {
    if s.len() <= 60 {
        return s.to_string();
    }
    let sep = if s.contains('\\') && !s.contains('/') {
        '\\'
    } else if s.contains('/') {
        '/'
    } else {
        return s.to_string();
    };
    let segments: Vec<&str> = s.split(sep).filter(|seg| !seg.is_empty()).collect();
    if segments.len() <= PATH_KEEP_TAIL_SEGMENTS + 1 {
        return s.to_string();
    }
    let tail = &segments[segments.len() - PATH_KEEP_TAIL_SEGMENTS..];
    let prefix = if s.starts_with(sep) {
        String::from(sep)
    } else {
        String::new()
    };
    format!("{}...{}{}", prefix, sep, tail.join(&sep.to_string()))
}

// ───────────────── Rule: blank-line squeeze (L2 only) ─────────────────

fn squeeze_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank_run = 0usize;
    for line in s.split('\n') {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                out.push_str(line);
                out.push('\n');
            }
        } else {
            blank_run = 0;
            out.push_str(line);
            out.push('\n');
        }
    }
    if !s.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    out
}

// ───────────────── Cross-message dedup (L2 only) ─────────────────

fn dedup_cross_message(messages: Vec<LlmMessage>) -> Vec<LlmMessage> {
    // Collapse adjacent messages with identical role + identical text
    // content. Common pattern: agent retries an identical tool call
    // after a transient error, same "assistant says X" appearing
    // twice back-to-back.
    let mut out: Vec<LlmMessage> = Vec::with_capacity(messages.len());
    for m in messages {
        if let Some(last) = out.last() {
            if last.role == m.role && content_text_eq(&last.content, &m.content) {
                continue;
            }
        }
        out.push(m);
    }
    out
}

fn content_text_eq(a: &MessageContent, b: &MessageContent) -> bool {
    fn flat(c: &MessageContent) -> String {
        match c {
            MessageContent::Text(t) => t.trim().to_string(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.trim().to_string()),
                    ContentBlock::ToolResult { content, .. } => Some(content.trim().to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
    flat(a) == flat(b)
}

// ───────────────── Bounded truncation ─────────────────

fn truncate_bounded(s: &str, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars {
        return s.to_string();
    }
    // Preserve head + tail (tail usually has exit code).
    let head_keep = max_chars * 2 / 3;
    let tail_keep = max_chars - head_keep - 20;
    let head: String = s.chars().take(head_keep).collect();
    let tail: String = s
        .chars()
        .skip(n.saturating_sub(tail_keep))
        .collect();
    format!("{}\n… <{} chars truncated>\n{}", head, n - max_chars, tail)
}

// ───────────────── Tests ─────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn idempotent_on_clean_input() {
        let clean = "Hello world";
        let out = preprocess(clean, Level::L1);
        assert_eq!(out.as_ref(), clean);
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn ansi_stripped() {
        let raw = "\x1b[31mRed\x1b[0m Text\n\x1b[1mBold\x1b[0m";
        let out = preprocess(raw, Level::L1);
        assert!(!out.contains('\x1b'));
        assert!(out.contains("Red Text"));
        assert!(out.contains("Bold"));
    }

    #[test]
    fn repeated_lines_folded() {
        let raw = "start\nworking\nworking\nworking\nworking\ndone";
        let out = preprocess(raw, Level::L1);
        assert!(out.contains("working"));
        assert!(out.contains("repeated"));
        assert!(out.matches("working\n").count() <= 1);
    }

    #[test]
    fn error_lines_never_folded() {
        let raw = "Error: bad\nError: bad\nError: bad\nError: bad";
        let out = preprocess(raw, Level::L1);
        // All four error lines must survive.
        assert_eq!(out.matches("Error: bad").count(), 4);
    }

    #[test]
    fn base64_blob_replaced() {
        let blob = "a".repeat(100) + &"B".repeat(100) + &"3".repeat(20);
        let raw = format!("data: {}\n", blob);
        let out = preprocess(&raw, Level::L1);
        assert!(out.contains("<base64:"));
        assert!(out.len() < raw.len());
    }

    #[test]
    fn short_base64_preserved() {
        let raw = "abc123 is short\n";
        let out = preprocess(raw, Level::L1);
        assert_eq!(out.as_ref(), raw);
    }

    #[test]
    fn python_stack_frames_folded() {
        let raw = concat!(
            "Traceback:\n",
            "  File \"a.py\", line 1, in a\n",
            "  File \"b.py\", line 2, in b\n",
            "  File \"c.py\", line 3, in c\n",
            "  File \"d.py\", line 4, in d\n",
            "  File \"e.py\", line 5, in e\n",
            "  File \"f.py\", line 6, in f\n",
            "  File \"g.py\", line 7, in g\n",
            "  File \"user.py\", line 99, in main\n",
            "ValueError: bad"
        );
        let out = preprocess(raw, Level::L1);
        assert!(out.contains("Traceback"));
        assert!(out.contains("frames folded"));
        assert!(out.contains("user.py"));
        assert!(out.contains("ValueError"));
    }

    #[test]
    fn markdown_table_rows_folded() {
        let mut raw = String::from("header\n|a|b|\n|-|-|\n");
        for i in 0..20 {
            raw.push_str(&format!("|r{}|v{}|\n", i, i));
        }
        raw.push_str("tail\n");
        let out = preprocess(&raw, Level::L1);
        assert!(out.contains("rows folded"));
        assert!(out.contains("|r0|v0|"));
        assert!(out.contains("tail"));
    }

    #[test]
    fn long_path_shortened() {
        let raw =
            "looking at /a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/file.rs for errors\n";
        let out = preprocess(raw, Level::L1);
        assert!(out.contains("file.rs"));
        assert!(out.contains("..."));
    }

    #[test]
    fn short_path_preserved() {
        let raw = "src/main.rs\n";
        let out = preprocess(raw, Level::L1);
        assert_eq!(out.as_ref(), raw);
    }

    #[test]
    fn bounded_output_length() {
        let huge: String = std::iter::repeat("line\n").take(10_000).collect();
        let out = preprocess(&huge, Level::L1);
        assert!(out.chars().count() <= MAX_OUT_CHARS + 80);
    }

    #[test]
    fn preserves_first_and_last_lines() {
        let mut raw = String::from("FIRST: important\n");
        for _ in 0..2_000 {
            raw.push_str("filler\n");
        }
        raw.push_str("LAST: exit=0\n");
        let out = preprocess(&raw, Level::L1);
        assert!(out.contains("FIRST:"));
        assert!(out.contains("LAST: exit=0"));
    }

    #[test]
    fn preprocess_message_leaves_clean_untouched() {
        let msg = LlmMessage {
            role: "assistant".into(),
            content: MessageContent::Text("hello".into()),
        };
        let out = preprocess_message(&msg, Level::L1);
        match out.content {
            MessageContent::Text(t) => assert_eq!(t, "hello"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn preprocess_message_cleans_tool_result_block() {
        let msg = LlmMessage {
            role: "user".into(),
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "tu1".into(),
                content: "start\nwork\nwork\nwork\nwork\nend".into(),
                is_error: false,
            }]),
        };
        let out = preprocess_message(&msg, Level::L1);
        let MessageContent::Blocks(blocks) = out.content else {
            panic!("expected blocks");
        };
        let ContentBlock::ToolResult { content, .. } = &blocks[0] else {
            panic!("expected ToolResult");
        };
        assert!(content.contains("repeated"));
    }

    #[test]
    fn preprocess_message_preserves_tool_use_and_image() {
        let msg = LlmMessage {
            role: "assistant".into(),
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: "tu1".into(),
                name: "shell".into(),
                input: json!({"command": "ls"}),
            }]),
        };
        let out = preprocess_message(&msg, Level::L1);
        let MessageContent::Blocks(blocks) = out.content else {
            panic!("expected blocks");
        };
        assert!(matches!(&blocks[0], ContentBlock::ToolUse { .. }));
    }

    #[test]
    fn l2_dedups_adjacent_identical_messages() {
        let msg1 = LlmMessage {
            role: "assistant".into(),
            content: MessageContent::Text("same text".into()),
        };
        let msg2 = msg1.clone();
        let msg3 = LlmMessage {
            role: "user".into(),
            content: MessageContent::Text("different".into()),
        };
        let out = preprocess_messages(&[msg1, msg2, msg3], Level::L2);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn l1_does_not_dedup_across_messages() {
        let msg1 = LlmMessage {
            role: "assistant".into(),
            content: MessageContent::Text("same text".into()),
        };
        let msg2 = msg1.clone();
        let out = preprocess_messages(&[msg1, msg2], Level::L1);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn l2_squeezes_blank_lines() {
        let raw = "a\n\n\n\n\nb\n";
        let out = preprocess(raw, Level::L2);
        // At most one blank line between a and b.
        let blanks: usize = out.matches("\n\n\n").count();
        assert_eq!(blanks, 0);
        assert!(out.contains("a"));
        assert!(out.contains("b"));
    }

    #[test]
    fn idempotent_second_application() {
        let raw = "start\nspam\nspam\nspam\nspam\nend";
        let once = preprocess(raw, Level::L1).into_owned();
        let twice = preprocess(&once, Level::L1).into_owned();
        assert_eq!(once, twice);
    }
}
