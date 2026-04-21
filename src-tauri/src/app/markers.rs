//! Small text/path helpers used by the IM outbound path.
//!
//! Extracted from the old monolithic `desktop_app.rs`. These are pure
//! functions with no global state — they just strip a single-line marker
//! (`SEND_IMAGE:<path>` / `SEND_FILE:<path>`) out of an agent reply and
//! map a file extension to a MIME type.

/// Extract a `SEND_IMAGE:<path>` or `SEND_FILE:<path>` marker from agent reply.
/// Scans all lines (not just the last), removes the marker line, and returns
/// (text_without_marker, Option<file_path>).
pub fn extract_send_marker(text: &str) -> (String, Option<String>) {
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate().rev() {
        let trimmed = line.trim();
        if let Some(path) = trimmed
            .strip_prefix("SEND_IMAGE:")
            .or_else(|| trimmed.strip_prefix("SEND_FILE:"))
        {
            let path = path.trim().to_string();
            if !path.is_empty() {
                let clean_parts: Vec<&str> = lines
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, l)| *l)
                    .collect();
                let clean = clean_parts.join("\n").trim().to_string();
                tracing::info!(
                    "extract_send_marker: found marker at line {}, path={}",
                    i,
                    path
                );
                return (clean, Some(path));
            }
        }
    }
    (text.to_string(), None)
}

/// Guess MIME type from file path extension.
pub fn guess_mime_from_path(path: &str) -> String {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        "image/png".to_string()
    } else if lower.ends_with(".gif") {
        "image/gif".to_string()
    } else if lower.ends_with(".webp") {
        "image/webp".to_string()
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg".to_string()
    } else if lower.ends_with(".pdf") {
        "application/pdf".to_string()
    } else if lower.ends_with(".mp4") {
        "video/mp4".to_string()
    } else {
        "application/octet-stream".to_string()
    }
}
