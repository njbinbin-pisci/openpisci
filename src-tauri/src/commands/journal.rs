//! File-journal commands (Undo / replay).
//!
//! The journal implementation is the shared kernel one
//! ([`pisci_kernel::agent::file_journal`]) — the same component CodeZ uses — so
//! both hosts stay in lockstep. openpisci stores snapshots per workspace at
//! `{workspace_root}/.pisci/journal.db`.
//!
//! `chat_send` returns before the agent finishes (the turn runs in a spawned
//! task), so these commands operate on the *latest* turn that still has
//! applied, not-yet-undone changes rather than a caller-supplied turn id.

use std::path::Path;

use pisci_kernel::agent::file_journal::{FileJournal, JournalChange};
use tauri::State;

use crate::commands::chat::resolve_session_workspace_root;
use crate::store::AppState;

fn open_workspace_journal(workspace_root: &str) -> Result<FileJournal, String> {
    let db = Path::new(workspace_root).join(".pisci").join("journal.db");
    FileJournal::open(workspace_root, db).map_err(|e| e.to_string())
}

/// Files changed by the most recent turn (applied, not yet undone), newest first.
#[tauri::command]
pub async fn journal_list_changes(
    state: State<'_, AppState>,
    session_id: String,
    workspace_root: String,
) -> Result<Vec<JournalChange>, String> {
    let root = resolve_session_workspace_root(&state, &session_id, workspace_root).await?;
    let journal = open_workspace_journal(&root)?;
    match journal
        .latest_turn_with_changes(&session_id)
        .map_err(|e| e.to_string())?
    {
        Some(turn) => journal
            .list_changes(&session_id, &turn)
            .map_err(|e| e.to_string()),
        None => Ok(Vec::new()),
    }
}

/// Undo every file change from the most recent turn, restoring pre-edit content.
#[tauri::command]
pub async fn journal_undo_last(
    state: State<'_, AppState>,
    session_id: String,
    workspace_root: String,
) -> Result<Vec<String>, String> {
    let root = resolve_session_workspace_root(&state, &session_id, workspace_root).await?;
    let journal = open_workspace_journal(&root)?;
    match journal
        .latest_turn_with_changes(&session_id)
        .map_err(|e| e.to_string())?
    {
        Some(turn) => journal
            .undo_turn(&session_id, &turn)
            .map_err(|e| e.to_string()),
        None => Ok(Vec::new()),
    }
}
