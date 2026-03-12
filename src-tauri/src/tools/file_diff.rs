use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct FileDiffTool;

#[async_trait]
impl Tool for FileDiffTool {
    fn name(&self) -> &str {
        "file_diff"
    }

    fn description(&self) -> &str {
        "Preview changes before writing, or compare two files. Read-only — makes no modifications.\n\
         Two modes:\n\
         1. Preview mode: provide `path` + `new_content` — shows what file_write/file_edit would produce.\n\
         2. Compare mode: provide `path_a` + `path_b` — shows differences between two existing files.\n\
         Output is unified diff format (--- / +++ / @@ hunks). \
         Use this before file_edit on large files to verify your changes are correct."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Preview mode: path to the existing file to compare against new_content"
                },
                "new_content": {
                    "type": "string",
                    "description": "Preview mode: the proposed new content to diff against the current file"
                },
                "path_a": {
                    "type": "string",
                    "description": "Compare mode: first file (shown as ---)"
                },
                "path_b": {
                    "type": "string",
                    "description": "Compare mode: second file (shown as +++)"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Lines of context around each change (default 3)"
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let context_lines = input["context_lines"].as_u64().unwrap_or(3) as usize;

        // Determine mode
        let (label_a, content_a, label_b, content_b) =
            if input["path"].is_string() && input["new_content"].is_string() {
                // Preview mode
                let path_str = input["path"].as_str().unwrap();
                let path = if std::path::Path::new(path_str).is_absolute() {
                    std::path::PathBuf::from(path_str)
                } else {
                    ctx.workspace_root.join(path_str)
                };
                if !path.exists() {
                    return Ok(ToolResult::err(format!(
                        "File not found: {}",
                        path.display()
                    )));
                }
                let current = std::fs::read_to_string(&path)?;
                let proposed = input["new_content"].as_str().unwrap().to_string();
                (
                    format!("{} (current)", path.display()),
                    current,
                    format!("{} (proposed)", path.display()),
                    proposed,
                )
            } else if input["path_a"].is_string() && input["path_b"].is_string() {
                // Compare mode
                let resolve = |s: &str| -> std::path::PathBuf {
                    if std::path::Path::new(s).is_absolute() {
                        std::path::PathBuf::from(s)
                    } else {
                        ctx.workspace_root.join(s)
                    }
                };
                let pa = resolve(input["path_a"].as_str().unwrap());
                let pb = resolve(input["path_b"].as_str().unwrap());
                if !pa.exists() {
                    return Ok(ToolResult::err(format!(
                        "path_a not found: {}",
                        pa.display()
                    )));
                }
                if !pb.exists() {
                    return Ok(ToolResult::err(format!(
                        "path_b not found: {}",
                        pb.display()
                    )));
                }
                let ca = std::fs::read_to_string(&pa)?;
                let cb = std::fs::read_to_string(&pb)?;
                (pa.display().to_string(), ca, pb.display().to_string(), cb)
            } else {
                return Ok(ToolResult::err(
                    "Provide either (path + new_content) for preview mode, \
                     or (path_a + path_b) for compare mode",
                ));
            };

        let diff = unified_diff(&content_a, &content_b, &label_a, &label_b, context_lines);

        if diff.is_empty() {
            Ok(ToolResult::ok(
                "No differences — files are identical.".to_string(),
            ))
        } else {
            Ok(ToolResult::ok(diff))
        }
    }
}

// ---------------------------------------------------------------------------
// Minimal unified diff implementation (no external crates needed)
// ---------------------------------------------------------------------------

/// Compute a unified diff between `old` and `new` text.
/// Returns the diff as a string, or empty string if identical.
fn unified_diff(old: &str, new: &str, label_a: &str, label_b: &str, context: usize) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    if old_lines == new_lines {
        return String::new();
    }

    // Myers diff — compute edit script as a sequence of operations
    let ops = diff_lines(&old_lines, &new_lines);

    // Group ops into hunks (groups of changes with `context` lines of padding)
    let hunks = build_hunks(&ops, old_lines.len(), new_lines.len(), context);

    if hunks.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str(&format!("--- {}\n", label_a));
    out.push_str(&format!("+++ {}\n", label_b));

    for hunk in &hunks {
        // Count old/new lines in this hunk
        let old_count = hunk
            .iter()
            .filter(|op| matches!(op, DiffOp::Equal(_) | DiffOp::Delete(_)))
            .count();
        let new_count = hunk
            .iter()
            .filter(|op| matches!(op, DiffOp::Equal(_) | DiffOp::Insert(_)))
            .count();
        let old_start = hunk
            .iter()
            .find_map(|op| match op {
                DiffOp::Equal(i) | DiffOp::Delete(i) => Some(*i + 1),
                _ => None,
            })
            .unwrap_or(1);
        let new_start = hunk
            .iter()
            .find_map(|op| match op {
                DiffOp::Equal(i) | DiffOp::Insert(i) => Some(*i + 1),
                _ => None,
            })
            .unwrap_or(1);

        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            old_start, old_count, new_start, new_count
        ));

        for op in hunk {
            match op {
                DiffOp::Equal(i) => out.push_str(&format!(" {}\n", old_lines[*i])),
                DiffOp::Delete(i) => out.push_str(&format!("-{}\n", old_lines[*i])),
                DiffOp::Insert(i) => out.push_str(&format!("+{}\n", new_lines[*i])),
            }
        }
    }

    out
}

#[derive(Debug, Clone)]
enum DiffOp {
    Equal(usize),  // index into old_lines
    Delete(usize), // index into old_lines
    Insert(usize), // index into new_lines
}

/// Simple O(ND) diff — good enough for code files up to a few thousand lines.
fn diff_lines<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<DiffOp> {
    let n = old.len();
    let m = new.len();

    if n == 0 {
        return (0..m).map(DiffOp::Insert).collect();
    }
    if m == 0 {
        return (0..n).map(DiffOp::Delete).collect();
    }

    // LCS-based diff via dynamic programming (simpler than Myers for our use case)
    // Build LCS table
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    // Trace back
    let mut ops = Vec::new();
    let mut i = 0;
    let mut j = 0;
    while i < n || j < m {
        if i < n && j < m && old[i] == new[j] {
            ops.push(DiffOp::Equal(i));
            i += 1;
            j += 1;
        } else if j < m && (i >= n || dp[i][j + 1] >= dp[i + 1][j]) {
            ops.push(DiffOp::Insert(j));
            j += 1;
        } else {
            ops.push(DiffOp::Delete(i));
            i += 1;
        }
    }
    ops
}

/// Group diff ops into hunks, each surrounded by `context` equal lines.
fn build_hunks(
    ops: &[DiffOp],
    _old_len: usize,
    _new_len: usize,
    context: usize,
) -> Vec<Vec<DiffOp>> {
    // Find indices of changed ops
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| !matches!(op, DiffOp::Equal(_)))
        .map(|(i, _)| i)
        .collect();

    if changed.is_empty() {
        return vec![];
    }

    // Merge nearby changes into hunk ranges [start, end) in ops index space
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut range_start = changed[0].saturating_sub(context);
    let mut range_end = (changed[0] + context + 1).min(ops.len());

    for &ci in &changed[1..] {
        let new_start = ci.saturating_sub(context);
        if new_start <= range_end {
            // Overlapping — extend
            range_end = (ci + context + 1).min(ops.len());
        } else {
            ranges.push((range_start, range_end));
            range_start = new_start;
            range_end = (ci + context + 1).min(ops.len());
        }
    }
    ranges.push((range_start, range_end));

    ranges.iter().map(|&(s, e)| ops[s..e].to_vec()).collect()
}
