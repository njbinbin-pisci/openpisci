/**
 * Overlay Component — Minimal HUD Panel
 *
 * A 280×56 pill-shaped floating strip rendered in the transparent overlay window.
 * Avoids circular-window shape issues on Windows by staying rectangular
 * with rounded corners achieved purely through CSS.
 *
 * Drag:       data-tauri-drag-region on the title area (maximizable=false so
 *             double-click on the drag region does nothing extra)
 * Restore:    "↑ 恢复" button (right side)
 * Right-click: onContextMenu with e.preventDefault() → custom React menu
 * Toasts:     pop above the pill when agent tools run
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { windowApi } from "../../services/tauri";
import "./Overlay.css";

// ─── Types ────────────────────────────────────────────────────────────────────

interface Toast {
  id: number;
  icon: string;
  text: string;
  kind: "start" | "end" | "error";
}

interface AgentEventPayload {
  type: string;
  name?: string;
  delta?: string;
  is_error?: boolean;
}

// ─── Tool icon mapping ────────────────────────────────────────────────────────

const TOOL_ICONS: Record<string, string> = {
  browser:    "🌐",
  file_read:  "📄",
  file_write: "✏️",
  shell:      "💻",
  powershell: "🖥️",
  web_search: "🔍",
  uia:        "🖱️",
  screen:     "📸",
  com_tool:   "🔗",
  email:      "📧",
  wmi_tool:   "🔧",
  office:     "📊",
  dpi:        "🔆",
};

function toolIcon(name: string): string {
  return TOOL_ICONS[name] ?? "⚙️";
}

// ─── Context Menu ─────────────────────────────────────────────────────────────

interface ContextMenuProps {
  onClose: () => void;
  onRestore: () => void;
  onQuit: () => void;
}

function ContextMenu({ onClose, onRestore, onQuit }: ContextMenuProps) {
  const ref = useRef<HTMLUListElement>(null);

  // Close on outside click
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        onClose();
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [onClose]);

  return (
    <ul className="hud-menu" ref={ref}>
      <li onMouseDown={(e) => { e.stopPropagation(); onRestore(); }}>
        🪟 恢复主界面
      </li>
      <li onMouseDown={(e) => { e.stopPropagation(); onQuit(); }}>
        ✕ 退出 Pisci
      </li>
    </ul>
  );
}

// ─── Overlay App ─────────────────────────────────────────────────────────────

let _toastId = 0;

export default function OverlayApp() {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [showMenu, setShowMenu] = useState(false);
  const [status, setStatus] = useState<"idle" | "running">("idle");
  const [lastTool, setLastTool] = useState<string>("");

  // ── Make body transparent so Tauri's transparent window shows correctly ──
  useEffect(() => {
    // Override the global body background (which is set to a dark color)
    document.documentElement.style.background = "transparent";
    document.body.style.background = "transparent";
    document.body.style.overflow = "hidden";
    const root = document.getElementById("root");
    if (root) root.style.background = "transparent";
  }, []);

  // ── Toast management ──────────────────────────────────────────────────────

  const addToast = useCallback((toast: Omit<Toast, "id">) => {
    const id = ++_toastId;
    setToasts((prev) => [...prev.slice(-3), { ...toast, id }]);
    const delay = toast.kind === "end" ? 800 : 2500;
    setTimeout(() => {
      setToasts((prev) => prev.filter((t) => t.id !== id));
    }, delay);
  }, []);

  // ── Agent event listener ──────────────────────────────────────────────────

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<AgentEventPayload>("agent_broadcast", (event) => {
      const p = event.payload;
      if (p.type === "tool_start" && p.name) {
        setStatus("running");
        setLastTool(p.name);
        addToast({ icon: toolIcon(p.name), text: p.name + "…", kind: "start" });
      } else if (p.type === "tool_end" && p.name) {
        setLastTool(p.name);
        addToast({
          icon: p.is_error ? "❌" : "✓",
          text: p.is_error ? `${p.name} 失败` : `${p.name} 完成`,
          kind: p.is_error ? "error" : "end",
        });
      } else if (p.type === "done") {
        setStatus("idle");
        setLastTool("");
      }
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [addToast]);

  // ── Restore main window ───────────────────────────────────────────────────

  const handleRestore = useCallback(async () => {
    setShowMenu(false);
    await windowApi.exitMinimalMode();
  }, []);

  // ── Quit ──────────────────────────────────────────────────────────────────

  const handleQuit = useCallback(() => {
    setShowMenu(false);
    invoke("plugin:process|exit", { code: 0 }).catch(() => {
      window.close();
    });
  }, []);

  // ── Right-click → custom menu ────────────────────────────────────────────

  const handleContextMenu = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setShowMenu((v) => !v);
  }, []);

  return (
    <div className="hud-root" onContextMenu={handleContextMenu}>
      {/* Toast bubbles — float above the pill */}
      <div className="hud-toasts">
        {toasts.map((t) => (
          <div key={t.id} className={`hud-toast hud-toast-${t.kind}`}>
            <span>{t.icon}</span>
            <span className="hud-toast-text">{t.text}</span>
          </div>
        ))}
      </div>

      {/* The pill strip */}
      <div className="hud-pill">
        {/* Drag region: entire pill except the restore button */}
        <div className="hud-drag" data-tauri-drag-region>
          {/* Status dot */}
          <span className={`hud-dot ${status === "running" ? "hud-dot-running" : ""}`} />

          {/* Label */}
          <span className="hud-title" data-tauri-drag-region>🐟 Pisci</span>

          {/* Current tool name (when running) */}
          {lastTool && (
            <span className="hud-tool-badge" data-tauri-drag-region>
              {toolIcon(lastTool)} {lastTool}
            </span>
          )}
        </div>

        {/* Restore button — NOT in drag region */}
        <button
          className="hud-restore-btn"
          onClick={handleRestore}
          title="恢复主界面"
        >
          ↑ 恢复
        </button>
      </div>

      {/* Context menu */}
      {showMenu && (
        <ContextMenu
          onClose={() => setShowMenu(false)}
          onRestore={handleRestore}
          onQuit={handleQuit}
        />
      )}
    </div>
  );
}
