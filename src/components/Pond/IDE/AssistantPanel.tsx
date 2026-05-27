/**
 * Pisci CLI-style assistant panel — a lightweight, terminal-shaped chat
 * that occupies the same bottom slot as TerminalPanel. Targeted at users
 * who would rather describe a task in natural language ("build the
 * project", "git status", "find TODOs in src/") than type shell commands.
 *
 * Design constraints:
 * - Self-contained: creates its own dedicated chat session lazily on
 *   first message, scoped per project directory so each project has its
 *   own conversation history.
 * - Streaming: listens to `agent_event_*` and renders text deltas live.
 * - Plain-text rendering (monospace) — this is a CLI, not a chat bubble.
 * - Tool calls / errors are surfaced as muted lines so the user can see
 *   what Pisci is actually doing under the hood.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { sessionsApi, chatApi, type AgentEventType } from "../../../services/tauri/chat";

interface AssistantPanelProps {
  projectDir: string | null;
  visible: boolean;
  onClose: () => void;
  height?: number;
}

interface CliLine {
  /** "user" | "assistant" | "tool" | "error" | "info" */
  kind: "user" | "assistant" | "tool" | "error" | "info";
  text: string;
}

export default function AssistantPanel({
  projectDir,
  visible,
  onClose,
  height,
}: AssistantPanelProps) {
  const { t } = useTranslation();
  const [lines, setLines] = useState<CliLine[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const sessionIdRef = useRef<string | null>(null);
  const unlistenRef = useRef<(() => void) | null>(null);
  const streamingTextRef = useRef<string>(""); // buffer text deltas for current turn
  const bodyRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  const scrollToBottom = useCallback(() => {
    requestAnimationFrame(() => {
      const el = bodyRef.current;
      if (el) el.scrollTop = el.scrollHeight;
    });
  }, []);

  const append = useCallback((line: CliLine) => {
    setLines((prev) => [...prev, line]);
    scrollToBottom();
  }, [scrollToBottom]);

  /** Update the last assistant line in place (used while streaming). */
  const updateLastAssistant = useCallback((delta: string) => {
    setLines((prev) => {
      const next = [...prev];
      const last = next[next.length - 1];
      if (last && last.kind === "assistant") {
        next[next.length - 1] = { ...last, text: last.text + delta };
      } else {
        next.push({ kind: "assistant", text: delta });
      }
      return next;
    });
    scrollToBottom();
  }, [scrollToBottom]);

  /** Lazily ensure a chat session exists. Bound to project dir so per-
   *  project history doesn't bleed between projects. */
  const ensureSession = useCallback(async (): Promise<string> => {
    if (sessionIdRef.current) return sessionIdRef.current;
    const title = projectDir
      ? `Pisci CLI — ${projectDir.split(/[\\/]/).pop() ?? projectDir}`
      : "Pisci CLI";
    const session = await sessionsApi.create(title);
    sessionIdRef.current = session.id;
    if (projectDir) {
      try { await sessionsApi.setWorkspace(session.id, projectDir); } catch { /* ignore */ }
    }
    return session.id;
  }, [projectDir]);

  /** Subscribe to agent events for the current session. Tears down any
   *  previous subscription. */
  const subscribe = useCallback(async (sessionId: string) => {
    if (unlistenRef.current) { unlistenRef.current(); unlistenRef.current = null; }
    const unlisten = await chatApi.onEvent(sessionId, (evt: AgentEventType) => {
      switch (evt.type) {
        case "text_delta":
          streamingTextRef.current += evt.delta;
          updateLastAssistant(evt.delta);
          break;
        case "tool_start":
          append({ kind: "tool", text: `[${evt.name}] running…` });
          break;
        case "tool_end": {
          const trimmed = evt.result.length > 200
            ? evt.result.slice(0, 200) + "…"
            : evt.result;
          append({
            kind: evt.is_error ? "error" : "tool",
            text: `[${evt.name}] ${evt.is_error ? "error" : "ok"}: ${trimmed}`,
          });
          break;
        }
        case "error":
          append({ kind: "error", text: `error: ${evt.message}` });
          setBusy(false);
          break;
        case "done":
          streamingTextRef.current = "";
          setBusy(false);
          break;
        case "cancelled":
          append({ kind: "info", text: "(cancelled)" });
          streamingTextRef.current = "";
          setBusy(false);
          break;
        default:
          // text_start / message_commit / context_usage / fish_progress / etc.
          // — silent on the CLI surface.
          break;
      }
    });
    unlistenRef.current = unlisten;
  }, [append, updateLastAssistant]);

  const sendCurrent = useCallback(async () => {
    const text = input.trim();
    if (!text || busy) return;
    setInput("");
    append({ kind: "user", text });
    setBusy(true);
    try {
      const sid = await ensureSession();
      // Subscribe lazily on first send (avoids attaching listeners for
      // sessions the user never actually uses).
      if (!unlistenRef.current) await subscribe(sid);
      streamingTextRef.current = "";
      await chatApi.send(sid, text);
    } catch (err) {
      append({ kind: "error", text: `send failed: ${String(err)}` });
      setBusy(false);
    }
  }, [input, busy, append, ensureSession, subscribe]);

  const cancelCurrent = useCallback(async () => {
    const sid = sessionIdRef.current;
    if (!sid) return;
    try { await chatApi.cancel(sid); } catch { /* ignore */ }
  }, []);

  const clearLog = useCallback(() => {
    setLines([]);
  }, []);

  // Auto-focus when shown.
  useEffect(() => {
    if (visible) {
      const t = setTimeout(() => inputRef.current?.focus(), 60);
      return () => clearTimeout(t);
    }
  }, [visible]);

  // Reset when project changes — don't carry conversation across projects.
  useEffect(() => {
    if (unlistenRef.current) { unlistenRef.current(); unlistenRef.current = null; }
    sessionIdRef.current = null;
    setLines([]);
    setBusy(false);
  }, [projectDir]);

  // Cleanup on unmount.
  useEffect(() => {
    return () => {
      if (unlistenRef.current) unlistenRef.current();
    };
  }, []);

  if (!visible) return null;

  return (
    <div className="ide-terminal-panel ide-assistant-panel" style={height ? { height } : undefined}>
      <div className="ide-terminal-header">
        <span className="term-title">{t("ide.assistantTitle") || "Pisci Assistant"}</span>
        <div style={{ flex: 1 }} />
        <button onClick={clearLog} title={t("ide.assistantClear") || "Clear"}>⌫</button>
        {busy && (
          <button onClick={cancelCurrent} title="Cancel">⏹</button>
        )}
        <button onClick={onClose} title="Close">✕</button>
      </div>
      <div className="ide-assistant-body" ref={bodyRef}>
        {lines.length === 0 && (
          <div className="ide-assistant-empty">
            {t("ide.assistantHint") ||
              "Ask Pisci in plain language."}
          </div>
        )}
        {lines.map((line, i) => (
          <div key={i} className={`ide-assistant-line ide-assistant-line--${line.kind}`}>
            {line.kind === "user"
              ? <><span className="ide-assistant-prompt">&gt;</span> {line.text}</>
              : line.text}
          </div>
        ))}
        {busy && <div className="ide-assistant-line ide-assistant-line--info">▍</div>}
      </div>
      <div className="ide-assistant-input-row">
        <span className="ide-assistant-prompt">&gt;</span>
        <textarea
          ref={inputRef}
          className="ide-assistant-input"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              sendCurrent();
            }
          }}
          placeholder={t("ide.assistantInputPlaceholder") || "Ask Pisci..."}
          rows={1}
          disabled={busy}
          spellCheck={false}
        />
        <button
          className="ide-assistant-send"
          onClick={sendCurrent}
          disabled={busy || !input.trim()}
        >
          {t("ide.assistantSend") || "Send"}
        </button>
      </div>
    </div>
  );
}
