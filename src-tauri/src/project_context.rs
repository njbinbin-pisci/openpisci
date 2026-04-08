use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

const DEFAULT_SINGLE_FILE_BUDGET_CHARS: usize = 4_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInstructionFile {
    pub path: PathBuf,
    pub content: String,
}

pub fn discover_project_instruction_files(
    start: impl AsRef<Path>,
) -> std::io::Result<Vec<ProjectInstructionFile>> {
    let start = start.as_ref();
    if start.as_os_str().is_empty() || !start.exists() {
        return Ok(Vec::new());
    }

    let base_dir = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent().map_or_else(PathBuf::new, Path::to_path_buf)
    };
    if base_dir.as_os_str().is_empty() {
        return Ok(Vec::new());
    }

    let mut dirs = base_dir.ancestors().map(Path::to_path_buf).collect::<Vec<_>>();
    dirs.reverse();

    let mut files = Vec::new();
    for dir in dirs {
        for candidate in [
            dir.join("PISCI.md"),
            dir.join("PISCI.local.md"),
            dir.join(".pisci").join("PISCI.md"),
            dir.join(".pisci").join("instructions.md"),
        ] {
            push_instruction_file(&mut files, candidate)?;
        }
    }

    Ok(dedupe_instruction_files(files))
}

pub fn render_project_instruction_context(
    start: impl AsRef<Path>,
    total_budget_chars: usize,
) -> std::io::Result<String> {
    if total_budget_chars == 0 {
        return Ok(String::new());
    }

    let files = discover_project_instruction_files(start)?;
    if files.is_empty() {
        return Ok(String::new());
    }

    let mut sections = vec!["## Project Instructions".to_string()];
    let mut remaining = total_budget_chars;

    for file in files {
        if remaining == 0 {
            sections.push("_Additional project instructions omitted after reaching the budget._".to_string());
            break;
        }

        let rendered = truncate_chars(
            file.content.trim(),
            remaining.min(DEFAULT_SINGLE_FILE_BUDGET_CHARS),
        );
        let consumed = rendered.chars().count().min(remaining);
        remaining = remaining.saturating_sub(consumed);

        sections.push(format!("### {}", file.path.display()));
        sections.push(rendered);
    }

    Ok(format!("\n\n{}", sections.join("\n\n")))
}

fn push_instruction_file(
    files: &mut Vec<ProjectInstructionFile>,
    path: PathBuf,
) -> std::io::Result<()> {
    match fs::read_to_string(&path) {
        Ok(content) if !content.trim().is_empty() => {
            files.push(ProjectInstructionFile { path, content });
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn dedupe_instruction_files(files: Vec<ProjectInstructionFile>) -> Vec<ProjectInstructionFile> {
    let mut seen_hashes = Vec::new();
    let mut deduped = Vec::new();

    for file in files {
        let normalized = normalize_content(&file.content);
        let hash = stable_hash(&normalized);
        if seen_hashes.contains(&hash) {
            continue;
        }
        seen_hashes.push(hash);
        deduped.push(file);
    }

    deduped
}

fn normalize_content(content: &str) -> String {
    let mut result = String::new();
    let mut previous_blank = false;
    for line in content.lines() {
        let trimmed = line.trim_end();
        let is_blank = trimmed.is_empty();
        if is_blank && previous_blank {
            continue;
        }
        result.push_str(trimmed);
        result.push('\n');
        previous_blank = is_blank;
    }
    result.trim().to_string()
}

fn stable_hash(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

fn truncate_chars(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }

    let mut truncated = content.chars().take(max_chars).collect::<String>();
    truncated.push_str("\n\n[truncated]");
    truncated
}

#[cfg(test)]
mod tests {
    use super::{discover_project_instruction_files, render_project_instruction_context};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("pisci-project-context-{nanos}"))
    }

    #[test]
    fn discovers_instruction_files_from_ancestor_chain() {
        let root = temp_dir();
        let nested = root.join("apps").join("desktop");
        fs::create_dir_all(root.join(".pisci")).expect("root .pisci");
        fs::create_dir_all(nested.join(".pisci")).expect("nested .pisci");
        fs::write(root.join("PISCI.md"), "root rules").expect("root rules");
        fs::write(
            root.join(".pisci").join("instructions.md"),
            "shared rules",
        )
        .expect("shared rules");
        fs::write(nested.join(".pisci").join("instructions.md"), "local rules")
            .expect("local rules");

        let files = discover_project_instruction_files(&nested).expect("discover");
        let contents = files.iter().map(|file| file.content.as_str()).collect::<Vec<_>>();
        assert_eq!(contents, vec!["root rules", "shared rules", "local rules"]);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn dedupes_equivalent_instruction_content() {
        let root = temp_dir();
        let nested = root.join("child");
        fs::create_dir_all(&nested).expect("child dir");
        fs::write(root.join("PISCI.md"), "same rules\n\n").expect("root write");
        fs::write(nested.join("PISCI.md"), "same rules\n").expect("nested write");

        let files = discover_project_instruction_files(&nested).expect("discover");
        assert_eq!(files.len(), 1);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn renders_budgeted_instruction_context() {
        let root = temp_dir();
        fs::create_dir_all(&root).expect("root dir");
        fs::write(root.join("PISCI.md"), "x".repeat(5000)).expect("write rules");

        let rendered = render_project_instruction_context(&root, 1200).expect("render");
        assert!(rendered.contains("## Project Instructions"));
        assert!(rendered.contains("[truncated]"));

        fs::remove_dir_all(root).expect("cleanup");
    }
}
