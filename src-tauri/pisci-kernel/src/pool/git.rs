//! Git helpers used by the pool services.
//!
//! Extracted verbatim (minus the Tauri `ToolContext` cancellation knob)
//! from the desktop-side `pool_org` tool. The service layer owns the
//! cancellation token; callers pass an `Option<Arc<AtomicBool>>` that
//! ticks every 100 ms while the subprocess runs.

use std::path::Path;
use std::process::Output;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::Duration;

/// Run `git <args>` inside `dir` with an async cancellation check.
///
/// Returns the raw `Output` so callers can decide how to present
/// stdout/stderr back to the user.
pub async fn run_git(
    dir: &Path,
    args: &[&str],
    cancel: Option<Arc<AtomicBool>>,
) -> anyhow::Result<Output> {
    if let Some(flag) = cancel.as_ref() {
        if flag.load(Ordering::Relaxed) {
            anyhow::bail!("Operation cancelled by user");
        }
    }

    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args).current_dir(dir).kill_on_drop(true);

    let cancel_flag = cancel.clone();
    let output = tokio::select! {
        biased;
        _ = async move {
            while cancel_flag
                .as_ref()
                .map(|f| !f.load(Ordering::Relaxed))
                .unwrap_or(true)
            {
                if cancel_flag.is_none() {
                    // No cancel knob wired — this arm should never fire.
                    std::future::pending::<()>().await;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        } => {
            anyhow::bail!("Operation cancelled by user");
        }
        result = cmd.output() => result?,
    };

    Ok(output)
}

/// Outcome of [`ensure_git_repo`]: tells the service layer whether it
/// had to create a new repo (so it can mention that in the user-facing
/// summary) or whether the directory was already a repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitInitOutcome {
    /// The directory was not a project directory before (created).
    Initialised,
    /// `.git` was already present — nothing changed.
    AlreadyInitialised,
}

/// Make sure `dir` exists and is a git repository. Creates `.gitignore`
/// (containing `.koi-worktrees/`) and an empty initial commit if the
/// repo is freshly created.
///
/// Returns an error if `dir` cannot be created, if `git init` fails,
/// or if the operation is cancelled.
pub async fn ensure_git_repo(
    dir: &Path,
    cancel: Option<Arc<AtomicBool>>,
) -> anyhow::Result<GitInitOutcome> {
    std::fs::create_dir_all(dir)?;

    if dir.join(".git").exists() {
        return Ok(GitInitOutcome::AlreadyInitialised);
    }

    let init = run_git(dir, &["init"], cancel.clone()).await?;
    if !init.status.success() {
        let stderr = String::from_utf8_lossy(&init.stderr);
        anyhow::bail!("git init failed: {}", stderr);
    }

    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, ".koi-worktrees/\n")?;
    }
    // These two calls are best-effort: even if the user has no git
    // identity configured the pool can still proceed; the service
    // layer only reports the initial `git init` success.
    let _ = run_git(dir, &["add", ".gitignore"], cancel.clone()).await;
    let _ = run_git(
        dir,
        &["commit", "-m", "Initial commit", "--allow-empty"],
        cancel,
    )
    .await;
    Ok(GitInitOutcome::Initialised)
}

/// Per-branch merge result for [`merge_koi_branches`].
#[derive(Debug, Clone)]
pub struct MergeBranchResult {
    pub branch: String,
    pub outcome: MergeOutcome,
}

#[derive(Debug, Clone)]
pub enum MergeOutcome {
    Merged,
    Conflict { message: String },
    Error { message: String },
}

/// Walk every `koi/*` branch and merge it into `main` (falling back to
/// `master` when `main` is missing). Aborts an individual merge on
/// conflict and continues to the next branch.
pub async fn merge_koi_branches(
    dir: &Path,
    cancel: Option<Arc<AtomicBool>>,
) -> anyhow::Result<Vec<MergeBranchResult>> {
    let branches = list_koi_branches(dir, cancel.clone()).await?;
    if branches.is_empty() {
        return Ok(Vec::new());
    }

    let checkout_main = run_git(dir, &["checkout", "main"], cancel.clone()).await;
    let cancelled = cancel
        .as_ref()
        .map(|f| f.load(Ordering::Relaxed))
        .unwrap_or(false);
    if checkout_main.is_err() && !cancelled {
        let _ = run_git(dir, &["checkout", "master"], cancel.clone()).await;
    }

    let mut results = Vec::new();
    for branch in branches {
        if cancel
            .as_ref()
            .map(|f| f.load(Ordering::Relaxed))
            .unwrap_or(false)
        {
            anyhow::bail!("Operation cancelled by user");
        }
        let merge_msg = format!("Merge {}", branch);
        let merge = run_git(
            dir,
            &["merge", "--no-ff", &branch, "-m", &merge_msg],
            cancel.clone(),
        )
        .await;
        let outcome = match merge {
            Ok(o) if o.status.success() => MergeOutcome::Merged,
            Ok(o) => {
                let _ = run_git(dir, &["merge", "--abort"], cancel.clone()).await;
                MergeOutcome::Conflict {
                    message: String::from_utf8_lossy(&o.stderr).trim().to_string(),
                }
            }
            Err(e) => {
                if e.to_string().contains("Operation cancelled by user") {
                    anyhow::bail!("Operation cancelled by user");
                }
                MergeOutcome::Error {
                    message: e.to_string(),
                }
            }
        };
        results.push(MergeBranchResult { branch, outcome });
    }
    Ok(results)
}

/// Create a short-lived git worktree for a Koi to work inside. The
/// worktree lives under `<project_dir>/../.koi-worktrees/<safe-name>-<short-id>`
/// and is placed on a fresh `koi/<safe-name>-<short-id>` branch.
///
/// Returns `None` when `project_dir` is not a git repository or when
/// the `git worktree add` command fails for any reason (the coordinator
/// silently falls back to running inside the main `project_dir` in that
/// case — matching the old `KoiRuntime::setup_worktree` behaviour).
pub fn setup_worktree(
    project_dir: &Path,
    koi_name: &str,
    todo_id: &str,
) -> Option<std::path::PathBuf> {
    if !project_dir.join(".git").exists() {
        return None;
    }
    let short_id: String = todo_id.chars().take(8).collect();
    let safe_name: String = koi_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let branch_name = format!("koi/{}-{}", safe_name, short_id);
    let wt_dir = project_dir
        .parent()
        .unwrap_or(project_dir)
        .join(".koi-worktrees")
        .join(format!("{}-{}", safe_name, short_id));

    let output = std::process::Command::new("git")
        .args([
            "worktree",
            "add",
            &wt_dir.to_string_lossy(),
            "-b",
            &branch_name,
        ])
        .current_dir(project_dir)
        .output();

    match output {
        Ok(o) if o.status.success() => {
            tracing::info!(target: "pool::git", "worktree created at {} (branch {branch_name})", wt_dir.display());
            Some(wt_dir)
        }
        Ok(o) => {
            tracing::warn!(
                target: "pool::git",
                "failed to create worktree: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            None
        }
        Err(e) => {
            tracing::warn!(target: "pool::git", "git worktree command failed: {e}");
            None
        }
    }
}

/// Commit any pending changes inside the worktree and then remove it.
/// Always a best-effort operation — failures are logged at `warn!`
/// because losing a worktree is recoverable from the user's side.
pub fn cleanup_worktree(worktree_path: &Path, koi_name: &str, task: &str) {
    if !worktree_path.exists() {
        return;
    }

    let task_preview: String = if task.chars().count() > 72 {
        task.chars().take(72).collect()
    } else {
        task.to_string()
    };
    let commit_msg = format!("koi/{}: {}", koi_name, task_preview);

    let _ = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(worktree_path)
        .output();
    let _ = std::process::Command::new("git")
        .args(["commit", "-m", &commit_msg, "--allow-empty"])
        .current_dir(worktree_path)
        .output();

    let parent = worktree_path.parent().unwrap_or(worktree_path);
    let output = std::process::Command::new("git")
        .args([
            "worktree",
            "remove",
            &worktree_path.to_string_lossy(),
            "--force",
        ])
        .current_dir(parent)
        .output();
    match output {
        Ok(o) if o.status.success() => {
            tracing::info!(target: "pool::git", "worktree removed: {}", worktree_path.display());
        }
        Ok(o) => {
            tracing::warn!(
                target: "pool::git",
                "failed to remove worktree {}: {}",
                worktree_path.display(),
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        Err(e) => {
            tracing::warn!(target: "pool::git", "git worktree remove command failed: {e}");
        }
    }
}

async fn list_koi_branches(
    dir: &Path,
    cancel: Option<Arc<AtomicBool>>,
) -> anyhow::Result<Vec<String>> {
    let out = run_git(dir, &["branch", "--list", "koi/*"], cancel).await?;
    if !out.status.success() {
        anyhow::bail!(
            "Failed to list git branches: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().trim_start_matches("* ").to_string())
        .filter(|l| !l.is_empty())
        .collect())
}
