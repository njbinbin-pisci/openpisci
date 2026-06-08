import { useState, useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { ideApi } from "../../../services/tauri/ide";
import type { GitFileStatus, BranchInfo } from "./types";

interface GitPanelProps {
  projectDir: string;
  onDiffClick: (path: string) => void;
  onRefresh: () => void;
  /** Version counter — incremented by the parent whenever git status
   *  should be re-fetched (e.g. after file watcher detects changes). */
  gitPanelVersion?: number;
}

function formatInvokeError(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  if (e && typeof e === "object") {
    const o = e as Record<string, unknown>;
    if (typeof o.message === "string" && o.message) return o.message;
    if (typeof o.data === "string" && o.data) return o.data;
  }
  try {
    const json = JSON.stringify(e);
    if (json && json !== "{}") return json;
  } catch {
    /* ignore */
  }
  return String(e);
}

export default function GitPanel({ projectDir, onDiffClick, onRefresh, gitPanelVersion }: GitPanelProps) {
  const { t } = useTranslation();
  const [statuses, setStatuses] = useState<GitFileStatus[]>([]);
  const [branches, setBranches] = useState<BranchInfo[]>([]);
  const [loading, setLoading] = useState(false);
  const [commitMsg, setCommitMsg] = useState("");
  const [committing, setCommitting] = useState(false);
  const [commitError, setCommitError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!projectDir) return;
    setLoading(true);
    try {
      const [s, b] = await Promise.all([
        ideApi.gitStatus(projectDir),
        ideApi.gitBranches(projectDir),
      ]);
      setStatuses(s);
      setBranches(b);
    } catch (e) {
      console.error("Git status error:", e);
    } finally {
      setLoading(false);
    }
  }, [projectDir]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // When the parent signals that git status should be re-fetched
  // (e.g. after file watcher detects external changes), refresh
  // this panel's internal state so the staged/unstaged lists
  // stay in sync.
  useEffect(() => {
    if (gitPanelVersion !== undefined && gitPanelVersion > 0) {
      refresh();
    }
    // Only depend on gitPanelVersion — not on `refresh` — so that
    // stale-closure issues are avoided and every version bump
    // triggers a refresh.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [gitPanelVersion]);

  const handleStage = useCallback(async (path: string) => {
    if (!projectDir) return;
    try {
      await ideApi.gitAdd(projectDir, path);
      await refresh();
      await onRefresh();
    } catch (e) {
      window.alert(`${t("ide.stageFailed")}\n${formatInvokeError(e)}`);
    }
  }, [projectDir, refresh, onRefresh, t]);

  const handleStageAll = useCallback(async () => {
    if (!projectDir) return;
    await ideApi.gitAddAll(projectDir);
    await refresh();
    await onRefresh();
  }, [projectDir, refresh, onRefresh]);

  const handleUnstageAll = useCallback(async () => {
    if (!projectDir) return;
    await ideApi.gitResetAll(projectDir);
    await refresh();
    await onRefresh();
  }, [projectDir, refresh, onRefresh]);

  const handleUnstage = useCallback(async (path: string) => {
    if (!projectDir) return;
    await ideApi.gitReset(projectDir, path);
    await refresh();
    await onRefresh();
  }, [projectDir, refresh, onRefresh]);

  const handleCommit = useCallback(async () => {
    const message = commitMsg.trim();
    if (!projectDir) {
      setCommitError(t("ide.gitNoProjectDir"));
      return;
    }
    if (!message) {
      setCommitError(t("ide.commitNeedMessage"));
      return;
    }
    if (!statuses.some((s) => s.staged)) {
      setCommitError(t("ide.commitNeedStaged"));
      return;
    }

    setCommitting(true);
    setCommitError(null);
    try {
      await ideApi.gitCommit(projectDir, message);
      setCommitMsg("");
      await refresh();
      await onRefresh();
    } catch (e) {
      const detail = formatInvokeError(e);
      console.error("Commit error:", e);
      setCommitError(detail);
      window.alert(`${t("ide.commitFailed")}\n${detail}`);
    } finally {
      setCommitting(false);
    }
  }, [projectDir, commitMsg, statuses, refresh, onRefresh, t]);

  const handleCheckout = useCallback(
    async (branch: string) => {
      if (!projectDir) return;
      const dirty = statuses.length > 0;
      if (dirty) {
        const ok = window.confirm(
          (t("ide.checkoutDirtyWarn") as string) ||
            `Switch to '${branch}'? You have uncommitted changes that may be overwritten.`,
        );
        if (!ok) return;
      }
      try {
        await ideApi.gitCheckout(projectDir, branch);
        await refresh();
        await onRefresh();
      } catch (e) {
        window.alert(`Checkout failed: ${e}`);
      }
    },
    [projectDir, statuses, t, refresh, onRefresh],
  );

  const handleCreateBranch = useCallback(async () => {
    if (!projectDir) return;
    const name = window.prompt(
      (t("ide.newBranchPrompt") as string) || "New branch name (from HEAD):",
      "",
    );
    if (!name || !name.trim()) return;
    try {
      await ideApi.gitCreateBranch(projectDir, name.trim());
      await refresh();
      await onRefresh();
    } catch (e) {
      window.alert(`Create branch failed: ${e}`);
    }
  }, [projectDir, t, refresh, onRefresh]);

  const statusBadge = (s: string) => {
    const map: Record<string, string> = {
      modified: "M",
      added: "A",
      deleted: "D",
      untracked: "U",
      renamed: "R",
    };
    return map[s] || "?";
  };

  const changed = statuses.filter((s) => !s.staged);
  const staged = statuses.filter((s) => s.staged);
  const koiBranches = branches.filter((b) => b.is_koi);
  const mainBranches = branches.filter((b) => !b.is_koi);
  return (
    <div className="git-panel">
      <div className="ide-sidebar-header">
        <span>{t("ide.sourceControl") || "Source Control"}</span>
        <button onClick={refresh} title={t("common.refresh") || "Refresh"} disabled={loading}>
          {loading ? "…" : "↻"}
        </button>
      </div>

      {/* Commit input */}
      <div className="git-commit-area">
        <input
          type="text"
          className="git-commit-input"
          placeholder={t("ide.commitPlaceholder") || "Commit message"}
          value={commitMsg}
          onChange={(e) => {
            setCommitMsg(e.target.value);
            if (commitError) setCommitError(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
              e.preventDefault();
              void handleCommit();
            }
          }}
          disabled={committing}
        />
        <div className="git-commit-actions">
          <button
            type="button"
            className="git-action-btn"
            onClick={() => void handleCommit()}
            disabled={committing || staged.length === 0 || !commitMsg.trim()}
            title={t("ide.commit")}
          >
            {committing ? "…" : "✓"}
          </button>
        </div>
      </div>
      {commitError && <div className="git-commit-error">{commitError}</div>}

      {/* Staged Changes */}
      <div className="git-panel-section">
        <div className="git-panel-title">
          {t("ide.stagedChanges") || "Staged Changes"} ({staged.length})
          {staged.length > 0 && (
            <button className="git-inline-btn" onClick={handleUnstageAll} title={t("ide.unstageAll") || "Unstage All"}>
              −
            </button>
          )}
        </div>
        {staged.map((s) => (
          <div
            key={`staged-${s.path}`}
            className="git-file-item"
          >
            <span className={`git-file-status-badge ${s.status}`}>
              {statusBadge(s.status)}
            </span>
            <span className="git-file-path" onClick={() => onDiffClick(s.path)}>{s.path}</span>
            <button className="git-inline-btn" onClick={() => handleUnstage(s.path)} title={t("ide.unstage") || "Unstage"}>
              −
            </button>
          </div>
        ))}
      </div>

      {/* Changes */}
      <div className="git-panel-section">
        <div className="git-panel-title">
          {t("ide.changes") || "Changes"} ({changed.length})
          {changed.length > 0 && (
            <button className="git-inline-btn" onClick={handleStageAll} title={t("ide.stageAll") || "Stage All"}>
              +
            </button>
          )}
        </div>
        {changed.length === 0 && (
          <div style={{ opacity: 0.4, fontSize: 12, padding: 4 }}>
            {t("ide.noChanges") || "No changes detected"}
          </div>
        )}
        {changed.map((s) => (
          <div
            key={`changed-${s.path}`}
            className="git-file-item"
          >
            <span className={`git-file-status-badge ${s.status}`}>
              {statusBadge(s.status)}
            </span>
            <span className="git-file-path" onClick={() => onDiffClick(s.path)}>{s.path}</span>
            <button className="git-inline-btn" onClick={() => handleStage(s.path)} title={t("ide.stage") || "Stage"}>
              +
            </button>
          </div>
        ))}
      </div>

      {/* Branches */}
      <div className="git-panel-section">
        <div className="git-panel-title">
          <span>{t("ide.branches") || "Branches"} ({mainBranches.length})</span>
          <button
            className="git-inline-btn"
            onClick={handleCreateBranch}
            title={(t("ide.newBranch") as string) || "New branch"}
            style={{ opacity: 0.6 }}
          >
            +
          </button>
        </div>
        {mainBranches.length === 0 && (
          <div style={{ opacity: 0.4, fontSize: 12, padding: 4 }}>
            {t("ide.noBranches") || "No branches"}
          </div>
        )}
        {mainBranches.map((b) => (
          <div
            key={b.name}
            className={`git-branch-item ${b.is_current ? "current" : ""}`}
            title={b.is_current ? (b.last_commit || "") : `Checkout ${b.name}`}
            onClick={() => { if (!b.is_current) handleCheckout(b.name); }}
          >
            <span className="branch-icon">{b.is_current ? "●" : "⑂"}</span>
            <span className="branch-name">{b.name}</span>
            {b.is_current && (
              <span style={{ opacity: 0.5, fontSize: 10, marginLeft: "auto" }}>
                {t("ide.current") || "current"}
              </span>
            )}
          </div>
        ))}
        {koiBranches.length > 0 && (
          <>
            <div className="git-panel-title" style={{ marginTop: 10 }}>
              Koi {t("ide.branches") || "Branches"} ({koiBranches.length})
            </div>
            {koiBranches.map((b) => (
              <div
                key={b.name}
                className="git-branch-item koi"
                onClick={() => handleCheckout(b.name)}
                title={`Checkout ${b.name}${b.last_commit ? " — " + b.last_commit : ""}`}
              >
                <span className="branch-icon">⑂</span>
                <span className="branch-name">{b.name}</span>
              </div>
            ))}
          </>
        )}
      </div>
    </div>
  );
}
