import { useState, useCallback, useRef, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { ideApi } from "../../../services/tauri/ide";
import type { FileNode } from "./types";

interface FileTreeProps {
  nodes: FileNode[];
  activePath: string | null;
  gitModified: Set<string>;
  gitAdded: Set<string>;
  projectDir: string | null;
  onFileClick: (node: FileNode) => void;
  onRefresh: () => void;
  depth?: number;
}

/** Inline creation state: which parent dir, creating file vs dir */
interface CreatingState {
  parentPath: string; // full path of the directory to create inside
  isDir: boolean;
}

function getFileIcon(name: string): string {
  const ext = name.split(".").pop()?.toLowerCase() || "";
  const iconMap: Record<string, string> = {
    ts: "TS", tsx: "TX", js: "JS", jsx: "JX",
    rs: "RS", py: "PY", go: "GO", java: "JV",
    c: "C", h: "H", cpp: "C+", hpp: "H+",
    json: "{}", yaml: "YM", yml: "YM", toml: "TM",
    md: "MD", txt: "TX", html: "HT", css: "CS",
    scss: "SC", less: "LS", svg: "SV", png: "PN",
    sh: "SH", ps1: "PS", sql: "SQ", lock: "LK",
  };
  return iconMap[ext] || " ";
}

// ─── Inline name input ─────────────────────────────────────────────────

function InlineInput({
  depth,
  isDir,
  onCommit,
  onCancel,
}: {
  depth: number;
  isDir: boolean;
  onCommit: (name: string) => void;
  onCancel: () => void;
}) {
  const ref = useRef<HTMLInputElement>(null);
  const [value, setValue] = useState("");

  useEffect(() => {
    // Auto-focus on mount. Use rAF to ensure DOM is ready after the tree
    // re-renders with expanded parent.
    requestAnimationFrame(() => ref.current?.focus());
  }, []);

  const commit = () => {
    const trimmed = value.trim();
    if (trimmed) onCommit(trimmed);
    else onCancel();
  };

  return (
    <div
      className={`file-tree-item file-tree-inline-input ${isDir ? "dir" : "file"}`}
      style={{ paddingLeft: 8 + depth * 12 }}
    >
      <span className="icon">{isDir ? "▶" : " "}</span>
      <input
        ref={ref}
        className="file-tree-name-input"
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") { e.preventDefault(); commit(); }
          else if (e.key === "Escape") { e.preventDefault(); onCancel(); }
        }}
        onBlur={commit}
        spellCheck={false}
        autoComplete="off"
      />
    </div>
  );
}

// ─── Tree node ──────────────────────────────────────────────────────────

function TreeNode({
  node,
  activePath,
  gitModified,
  gitAdded,
  onFileClick,
  depth,
  creating,
  onCommitCreate,
  onCancelCreate,
}: {
  node: FileNode;
  activePath: string | null;
  gitModified: Set<string>;
  gitAdded: Set<string>;
  onFileClick: (node: FileNode) => void;
  depth: number;
  creating: CreatingState | null;
  onCommitCreate: (name: string) => void;
  onCancelCreate: () => void;
}) {
  const isCreateTarget = creating && creating.parentPath === node.path;
  // Auto-expand directory when creating inside it
  const [expanded, setExpanded] = useState(depth < 2 || !!isCreateTarget);

  // If this node becomes the creation target, expand it
  useEffect(() => {
    if (isCreateTarget) setExpanded(true);
  }, [isCreateTarget]);

  const handleClick = useCallback(() => {
    if (node.is_dir) {
      setExpanded((e) => !e);
    } else {
      onFileClick(node);
    }
  }, [node, onFileClick]);

  const isActive = node.path === activePath;
  const isModified = gitModified.has(node.path);
  const isAdded = gitAdded.has(node.path);

  const classNames = [
    "file-tree-item",
    node.is_dir ? "dir" : "file",
    isActive ? "active" : "",
    isModified ? "git-modified" : "",
    isAdded ? "git-added" : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div>
      <div
        className={classNames}
        style={{ paddingLeft: 8 + depth * 12 }}
        onClick={handleClick}
        title={node.path}
      >
        <span className="icon">
          {node.is_dir ? (expanded ? "▼" : "▶") : getFileIcon(node.name)}
        </span>
        <span className="name">{node.name}</span>
      </div>
      {node.is_dir && expanded && (
        <div>
          {/* Inline input at the TOP of this directory's children (like VS Code) */}
          {isCreateTarget && (
            <InlineInput
              depth={depth + 1}
              isDir={creating!.isDir}
              onCommit={onCommitCreate}
              onCancel={onCancelCreate}
            />
          )}
          {node.children?.map((child) => (
            <TreeNode
              key={child.path}
              node={child}
              activePath={activePath}
              gitModified={gitModified}
              gitAdded={gitAdded}
              onFileClick={onFileClick}
              depth={depth + 1}
              creating={creating}
              onCommitCreate={onCommitCreate}
              onCancelCreate={onCancelCreate}
            />
          ))}
        </div>
      )}
    </div>
  );
}

// ─── FileTree root ──────────────────────────────────────────────────────

export default function FileTree({
  nodes,
  activePath,
  gitModified,
  gitAdded,
  projectDir,
  onFileClick,
  onRefresh,
}: FileTreeProps) {
  const { t } = useTranslation();
  const [creating, setCreating] = useState<CreatingState | null>(null);

  /** Determine the target parent directory for a new file/folder based on
   *  the currently selected path. If a directory is selected, create inside
   *  it. If a file is selected, create inside its parent (sibling level).
   *  If nothing is selected, create at the project root. */
  const resolveParentPath = useCallback((): string | null => {
    if (!projectDir) return null;
    if (!activePath) return projectDir;
    // Walk the tree to find the selected node
    const findNode = (nodes: FileNode[], path: string): FileNode | null => {
      for (const n of nodes) {
        if (n.path === path) return n;
        if (n.children) {
          const found = findNode(n.children, path);
          if (found) return found;
        }
      }
      return null;
    };
    const selected = findNode(nodes, activePath);
    if (!selected) return projectDir;
    if (selected.is_dir) return selected.path;
    // File selected — use its parent directory
    const sep = selected.path.includes("\\") ? "\\" : "/";
    const lastSep = selected.path.lastIndexOf(sep);
    return lastSep > 0 ? selected.path.substring(0, lastSep) : projectDir;
  }, [activePath, nodes, projectDir]);

  const startCreate = useCallback(
    (isDir: boolean) => {
      const parentPath = resolveParentPath();
      if (!parentPath) return;
      setCreating({ parentPath, isDir });
    },
    [resolveParentPath],
  );

  const commitCreate = useCallback(
    async (name: string) => {
      if (!creating) return;
      const sep = creating.parentPath.includes("\\") ? "\\" : "/";
      const fullPath = `${creating.parentPath}${sep}${name}`;
      try {
        await ideApi.fileAction(fullPath, creating.isDir ? "create_dir" : "create_file");
        setCreating(null);
        onRefresh();
      } catch (e) {
        // Keep inline input active so user can retry or Escape.
        console.error("FileTree create failed:", e);
      }
    },
    [creating, onRefresh],
  );

  const cancelCreate = useCallback(() => setCreating(null), []);

  // Is the inline input at root level (parentPath === projectDir)?
  const isRootCreate = creating && creating.parentPath === projectDir;

  return (
    <>
      <div className="ide-sidebar-header">
        <span>{t("ide.explorer") || "Explorer"}</span>
        <div className="ide-sidebar-header-actions">
          <button
            type="button"
            onClick={() => startCreate(false)}
            disabled={!projectDir}
            title={t("ide.newFile") || "New File"}
            aria-label={t("ide.newFile") || "New File"}
          >
            📄+
          </button>
          <button
            type="button"
            onClick={() => startCreate(true)}
            disabled={!projectDir}
            title={t("ide.newFolder") || "New Folder"}
            aria-label={t("ide.newFolder") || "New Folder"}
          >
            📁+
          </button>
          <button
            type="button"
            onClick={onRefresh}
            title={t("ide.refresh") || "Refresh"}
            aria-label={t("ide.refresh") || "Refresh"}
          >
            ↻
          </button>
        </div>
      </div>
      {nodes.length === 0 && !creating ? (
        <div style={{ padding: 12, opacity: 0.5, fontSize: 12 }}>
          {t("ide.noFiles") || "No files found"}
        </div>
      ) : (
        <div className="file-tree-root">
          {/* Root-level inline input (when projectDir itself is the target) */}
          {isRootCreate && (
            <InlineInput
              depth={0}
              isDir={creating!.isDir}
              onCommit={commitCreate}
              onCancel={cancelCreate}
            />
          )}
          {nodes.map((node) => (
            <TreeNode
              key={node.path}
              node={node}
              activePath={activePath}
              gitModified={gitModified}
              gitAdded={gitAdded}
              onFileClick={onFileClick}
              depth={0}
              creating={creating}
              onCommitCreate={commitCreate}
              onCancelCreate={cancelCreate}
            />
          ))}
        </div>
      )}
    </>
  );
}
