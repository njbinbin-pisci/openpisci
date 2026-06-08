/**
 * Piscis CLI-style assistant panel — a lightweight, terminal-shaped chat
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
 *   what Piscis is actually doing under the hood.
 * - `chat_ui` / permission prompts render inline (not the main Chat tab)
 *   so blocking tools do not hang waiting for a UI that never appears.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useDispatch, useSelector } from "react-redux";
import { sessionsApi, chatApi, type AgentEventType } from "../../../services/tauri/chat";
import { permissionApi } from "../../../services/tauri/platform";
import { RootState, sessionsActions } from "../../../store";
import { isPondCliSession } from "../../../utils/session";
import {
  handleInputHistoryKeyDown,
  pushInputHistory,
  resetInputHistoryNav,
} from "../../../utils/inputHistory";
import InteractiveCard from "../../Chat/InteractiveCard";
import { applyUiPatch } from "../../Chat/interactiveUi/patch";
import type { UiDefinition, UiPatch } from "../../Chat/interactiveUi/protocol";

interface AssistantPanelProps {
  projectDir: string | null;
  visible: boolean;
  onClose: () => void;
  height?: number;
  /** Refresh explorer / git when agent mutates workspace files. */
  onWorkspaceFilesChanged?: () => void;
}

interface CliLine {
  /** "user" | "assistant" | "tool" | "error" | "info" */
  kind: "user" | "assistant" | "tool" | "error" | "info";
  text: string;
}

interface CliInteractiveCard {
  requestId: string;
  uiDefinition: UiDefinition;
  listenOpen: boolean;
  wizardStepHint?: number;
}

interface PermissionRequest {
  requestId: string;
  toolName: string;
  toolInput: unknown;
  description: string;
}

const FILE_MUTATION_TOOLS = new Set(["file_write", "file_edit"]);

export default function AssistantPanel({
  projectDir,
  visible,
  onClose,
  height,
  onWorkspaceFilesChanged,
}: AssistantPanelProps) {
  const { t } = useTranslation();
  const dispatch = useDispatch();
  const storeSessions = useSelector((s: RootState) => s.sessions.sessions);
  const [lines, setLines] = useState<CliLine[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [interactiveCards, setInteractiveCards] = useState<Record<string, CliInteractiveCard>>({});
  const [permissionRequest, setPermissionRequest] = useState<PermissionRequest | null>(null);
  const sessionIdRef = useRef<string | null>(null);
  const unlistenRef = useRef<(() => void) | null>(null);
  const streamingTextRef = useRef<string>("");
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

  const clearInteractiveCards = useCallback(() => {
    setInteractiveCards({});
  }, []);

  const ensureSession = useCallback(async (): Promise<string> => {
    if (sessionIdRef.current) return sessionIdRef.current;

    const title = projectDir
      ? `Piscis CLI — ${projectDir.split(/[\\/]/).pop() ?? projectDir}`
      : "Piscis CLI";

    const matchesProject = (s: { title?: string; workspace_root?: string | null }) => {
      if (s.title === title) return true;
      if (projectDir && s.workspace_root === projectDir) return true;
      return false;
    };

    let existing = storeSessions.find((s) => isPondCliSession(s) && matchesProject(s));
    if (!existing) {
      try {
        const { sessions } = await sessionsApi.list(200, 0);
        existing = sessions.find((s) => isPondCliSession(s) && matchesProject(s));
      } catch { /* ignore */ }
    }

    if (existing) {
      sessionIdRef.current = existing.id;
      dispatch(sessionsActions.upsertSession(existing));
      return existing.id;
    }

    const session = await sessionsApi.create(title, "cli");
    sessionIdRef.current = session.id;
    if (projectDir) {
      try {
        await sessionsApi.setWorkspace(session.id, projectDir);
        session.workspace_root = projectDir;
      } catch { /* ignore */ }
    }
    dispatch(sessionsActions.addSession(session));
    return session.id;
  }, [projectDir, storeSessions, dispatch]);

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
          if (!evt.is_error && FILE_MUTATION_TOOLS.has(evt.name)) {
            onWorkspaceFilesChanged?.();
          }
          break;
        }
        case "error":
          append({ kind: "error", text: `error: ${evt.message}` });
          setBusy(false);
          break;
        case "permission_request":
          setPermissionRequest({
            requestId: evt.request_id,
            toolName: evt.tool_name,
            toolInput: evt.tool_input,
            description: evt.description,
          });
          append({ kind: "info", text: t("ide.assistantPermissionWaiting", { tool: evt.tool_name }) });
          scrollToBottom();
          break;
        case "interactive_ui":
          setInteractiveCards((prev) => ({
            ...prev,
            [evt.request_id]: {
              requestId: evt.request_id,
              uiDefinition: evt.ui_definition as UiDefinition,
              listenOpen: false,
            },
          }));
          append({ kind: "info", text: t("ide.assistantInteractiveWaiting") });
          scrollToBottom();
          break;
        case "interactive_ui_patch": {
          const patch = evt.patch as UiPatch;
          setInteractiveCards((prev) => {
            const card = prev[evt.request_id];
            if (!card) return prev;
            return {
              ...prev,
              [evt.request_id]: {
                ...card,
                uiDefinition: applyUiPatch(card.uiDefinition, patch),
                listenOpen: patch.reopen_submit === true ? true : card.listenOpen,
                wizardStepHint: patch.wizard_step ?? card.wizardStepHint,
              },
            };
          });
          break;
        }
        case "interactive_ui_listen":
          setInteractiveCards((prev) => {
            const card = prev[evt.request_id];
            if (!card) return prev;
            return {
              ...prev,
              [evt.request_id]: { ...card, listenOpen: true },
            };
          });
          break;
        case "done":
          streamingTextRef.current = "";
          setBusy(false);
          setPermissionRequest(null);
          clearInteractiveCards();
          sessionsApi.list(200, 0).then(({ sessions }) => {
            const fresh = sessions.find((s) => s.id === sessionId);
            if (fresh) dispatch(sessionsActions.upsertSession(fresh));
          }).catch(() => {});
          onWorkspaceFilesChanged?.();
          break;
        case "cancelled":
          append({ kind: "info", text: "(cancelled)" });
          streamingTextRef.current = "";
          setBusy(false);
          setPermissionRequest(null);
          clearInteractiveCards();
          break;
        default:
          break;
      }
    });
    unlistenRef.current = unlisten;
  }, [
    append,
    updateLastAssistant,
    clearInteractiveCards,
    dispatch,
    onWorkspaceFilesChanged,
    scrollToBottom,
    t,
  ]);

  const handlePermissionResponse = useCallback(async (approved: boolean) => {
    if (!permissionRequest) return;
    const { requestId } = permissionRequest;
    setPermissionRequest(null);
    try {
      await permissionApi.respond(requestId, approved);
      append({
        kind: "info",
        text: approved
          ? t("ide.assistantPermissionAllowed")
          : t("ide.assistantPermissionDenied"),
      });
    } catch (err) {
      append({ kind: "error", text: `permission response failed: ${String(err)}` });
    }
  }, [permissionRequest, append, t]);

  const cliInputHistoryScope = `cli:${projectDir ?? "global"}`;

  const sendCurrent = useCallback(async () => {
    const text = input.trim();
    if (!text || busy) return;
    pushInputHistory(cliInputHistoryScope, text);
    resetInputHistoryNav(cliInputHistoryScope);
    setInput("");
    append({ kind: "user", text });
    setBusy(true);
    try {
      const sid = await ensureSession();
      if (!unlistenRef.current) await subscribe(sid);
      streamingTextRef.current = "";
      await chatApi.send(sid, text);
    } catch (err) {
      append({ kind: "error", text: `send failed: ${String(err)}` });
      setBusy(false);
    }
  }, [input, busy, append, ensureSession, subscribe, cliInputHistoryScope]);

  const cancelCurrent = useCallback(async () => {
    const sid = sessionIdRef.current;
    if (!sid) return;
    try { await chatApi.cancel(sid); } catch { /* ignore */ }
  }, []);

  const openInMainChat = useCallback(() => {
    dispatch(
      sessionsActions.openMainChatView({
        filter: "cli",
        sessionId: sessionIdRef.current,
      }),
    );
  }, [dispatch]);

  const clearLog = useCallback(() => {
    setLines([]);
    clearInteractiveCards();
    setPermissionRequest(null);
  }, [clearInteractiveCards]);

  useEffect(() => {
    if (visible) {
      const timer = setTimeout(() => inputRef.current?.focus(), 60);
      return () => clearTimeout(timer);
    }
  }, [visible]);

  useEffect(() => {
    if (unlistenRef.current) { unlistenRef.current(); unlistenRef.current = null; }
    sessionIdRef.current = null;
    setLines([]);
    setBusy(false);
    clearInteractiveCards();
    setPermissionRequest(null);
  }, [projectDir, clearInteractiveCards]);

  useEffect(() => {
    return () => {
      if (unlistenRef.current) unlistenRef.current();
    };
  }, []);

  if (!visible) return null;

  const pendingCards = Object.values(interactiveCards);

  return (
    <div className="ide-terminal-panel ide-assistant-panel" style={height ? { height } : undefined}>
      <div className="ide-terminal-header">
        <span className="term-title">{t("ide.assistantTitle") || "Piscis Assistant"}</span>
        <div style={{ flex: 1 }} />
        <button
          className="ide-assistant-open-main"
          onClick={openInMainChat}
          title={t("ide.assistantOpenMainChat")}
        >
          {t("ide.assistantOpenMainChat")}
        </button>
        <button onClick={clearLog} title={t("ide.assistantClear") || "Clear"}>⌫</button>
        {busy && (
          <button onClick={cancelCurrent} title={t("ide.cancel")}>⏹</button>
        )}
        <button onClick={onClose} title={t("ide.closePanel")}>✕</button>
      </div>
      <div className="ide-assistant-body" ref={bodyRef}>
        {lines.length === 0 && pendingCards.length === 0 && !permissionRequest && (
          <div className="ide-assistant-empty">
            {t("ide.assistantHint") || "Ask Piscis in plain language."}
          </div>
        )}
        {lines.map((line, i) => (
          <div key={i} className={`ide-assistant-line ide-assistant-line--${line.kind}`}>
            {line.kind === "user"
              ? <><span className="ide-assistant-prompt">&gt;</span> {line.text}</>
              : line.text}
          </div>
        ))}
        {permissionRequest && (
          <div className="ide-assistant-permission">
            <div className="ide-assistant-permission-title">{t("chat.permissionTitle")}</div>
            <div className="ide-assistant-permission-desc">{permissionRequest.description}</div>
            <div className="ide-assistant-permission-actions">
              <button type="button" className="btn-deny" onClick={() => handlePermissionResponse(false)}>
                {t("chat.permissionDeny")}
              </button>
              <button type="button" className="btn-allow" onClick={() => handlePermissionResponse(true)}>
                {t("chat.permissionAllow")}
              </button>
            </div>
          </div>
        )}
        {pendingCards.map((card) => (
          <div key={card.requestId} className="ide-assistant-interactive">
            <InteractiveCard
              requestId={card.requestId}
              uiDefinition={card.uiDefinition}
              listenOpen={card.listenOpen}
              wizardStepHint={card.wizardStepHint}
              onSubmitted={() => {
                setInteractiveCards((prev) => {
                  const next = { ...prev };
                  delete next[card.requestId];
                  return next;
                });
                append({ kind: "info", text: t("ide.assistantInteractiveSubmitted") });
              }}
              onActionSent={() => {
                setInteractiveCards((prev) => {
                  const c = prev[card.requestId];
                  if (!c) return prev;
                  return {
                    ...prev,
                    [card.requestId]: { ...c, listenOpen: false },
                  };
                });
              }}
            />
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
            if (handleInputHistoryKeyDown(e, cliInputHistoryScope, setInput)) return;
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              resetInputHistoryNav(cliInputHistoryScope);
              sendCurrent();
            }
          }}
          placeholder={t("ide.assistantInputPlaceholder") || "Ask Piscis..."}
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
