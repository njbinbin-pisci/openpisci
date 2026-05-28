import { useState, useEffect, useCallback, useRef } from "react";
import { useTranslation } from "react-i18next";
import FileTree, { type FileTreeContextMenu } from "./FileTree";
import EditorTabs from "./EditorTabs";
import CodeEditor from "./CodeEditor";
import TerminalPanel from "./Terminal";
import GitPanel from "./GitPanel";
import SearchPanel from "./SearchPanel";
import { ideApi, onFileChanged } from "../../../services/tauri/ide";
import { openPath } from "../../../services/tauri";
import type { FileNode, OpenTab, GitFileStatus } from "./types";
import "./IDE.css";

type SidebarTab = "explorer" | "search" | "git";

/** Right-click context menu state (shown over a tab). */
interface TabContextMenu {
  x: number;
  y: number;
  /** Path of the tab that was right-clicked. */
  targetPath: string;
}

interface IDEProps {
  projectDir: string | null;
  poolSessionId: string | null;
}

/** Handle to the imperative methods exposed by FileTree via its root ref. */
interface FileTreeHandle {
  deleteSelected?: () => void;
  renameActive?: () => void;
  startCreate?: (isDir: boolean) => void;
}

export default function IDE({ projectDir, poolSessionId: _poolSessionId }: IDEProps) {
  const { t } = useTranslation();

  // File tree
  const [fileTree, setFileTree] = useState<FileNode[]>([]);

  // Open tabs
  const [tabs, setTabs] = useState<OpenTab[]>([]);
  const [activeTabPath, setActiveTabPath] = useState<string | null>(null);

  // Git status
  const [gitModified, setGitModified] = useState<Set<string>>(new Set());
  const [gitAdded, setGitAdded] = useState<Set<string>>(new Set());

  // UI state
  const [showTerminal, setShowTerminal] = useState(false);
  const [sidebarTab, setSidebarTab] = useState<SidebarTab>("explorer");
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);

  // Resizable panel widths
  const [sidebarWidth, setSidebarWidth] = useState(260);
  const [terminalHeight, setTerminalHeight] = useState(200);
  const ideRef = useRef<HTMLDivElement>(null);

  // Right-click context menu for tab headers
  const [tabContextMenu, setTabContextMenu] = useState<TabContextMenu | null>(null);

  // File tree: selected paths (Ctrl/Cmd multi-select) + right-click menu.
  const [fileTreeSelection, setFileTreeSelection] = useState<Set<string>>(new Set());
  const [fileTreeContextMenu, setFileTreeContextMenu] = useState<FileTreeContextMenu | null>(null);
  const fileTreeRef = useRef<(HTMLDivElement & FileTreeHandle) | null>(null);

  // Stable refs so keyboard shortcuts / beforeunload always read the latest
  // state without re-registering listeners on every render.
  const tabsRef = useRef<OpenTab[]>(tabs);
  tabsRef.current = tabs;
  const activeTabPathRef = useRef<string | null>(activeTabPath);
  activeTabPathRef.current = activeTabPath;
  const projectDirRef = useRef<string | null>(projectDir);
  projectDirRef.current = projectDir;

  const activeTab = tabs.find((t) => t.path === activeTabPath) || null;

  // ─── Load file tree ──────────────────────────────────────────────
  const loadFileTree = useCallback(async () => {
    if (!projectDir) return;
    try {
      const nodes = await ideApi.listFiles(projectDir, 8);
      setFileTree(nodes);
    } catch (e) {
      console.error("Failed to load file tree:", e);
    }
  }, [projectDir]);

  // ─── Load git status ─────────────────────────────────────────────
  const loadGitStatus = useCallback(async () => {
    if (!projectDir) return;
    try {
      const statuses = await ideApi.gitStatus(projectDir);
      const modified = new Set<string>();
      const added = new Set<string>();
      statuses.forEach((s: GitFileStatus) => {
        if (s.status === "modified") modified.add(s.path);
        else if (s.status === "added" || s.status === "untracked") added.add(s.path);
      });
      setGitModified(modified);
      setGitAdded(added);
    } catch {
      // No git repo or error — ignore
    }
  }, [projectDir]);

  // ─── Panel resize drag handlers ──────────────────────────────────
  const startSidebarResize = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      const startX = e.clientX;
      const startW = sidebarWidth;
      const onMove = (ev: MouseEvent) => {
        const delta = ev.clientX - startX;
        setSidebarWidth(Math.min(500, Math.max(220, startW + delta)));
      };
      const onUp = () => {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
    },
    [sidebarWidth],
  );

  const startTerminalResize = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      const startY = e.clientY;
      const startH = terminalHeight;
      const onMove = (ev: MouseEvent) => {
        const delta = startY - ev.clientY;
        setTerminalHeight(Math.min(400, Math.max(120, startH + delta)));
      };
      const onUp = () => {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
      document.body.style.cursor = "row-resize";
      document.body.style.userSelect = "none";
    },
    [terminalHeight],
  );

  // ─── Initialize ──────────────────────────────────────────────────
  // Debounce file-change refreshes: bursty external edits (Koi agents,
  // formatters, watch-mode builds) used to fire `loadFileTree`+`loadGitStatus`
  // dozens of times per second, which on Windows compounded the popup-loop
  // bug that v0.8.0 fixed at the watcher level. 250 ms trailing-edge is
  // slow enough to coalesce a save-burst yet fast enough to feel live.
  const refreshTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const scheduleRefresh = useCallback(() => {
    if (refreshTimer.current) clearTimeout(refreshTimer.current);
    refreshTimer.current = setTimeout(() => {
      refreshTimer.current = null;
      loadFileTree();
      loadGitStatus();
    }, 250);
  }, [loadFileTree, loadGitStatus]);

  useEffect(() => {
    if (!projectDir) return;
    loadFileTree();
    loadGitStatus();

    // Start file watcher
    ideApi.startWatcher(projectDir).catch(() => {});

    // Listen for file changes (from Koi agents or external edits)
    const unlistenPromise = onFileChanged((evt) => {
      // Guard: `project_dir` is whatever the caller passed to startWatcher —
      // compare raw. But normalize the path to `/` before comparing with
      // `tab.path`, because `tab.path` is always stored with `/` (that's
      // how `FileTree` node paths come from the backend, and how
      // `openFile` stores them). On Windows, older backend versions emit
      // backslash paths, which silently failed the `===` check and made
      // externally-modified files never reload in the IDE.
      if (evt.project_dir !== projectDir) return;
      const evtPath = evt.path.replace(/\\/g, "/");
      scheduleRefresh();

      setTabs((prev) =>
        prev.map((tab) => {
          if (tab.path === evtPath && !tab.isDirty) {
            const fullPath = `${projectDir}/${evtPath}`;
            ideApi.readFile(fullPath).then((fc) => {
              setTabs((p) =>
                p.map((t) =>
                  t.path === evtPath && !t.isDirty
                    ? { ...t, content: fc.content }
                    : t,
                ),
              );
            }).catch(() => {});
          }
          return tab;
        }),
      );
    });

    return () => {
      if (refreshTimer.current) {
        clearTimeout(refreshTimer.current);
        refreshTimer.current = null;
      }
      unlistenPromise.then((fn) => fn());
      ideApi.stopWatcher(projectDir).catch(() => {});
    };
  }, [projectDir, loadFileTree, loadGitStatus, scheduleRefresh]);

  // ─── Open a file ─────────────────────────────────────────────────
  const openFile = useCallback(
    async (path: string, readOnly = false) => {
      // Check if already open
      const existing = tabs.find((t) => t.path === path);
      if (existing) {
        setActiveTabPath(path);
        return;
      }

      const fullPath = projectDir ? `${projectDir}/${path}` : path;
      try {
        const fc = await ideApi.readFile(fullPath);
        if (fc.is_binary) {
          return; // Don't open binary files
        }
        const newTab: OpenTab = {
          path,
          name: path.split("/").pop() || path,
          language: fc.language,
          content: fc.content,
          isDirty: false,
          isReadOnly: readOnly,
        };
        setTabs((prev) => [...prev, newTab]);
        setActiveTabPath(path);
      } catch (e) {
        console.error("Failed to read file:", e);
      }
    },
    [projectDir, tabs],
  );

  // ─── Open diff for a file ────────────────────────────────────────
  const openDiff = useCallback(
    async (path: string) => {
      if (!projectDir) return;
      const diffPath = `diff:${path}`;
      const existing = tabs.find((t) => t.path === diffPath);
      if (existing) {
        setActiveTabPath(diffPath);
        return;
      }

      try {
        const diff = await ideApi.gitDiff(projectDir, path);
        const newTab: OpenTab = {
          path: diffPath,
          name: `${path} (diff)`,
          language: null,
          content: diff.modified,
          isDirty: false,
          isReadOnly: true,
          isDiff: true,
          originalContent: diff.original,
        };
        setTabs((prev) => [...prev, newTab]);
        setActiveTabPath(diffPath);
      } catch (e) {
        console.error("Failed to get diff:", e);
      }
    },
    [projectDir, tabs],
  );

  // ─── Handle editor content change ───────────────────────────────
  const handleEditorChange = useCallback(
    (value: string) => {
      if (!activeTabPath) return;
      setTabs((prev) =>
        prev.map((t) =>
          t.path === activeTabPath
            ? { ...t, content: value, isDirty: true }
            : t,
        ),
      );
    },
    [activeTabPath],
  );

  // ─── Save file (Ctrl+S) ──────────────────────────────────────────
  // Uses refs so the latest tab content / projectDir are always used,
  // even if the user's keystrokes raced the React render cycle.
  const saveFile = useCallback(
    async (path: string) => {
      const tab = tabsRef.current.find((t) => t.path === path);
      const dir = projectDirRef.current;
      if (!tab || !dir) return;
      const fullPath = `${dir}/${path}`;
      try {
        await ideApi.writeFile(fullPath, tab.content);
        setTabs((prev) =>
          prev.map((t) => (t.path === path ? { ...t, isDirty: false } : t)),
        );
        loadGitStatus();
      } catch (e) {
        console.error("Failed to save:", e);
      }
    },
    [loadGitStatus],
  );

  // ─── Close tab (no dirty prompt — used internally) ─────────────────
  const removeTab = useCallback(
    (path: string) => {
      setTabs((prev) => {
        const idx = prev.findIndex((t) => t.path === path);
        const next = prev.filter((t) => t.path !== path);
        if (activeTabPathRef.current === path) {
          const newActive = next[Math.min(idx, next.length - 1)] || null;
          setActiveTabPath(newActive?.path || null);
        }
        return next;
      });
    },
    [],
  );

  // ─── Close tab (with dirty prompt) ──────────────────────────────────
  const closeTab = useCallback(
    (path: string) => {
      const tab = tabsRef.current.find((t) => t.path === path);
      if (tab?.isDirty) {
        const name = tab.name || path;
        const msg = t("ide.unsavedConfirm", { name })
          || `"${name}" has unsaved changes. Save before closing?`;
        // eslint-disable-next-line no-alert
        const answer = window.confirm(msg);
        if (!answer) return; // cancel — keep tab open
        // User clicked OK — save then close
        const dir = projectDirRef.current;
        if (dir && !tab.isReadOnly && !tab.path.startsWith("diff:")) {
          ideApi.writeFile(`${dir}/${tab.path}`, tab.content)
            .then(() => removeTab(path))
            .catch(() => removeTab(path));
        } else {
          removeTab(path);
        }
      } else {
        removeTab(path);
      }
    },
    [removeTab, t],
  );

  // ─── Context menu actions ─────────────────────────────────────────
  const closeAllTabs = useCallback(async () => {
    const dir = projectDirRef.current;
    const snapshot = tabsRef.current.slice();
    // Save all dirty tabs first (with confirmation)
    const dirty = snapshot.filter((t) => t.isDirty);
    if (dirty.length > 0) {
      // eslint-disable-next-line no-alert
      const msg = t("ide.unsavedBulkConfirm", { count: dirty.length })
        || `${dirty.length} file(s) have unsaved changes. Save all before closing?`;
      // eslint-disable-next-line no-alert
      const save = window.confirm(msg);
      if (save && dir) {
        await Promise.all(
          dirty
            .filter((t) => !t.isReadOnly && !t.path.startsWith("diff:"))
            .map((t) => ideApi.writeFile(`${dir}/${t.path}`, t.content).catch(() => {})),
        );
      }
    }
    setTabs([]);
    setActiveTabPath(null);
    loadGitStatus();
  }, [loadGitStatus, t]);

  const closeUnsavedTabs = useCallback(async () => {
    const dir = projectDirRef.current;
    const snapshot = tabsRef.current.slice();
    const dirty = snapshot.filter((t) => t.isDirty);
    if (dirty.length > 0) {
      // eslint-disable-next-line no-alert
      const msg = t("ide.unsavedBulkConfirm", { count: dirty.length })
        || `${dirty.length} file(s) have unsaved changes. Save before closing?`;
      // eslint-disable-next-line no-alert
      const save = window.confirm(msg);
      if (save && dir) {
        await Promise.all(
          dirty
            .filter((t) => !t.isReadOnly && !t.path.startsWith("diff:"))
            .map((t) => ideApi.writeFile(`${dir}/${t.path}`, t.content).catch(() => {})),
        );
      }
    }
    setTabs((prev) => prev.filter((t) => !t.isDirty));
    setActiveTabPath((current) => {
      const stillOpen = tabsRef.current.filter((t) => !t.isDirty);
      if (current && stillOpen.some((t) => t.path === current)) return current;
      return stillOpen[0]?.path || null;
    });
    loadGitStatus();
  }, [loadGitStatus, t]);

  const closeOtherTabs = useCallback(async (keepPath: string) => {
    const dir = projectDirRef.current;
    const snapshot = tabsRef.current.slice();
    const closingDirty = snapshot.filter((t) => t.path !== keepPath && t.isDirty);
    if (closingDirty.length > 0) {
      // eslint-disable-next-line no-alert
      const msg = t("ide.unsavedBulkConfirm", { count: closingDirty.length })
        || `${closingDirty.length} file(s) have unsaved changes. Save before closing?`;
      // eslint-disable-next-line no-alert
      const save = window.confirm(msg);
      if (save && dir) {
        await Promise.all(
          closingDirty
            .filter((t) => !t.isReadOnly && !t.path.startsWith("diff:"))
            .map((t) => ideApi.writeFile(`${dir}/${t.path}`, t.content).catch(() => {})),
        );
      }
    }
    setTabs((prev) => prev.filter((t) => t.path === keepPath));
    setActiveTabPath(keepPath);
    loadGitStatus();
  }, [loadGitStatus, t]);

  const handleTabContextMenu = useCallback(
    (e: React.MouseEvent, path: string) => {
      e.preventDefault();
      e.stopPropagation();
      setTabContextMenu({ x: e.clientX, y: e.clientY, targetPath: path });
    },
    [],
  );

  // ─── File tree: multi-select + context menu ──────────────────────────
  const handleFileTreeSelect = useCallback(
    (path: string, opts: { multi: boolean }) => {
      setFileTreeSelection((prev) => {
        if (opts.multi) {
          const next = new Set(prev);
          if (next.has(path)) next.delete(path);
          else next.add(path);
          return next;
        }
        return new Set([path]);
      });
    },
    [],
  );

  const handleFileTreeContextMenu = useCallback(
    (menu: FileTreeContextMenu) => {
      setFileTreeContextMenu(menu);
    },
    [],
  );

  // Dismiss file tree context menu on outside click / escape / scroll
  useEffect(() => {
    if (!fileTreeContextMenu) return;
    const dismiss = () => setFileTreeContextMenu(null);
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") dismiss(); };
    window.addEventListener("click", dismiss);
    window.addEventListener("contextmenu", dismiss);
    window.addEventListener("keydown", onKey);
    window.addEventListener("scroll", dismiss, true);
    return () => {
      window.removeEventListener("click", dismiss);
      window.removeEventListener("contextmenu", dismiss);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", dismiss, true);
    };
  }, [fileTreeContextMenu]);

  // ─── File tree keyboard shortcuts (Delete / F2) ───────────────────────
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      // Only when the file tree (or a child input inside it) has focus
      const el = fileTreeRef.current;
      if (!el) return;
      if (!el.contains(document.activeElement) && document.activeElement !== el) return;
      // Don't intercept when the user is typing in an inline create/rename input
      if ((document.activeElement as HTMLElement)?.tagName === "INPUT") return;

      if (e.key === "Delete") {
        e.preventDefault();
        el.deleteSelected?.();
      } else if (e.key === "F2") {
        e.preventDefault();
        el.renameActive?.();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  // Dismiss context menu on outside click / escape / scroll
  useEffect(() => {
    if (!tabContextMenu) return;
    const dismiss = () => setTabContextMenu(null);
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") dismiss(); };
    window.addEventListener("click", dismiss);
    window.addEventListener("contextmenu", dismiss);
    window.addEventListener("keydown", onKey);
    window.addEventListener("scroll", dismiss, true);
    return () => {
      window.removeEventListener("click", dismiss);
      window.removeEventListener("contextmenu", dismiss);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", dismiss, true);
    };
  }, [tabContextMenu]);

  // ─── Keyboard shortcut: Ctrl+S to save (stable, reads refs) ──────
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      if (e.key === "s" || e.key === "S") {
        e.preventDefault();
        const active = activeTabPathRef.current;
        if (active && !active.startsWith("diff:")) {
          saveFile(active);
        }
      } else if ((e.key === "k" || e.key === "K") && e.shiftKey) {
        // Ctrl+Shift+S or Ctrl+K+S — save all
        // (handled below; included for symmetry)
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [saveFile]);

  // ─── beforeunload: warn if any tab has unsaved changes ────────────
  useEffect(() => {
    const handler = (e: BeforeUnloadEvent) => {
      const hasDirty = tabsRef.current.some((t) => t.isDirty);
      if (hasDirty) {
        e.preventDefault();
        // Modern browsers ignore custom messages but still require returnValue
        e.returnValue = "";
      }
    };
    window.addEventListener("beforeunload", handler);
    return () => window.removeEventListener("beforeunload", handler);
  }, []);

  // ─── No project dir placeholder ──────────────────────────────────
  if (!projectDir) {
    return (
      <div className="pond-ide">
        <div className="ide-no-project">
          <div className="icon">📂</div>
          <div>{t("ide.noProjectDir") || "No project directory configured for this pool."}</div>
          <div style={{ fontSize: 12, opacity: 0.6 }}>
            {t("ide.noProjectDirHint") ||
              "Set a project_dir on the pool session to enable the IDE."}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="pond-ide" ref={ideRef}>
      {/* Activity bar (icon strip) */}
      <div className="ide-activity-bar">
        <button
          className={sidebarTab === "explorer" && !sidebarCollapsed ? "active" : ""}
          onClick={() => { sidebarCollapsed ? setSidebarCollapsed(false) : setSidebarTab("explorer"); if (sidebarCollapsed) setSidebarTab("explorer"); else if (sidebarTab === "explorer") setSidebarCollapsed(true); else setSidebarTab("explorer"); }}
          title={t("ide.explorer") || "Explorer"}
        >
          <span className="activity-icon">📁</span>
        </button>
        <button
          className={sidebarTab === "search" && !sidebarCollapsed ? "active" : ""}
          onClick={() => { if (sidebarCollapsed) { setSidebarCollapsed(false); setSidebarTab("search"); } else if (sidebarTab === "search") setSidebarCollapsed(true); else setSidebarTab("search"); }}
          title={t("ide.search") || "Search"}
        >
          <span className="activity-icon">🔍</span>
        </button>
        <button
          className={sidebarTab === "git" && !sidebarCollapsed ? "active" : ""}
          onClick={() => { if (sidebarCollapsed) { setSidebarCollapsed(false); setSidebarTab("git"); } else if (sidebarTab === "git") setSidebarCollapsed(true); else setSidebarTab("git"); }}
          title={t("ide.sourceControl") || "Source Control"}
        >
          <span className="activity-icon">⑂</span>
          {(gitModified.size + gitAdded.size) > 0 && (
            <span className="activity-badge">{gitModified.size + gitAdded.size}</span>
          )}
        </button>
        <div style={{ flex: 1 }} />
        <button
          className={showTerminal ? "active" : ""}
          onClick={() => setShowTerminal((v) => !v)}
          title={t("ide.terminal") || "Terminal"}
        >
          <span className="activity-icon">⌨</span>
        </button>
      </div>

      {/* Sidebar content */}
      {!sidebarCollapsed && (
        <div className="ide-sidebar" style={{ width: sidebarWidth }}>
          {sidebarTab === "explorer" && (
            <FileTree
              nodes={fileTree}
              activePath={activeTabPath}
              selectedPaths={fileTreeSelection}
              gitModified={gitModified}
              gitAdded={gitAdded}
              projectDir={projectDir}
              onFileClick={(node) => openFile(node.path)}
              onRefresh={() => {
                loadFileTree();
                loadGitStatus();
              }}
              onSelect={handleFileTreeSelect}
              onContextMenu={handleFileTreeContextMenu}
              containerRef={fileTreeRef}
            />
          )}
          {sidebarTab === "search" && (
            <SearchPanel
              projectDir={projectDir}
              onResultClick={(path, _line) => openFile(path)}
            />
          )}
          {sidebarTab === "git" && (
            <GitPanel
              projectDir={projectDir}
              onDiffClick={(path) => openDiff(path)}
              onRefresh={loadGitStatus}
            />
          )}
        </div>
      )}

      {/* Sidebar resize handle */}
      {!sidebarCollapsed && (
        <div
          className="ide-resize-handle-h"
          onMouseDown={startSidebarResize}
        />
      )}

      {/* Editor area */}
      <div className="ide-editor-area">
        <EditorTabs
          tabs={tabs}
          activeTabPath={activeTabPath}
          onTabClick={setActiveTabPath}
          onTabClose={closeTab}
          onSave={saveFile}
          onTabContextMenu={handleTabContextMenu}
          onCloseAll={closeAllTabs}
          onCloseUnsaved={closeUnsavedTabs}
          onCloseOther={closeOtherTabs}
          contextMenu={tabContextMenu}
        />
        <div className="ide-editor">
          {activeTab ? (
            <CodeEditor
              tab={activeTab}
              theme="violet"
              projectDir={projectDir}
              onChange={handleEditorChange}
              onSave={() => {
                const p = activeTabPathRef.current;
                if (p && !p.startsWith("diff:")) saveFile(p);
              }}
            />
          ) : (
            <div className="ide-editor-welcome">
              <img src="/pisci.png" alt="Pisci" className="welcome-logo" />
              <div className="welcome-title">
                {t("ide.welcome") || "Select a file to start editing"}
              </div>
              <div className="welcome-hint">
                {t("ide.welcomeHint") ||
                  "Collaborate with Koi agents in the same project directory"}
              </div>
            </div>
          )}
        </div>

        {/* Terminal resize handle */}
        {showTerminal && (
          <div
            className="ide-resize-handle-v"
            onMouseDown={startTerminalResize}
          />
        )}

        <TerminalPanel
          projectDir={projectDir}
          visible={showTerminal}
          onClose={() => setShowTerminal(false)}
          height={terminalHeight}
        />
      </div>

      {/* File tree right-click context menu */}
      {fileTreeContextMenu && (
        <div
          className="ide-tab-context-menu"
          style={{ position: "fixed", left: fileTreeContextMenu.x, top: fileTreeContextMenu.y, zIndex: 1000 }}
        >
          <button onClick={() => { openFile(fileTreeContextMenu.targetPath); setFileTreeContextMenu(null); }}>
            {t("ide.openFile") || "Open"}
          </button>
          <button onClick={() => { fileTreeRef.current?.renameActive?.(); setFileTreeContextMenu(null); }}>
            {t("ide.renameFile") || "Rename"} <span style={{ opacity: 0.5, fontSize: 11, marginLeft: 8 }}>F2</span>
          </button>
          <button onClick={() => { fileTreeRef.current?.deleteSelected?.(); setFileTreeContextMenu(null); }}>
            {t("ide.deleteFile") || "Delete"} <span style={{ opacity: 0.5, fontSize: 11, marginLeft: 8 }}>Del</span>
          </button>
          <div className="ide-tab-context-menu-sep" />
          <button onClick={() => {
            const dir = projectDirRef.current;
            if (dir) navigator.clipboard.writeText(`${dir}/${fileTreeContextMenu.targetPath}`).catch(() => {});
            setFileTreeContextMenu(null);
          }}>
            {t("ide.copyPath") || "Copy Path"}
          </button>
          <button onClick={() => {
            navigator.clipboard.writeText(fileTreeContextMenu.targetPath).catch(() => {});
            setFileTreeContextMenu(null);
          }}>
            {t("ide.copyRelPath") || "Copy Relative Path"}
          </button>
          <button onClick={() => {
            const dir = projectDirRef.current;
            if (dir) openPath(`${dir}/${fileTreeContextMenu.targetPath}`).catch(() => {});
            setFileTreeContextMenu(null);
          }}>
            {t("ide.revealInExplorer") || "Reveal in File Manager"}
          </button>
          <div className="ide-tab-context-menu-sep" />
          <button onClick={() => { fileTreeRef.current?.startCreate?.(false); setFileTreeContextMenu(null); }}>
            {t("ide.newFile") || "New File"}
          </button>
          <button onClick={() => { fileTreeRef.current?.startCreate?.(true); setFileTreeContextMenu(null); }}>
            {t("ide.newFolder") || "New Folder"}
          </button>
        </div>
      )}
    </div>
  );
}
