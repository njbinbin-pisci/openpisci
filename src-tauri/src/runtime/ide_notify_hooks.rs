//! Agent hooks that journal file edits and notify the IDE file tree / git panel.
//!
//! `notify` watchers can miss same-process writes on some platforms; emitting
//! `ide-file-changed` after successful `file_write` / `file_edit` keeps the
//! Pond IDE explorer and git status in sync when Piscis runs from the CLI panel.

use std::sync::Arc;

use async_trait::async_trait;
use piscis_kernel::agent::file_journal::FileJournal;
use piscis_kernel::agent::hooks::{AgentHooks, HookDecision, ToolHookEvent};
use piscis_kernel::agent::tool::ToolResult;
use tauri::{AppHandle, Emitter};

const FILE_TOOLS: &[&str] = &["file_write", "file_edit"];

/// Wraps [`FileJournal`] and broadcasts IDE refresh events after file mutations.
pub struct JournalWithIdeNotify {
    journal: Arc<FileJournal>,
    app: AppHandle,
}

impl JournalWithIdeNotify {
    pub fn new(journal: Arc<FileJournal>, app: AppHandle) -> Self {
        Self { journal, app }
    }

    fn rel_path(workspace_root: &std::path::Path, raw: &str) -> Option<String> {
        let p = std::path::Path::new(raw);
        let rel = p
            .strip_prefix(workspace_root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/");
        let rel = rel.trim_start_matches('/').to_string();
        if rel.is_empty() || rel == ".git" || rel.starts_with(".git/") {
            return None;
        }
        Some(rel)
    }

    fn emit_file_changed(&self, ev: &ToolHookEvent<'_>, kind: &str) {
        let Some(path) = ev
            .input
            .get("path")
            .and_then(|v| v.as_str())
            .and_then(|raw| Self::rel_path(ev.workspace_root, raw))
        else {
            return;
        };
        let project_dir = ev.workspace_root.to_string_lossy().to_string();
        let _ = self.app.emit(
            "ide-file-changed",
            serde_json::json!({
                "project_dir": project_dir,
                "path": path,
                "kind": kind,
            }),
        );
    }
}

#[async_trait]
impl AgentHooks for JournalWithIdeNotify {
    async fn before_tool(&self, ev: &ToolHookEvent<'_>) -> HookDecision {
        self.journal.before_tool(ev).await
    }

    async fn after_tool(&self, ev: &ToolHookEvent<'_>, result: &ToolResult) {
        self.journal.after_tool(ev, result).await;
        if result.is_error || !FILE_TOOLS.contains(&ev.tool_name) {
            return;
        }
        self.emit_file_changed(ev, "modified");
    }
}
