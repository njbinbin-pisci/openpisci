//! File-journal commands (Undo / replay).
//!
//! The journal implementation is the shared kernel one
//! ([`piscis_kernel::agent::file_journal`]) — the same component AgentZ uses — so
//! both hosts stay in lockstep. openpiscis stores snapshots per workspace at
//! `{workspace_root}/.piscis/journal.db`.
//!
//! `chat_send` returns before the agent finishes (the turn runs in a spawned
//! task), so these commands operate on the *latest* turn that still has
//! applied, not-yet-undone changes rather than a caller-supplied turn id.

use std::path::Path;

use piscis_kernel::agent::file_journal::{FileJournal, JournalChange};
use tauri::State;

use crate::commands::chat::resolve_session_workspace_root;
use crate::store::AppState;

fn open_workspace_journal(workspace_root: &str) -> Result<FileJournal, String> {
    let db = Path::new(workspace_root).join(".piscis").join("journal.db");
    FileJournal::open(workspace_root, db).map_err(|e| e.to_string())
}

/// Resolve the session's effective workspace root (per-session override, else
/// the global setting) so the frontend only needs to pass a session id.
async fn session_workspace(
    state: &State<'_, AppState>,
    session_id: &str,
) -> Result<String, String> {
    let default_root = { state.settings.lock().await.workspace_root.clone() };
    resolve_session_workspace_root(state, session_id, default_root).await
}

/// Files changed by the most recent turn (applied, not yet undone), newest first.
#[tauri::command]
pub async fn journal_list_changes(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<JournalChange>, String> {
    let root = session_workspace(&state, &session_id).await?;
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
/// Returns the restored relative paths.
#[tauri::command]
pub async fn journal_undo_last(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<String>, String> {
    let root = session_workspace(&state, &session_id).await?;
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
