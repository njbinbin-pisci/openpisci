/// File search tool — glob (find files by name pattern) and grep (search file contents).
/// Equivalent to Cursor's Glob and Grep tools.
use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const MAX_RESULTS: usize = 500;
const MAX_CONTENT_BYTES: usize = 200 * 1024; // 200 KB per file for grep

pub struct FileSearchTool;

#[async_trait]
impl Tool for FileSearchTool {
    fn name(&self) -> &str { "file_search" }

    fn description(&self) -> &str {
        "Search for files by name pattern (glob) or search file contents by regex (grep). \
         Equivalent to the Glob and Grep tools used by Cursor AI. \
         \
         Actions: \
         - 'glob': Find files whose names match a pattern. Pattern supports * (any chars in name), \
           ** (any path segment), ? (single char). Example: '*.rs', '**/*.json', 'config*'. \
         - 'grep': Search file contents for a regex pattern. Returns matching lines with file path and line number. \
           Use 'include' to filter by file extension (e.g. '*.rs', '*.txt'). \
         \
         Tips: \
         - To find all .py files under C:\\MyApp: action=glob, pattern=**/*.py, path=C:\\MyApp \
         - To find all files containing 'TBRuntime': action=grep, pattern=TBRuntime, path=C:\\Tribon \
         - To search only in .ini files: action=grep, pattern=password, include=*.ini, path=C:\\"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["glob", "grep"],
                    "description": "'glob' = find files by name pattern. 'grep' = search file contents by regex."
                },
                "pattern": {
                    "type": "string",
                    "description": "For glob: file name pattern (e.g. '*.rs', '**/*.json'). For grep: regex pattern to search in file contents."
                },
                "path": {
                    "type": "string",
                    "description": "Root directory to search in. Defaults to C:\\ on Windows."
                },
                "include": {
                    "type": "string",
                    "description": "For grep only: file name filter (e.g. '*.txt', '*.rs'). Only files matching this pattern will be searched."
                },
                "file_extensions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "For grep/glob: restrict search to files with these extensions (e.g. [\"rs\", \"toml\"]). More precise than `include`."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default 50, max 500)"
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum directory depth to recurse (default 10)"
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "For grep: case-sensitive search (default false)"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "For grep: number of lines of context to show around each match (default 0)"
                }
            },
            "required": ["action", "pattern"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let action = match input["action"].as_str() {
            Some(a) => a,
            None => return Ok(ToolResult::err("Missing required parameter: action")),
        };
        let pattern = match input["pattern"].as_str() {
            Some(p) => p,
            None => return Ok(ToolResult::err("Missing required parameter: pattern")),
        };

        let root = input["path"].as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                #[cfg(target_os = "windows")]
                { PathBuf::from("C:\\") }
                #[cfg(not(target_os = "windows"))]
                { PathBuf::from("/") }
            });

        if !root.exists() {
            return Ok(ToolResult::err(format!("Path does not exist: {}", root.display())));
        }

        let max_results = (input["max_results"].as_u64().unwrap_or(50) as usize).min(MAX_RESULTS);
        let max_depth = input["max_depth"].as_u64().unwrap_or(10) as usize;

        // Collect file_extensions filter (lowercase, no leading dot)
        let file_extensions: Vec<String> = input["file_extensions"]
            .as_array()
            .map(|arr| arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim_start_matches('.').to_lowercase())
                .collect())
            .unwrap_or_default();
        let ext_filter: Option<&[String]> = if file_extensions.is_empty() { None } else { Some(&file_extensions) };

        match action {
            "glob" => self.do_glob(pattern, &root, max_results, max_depth, ext_filter),
            "grep" => {
                let include = input["include"].as_str();
                let case_sensitive = input["case_sensitive"].as_bool().unwrap_or(false);
                let context_lines = input["context_lines"].as_u64().unwrap_or(0) as usize;
                self.do_grep(pattern, &root, include, ext_filter, max_results, max_depth, case_sensitive, context_lines)
            }
            _ => Ok(ToolResult::err(format!("Unknown action: {}", action))),
        }
    }
}

impl FileSearchTool {
    fn do_glob(
        &self,
        pattern: &str,
        root: &Path,
        max_results: usize,
        max_depth: usize,
        ext_filter: Option<&[String]>,
    ) -> Result<ToolResult> {
        let regex_pat = glob_to_regex(pattern);
        let re = match regex::Regex::new(&regex_pat) {
            Ok(r) => r,
            Err(e) => return Ok(ToolResult::err(format!("Invalid glob pattern '{}': {}", pattern, e))),
        };

        let mut matches: Vec<String> = Vec::new();
        walk_dir(root, 0, max_depth, &mut |path: &Path| {
            if matches.len() >= max_results {
                return false;
            }
            if path.is_file() {
                // Apply extension filter
                if let Some(exts) = ext_filter {
                    let file_ext = path.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.to_lowercase())
                        .unwrap_or_default();
                    if !exts.iter().any(|e| e == &file_ext) {
                        return true;
                    }
                }
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let rel = path.strip_prefix(root).unwrap_or(path);
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                if re.is_match(&rel_str) || re.is_match(file_name) {
                    matches.push(path.to_string_lossy().to_string());
                }
            }
            true
        });

        if matches.is_empty() {
            return Ok(ToolResult::ok(format!(
                "No files found matching '{}' under {}",
                pattern, root.display()
            )));
        }

        Ok(ToolResult::ok(format!(
            "Found {} file(s) matching '{}' under {}:\n{}",
            matches.len(),
            pattern,
            root.display(),
            matches.join("\n")
        )))
    }

    fn do_grep(
        &self,
        pattern: &str,
        root: &Path,
        include: Option<&str>,
        ext_filter: Option<&[String]>,
        max_results: usize,
        max_depth: usize,
        case_sensitive: bool,
        context_lines: usize,
    ) -> Result<ToolResult> {
        let re = {
            let pat = if case_sensitive {
                pattern.to_string()
            } else {
                format!("(?i){}", pattern)
            };
            match regex::Regex::new(&pat) {
                Ok(r) => r,
                Err(e) => return Ok(ToolResult::err(format!("Invalid regex '{}': {}", pattern, e))),
            }
        };

        // Build include filter regex if provided
        let include_re = include.map(|inc| {
            let pat = glob_to_regex(inc);
            regex::Regex::new(&pat).ok()
        }).flatten();

        let mut results: Vec<String> = Vec::new();
        let mut total_matches = 0usize;

        walk_dir(root, 0, max_depth, &mut |path: &Path| {
            if total_matches >= max_results {
                return false;
            }
            if !path.is_file() {
                return true;
            }

            let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            // Apply extension filter (takes priority over include pattern)
            if let Some(exts) = ext_filter {
                let file_ext = path.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                    .unwrap_or_default();
                if !exts.iter().any(|e| e == &file_ext) {
                    return true;
                }
            } else if let Some(ref inc_re) = include_re {
                // Fall back to include glob pattern
                if !inc_re.is_match(fname) {
                    return true;
                }
            }

            let meta = match std::fs::metadata(path) {
                Ok(m) => m,
                Err(_) => return true,
            };
            if meta.len() > MAX_CONTENT_BYTES as u64 * 4 {
                return true;
            }

            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => return true,
            };

            let lines: Vec<&str> = content.lines().collect();
            let mut file_matches: Vec<String> = Vec::new();

            for (i, line) in lines.iter().enumerate() {
                if total_matches + file_matches.len() >= max_results {
                    break;
                }
                if re.is_match(line) {
                    let line_num = i + 1;
                    if context_lines == 0 {
                        file_matches.push(format!("  {}:{}: {}", path.display(), line_num, line.trim_end()));
                    } else {
                        let start = i.saturating_sub(context_lines);
                        let end = (i + context_lines + 1).min(lines.len());
                        for (j, ctx_line) in lines[start..end].iter().enumerate() {
                            let actual_line = start + j + 1;
                            let marker = if actual_line == line_num { ">" } else { " " };
                            file_matches.push(format!(
                                "  {}:{}{}: {}",
                                path.display(), marker, actual_line, ctx_line.trim_end()
                            ));
                        }
                        file_matches.push(String::new());
                    }
                }
            }

            if !file_matches.is_empty() {
                total_matches += file_matches.len();
                results.extend(file_matches);
            }

            true
        });

        if results.is_empty() {
            let filter_note = match (ext_filter, include) {
                (Some(exts), _) => format!(" (extensions: {})", exts.join(", ")),
                (None, Some(i)) => format!(" (filter: {})", i),
                _ => String::new(),
            };
            return Ok(ToolResult::ok(format!(
                "No matches found for '{}' under {}{}",
                pattern, root.display(), filter_note
            )));
        }

        Ok(ToolResult::ok(format!(
            "Found {} match(es) for '{}' under {}:\n{}",
            total_matches,
            pattern,
            root.display(),
            results.join("\n")
        )))
    }
}

/// Walk a directory recursively, calling `visitor` for each entry.
/// Returns early if visitor returns false.
fn walk_dir<F: FnMut(&Path) -> bool>(dir: &Path, depth: usize, max_depth: usize, visitor: &mut F) {
    if depth > max_depth {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !visitor(&path) {
            return;
        }
        if path.is_dir() {
            // Skip common noise directories
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if matches!(name, ".git" | "node_modules" | "target" | ".cache" | "__pycache__" | ".vs") {
                continue;
            }
            walk_dir(&path, depth + 1, max_depth, visitor);
        }
    }
}

/// Convert a glob pattern to a regex string.
/// Supports: * (match any chars except /), ** (match any path), ? (match single char)
fn glob_to_regex(glob: &str) -> String {
    let mut regex = String::from("(?i)^");
    let chars: Vec<char> = glob.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if i + 1 < chars.len() && chars[i + 1] == '*' => {
                regex.push_str(".*");
                i += 2;
                // Skip trailing slash after **
                if i < chars.len() && (chars[i] == '/' || chars[i] == '\\') {
                    i += 1;
                }
            }
            '*' => {
                regex.push_str("[^/\\\\]*");
                i += 1;
            }
            '?' => {
                regex.push('.');
                i += 1;
            }
            '.' => {
                regex.push_str("\\.");
                i += 1;
            }
            c => {
                // Escape other regex metacharacters
                if "()[]{}+^$|\\".contains(c) {
                    regex.push('\\');
                }
                regex.push(c);
                i += 1;
            }
        }
    }
    regex.push('$');
    regex
}
