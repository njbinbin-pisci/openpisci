import { useEffect, useRef, useState, useCallback } from "react";
import { useDispatch, useSelector } from "react-redux";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { RootState, chatActions, sessionsActions } from "../../store";
import { chatApi, sessionsApi, AgentEventType } from "../../services/tauri";
import "./Chat.css";

export default function Chat() {
  const dispatch = useDispatch();
  const { sessions, activeSessionId } = useSelector((s: RootState) => s.sessions);
  const { messagesBySession, streamingText, activeTools, isRunning } = useSelector(
    (s: RootState) => s.chat
  );

  const [input, setInput] = useState("");
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const unlistenRef = useRef<UnlistenFn | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const activeMessages = activeSessionId ? messagesBySession[activeSessionId] ?? [] : [];
  const streamingContent = activeSessionId ? streamingText[activeSessionId] ?? "" : "";
  const running = activeSessionId ? isRunning[activeSessionId] ?? false : false;
  const tools = activeSessionId ? activeTools[activeSessionId] ?? [] : [];

  // Load messages when session changes
  useEffect(() => {
    if (!activeSessionId) return;
    sessionsApi.getMessages(activeSessionId).then((messages) => {
      dispatch(chatActions.setMessages({ sessionId: activeSessionId, messages }));
    });
  }, [activeSessionId, dispatch]);

  // Subscribe to agent events
  useEffect(() => {
    if (!activeSessionId) return;

    // Cleanup previous listener
    if (unlistenRef.current) {
      unlistenRef.current();
      unlistenRef.current = null;
    }

    chatApi.onEvent(activeSessionId, (event: AgentEventType) => {
      if (!activeSessionId) return;
      switch (event.type) {
        case "text_delta":
          dispatch(chatActions.appendDelta({ sessionId: activeSessionId, delta: event.delta }));
          break;
        case "tool_start":
          dispatch(chatActions.setToolStart({ sessionId: activeSessionId, ...event }));
          break;
        case "tool_end":
          dispatch(chatActions.removeActiveTool({ sessionId: activeSessionId, id: event.id }));
          break;
        case "done":
          dispatch(chatActions.setRunning({ sessionId: activeSessionId, running: false }));
          // Reload messages to get the saved assistant message
          sessionsApi.getMessages(activeSessionId).then((messages) => {
            dispatch(chatActions.setMessages({ sessionId: activeSessionId, messages }));
            dispatch(chatActions.clearStreaming(activeSessionId));
          });
          break;
        case "error":
          dispatch(chatActions.setRunning({ sessionId: activeSessionId, running: false }));
          dispatch(chatActions.clearStreaming(activeSessionId));
          break;
      }
    }).then((unlisten) => {
      unlistenRef.current = unlisten;
    });

    return () => {
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
    };
  }, [activeSessionId, dispatch]);

  // Auto-scroll
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [activeMessages, streamingContent]);

  const handleNewSession = useCallback(async () => {
    const session = await sessionsApi.create("New Chat");
    dispatch(sessionsActions.addSession(session));
    dispatch(sessionsActions.setActiveSession(session.id));
  }, [dispatch]);

  const handleSend = useCallback(async () => {
    if (!input.trim() || !activeSessionId || running) return;

    const content = input.trim();
    setInput("");

    // Optimistically add user message
    dispatch(chatActions.appendMessage({
      sessionId: activeSessionId,
      message: {
        id: Date.now().toString(),
        session_id: activeSessionId,
        role: "user",
        content,
        created_at: new Date().toISOString(),
      },
    }));

    dispatch(chatActions.setRunning({ sessionId: activeSessionId, running: true }));

    try {
      await chatApi.send(activeSessionId, content);
    } catch (e) {
      dispatch(chatActions.setRunning({ sessionId: activeSessionId, running: false }));
      dispatch(chatActions.clearStreaming(activeSessionId));
    }
  }, [input, activeSessionId, running, dispatch]);

  const handleCancel = useCallback(() => {
    if (activeSessionId) {
      chatApi.cancel(activeSessionId);
    }
  }, [activeSessionId]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  return (
    <div className="chat-layout">
      {/* Session sidebar */}
      <div className="session-list">
        <div className="session-list-header">
          <span>Chats</span>
          <button className="btn-icon" onClick={handleNewSession} title="New chat">+</button>
        </div>
        {sessions.map((s) => (
          <button
            key={s.id}
            className={`session-item ${s.id === activeSessionId ? "active" : ""}`}
            onClick={() => dispatch(sessionsActions.setActiveSession(s.id))}
          >
            <span className="session-title">{s.title ?? "Chat"}</span>
            <span className="session-count">{s.message_count}</span>
          </button>
        ))}
        {sessions.length === 0 && (
          <div className="session-empty">No chats yet</div>
        )}
      </div>

      {/* Main chat area */}
      <div className="chat-main">
        {activeSessionId ? (
          <>
            <div className="messages-area">
              {activeMessages.map((msg) => (
                <div key={msg.id} className={`message message-${msg.role}`}>
                  <div className="message-role">
                    {msg.role === "user" ? "You" : "Pisci"}
                  </div>
                  <div className="message-content">
                    <MessageContent content={msg.content} />
                  </div>
                </div>
              ))}

              {/* Active tools */}
              {tools.map((tool) => (
                <div key={tool.id} className="tool-call">
                  <span className="tool-icon">⚙️</span>
                  <span className="tool-name">{tool.name}</span>
                  <span className="tool-running">running...</span>
                </div>
              ))}

              {/* Streaming text */}
              {streamingContent && (
                <div className="message message-assistant">
                  <div className="message-role">Pisci</div>
                  <div className="message-content">
                    <MessageContent content={streamingContent} />
                    <span className="cursor-blink">▋</span>
                  </div>
                </div>
              )}

              <div ref={messagesEndRef} />
            </div>

            <div className="input-area">
              <textarea
                ref={textareaRef}
                className="chat-input"
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={handleKeyDown}
                placeholder="Message Pisci... (Enter to send, Shift+Enter for new line)"
                rows={3}
                disabled={running}
              />
              <div className="input-actions">
                {running ? (
                  <button className="btn btn-danger" onClick={handleCancel}>
                    ⏹ Stop
                  </button>
                ) : (
                  <button
                    className="btn btn-primary"
                    onClick={handleSend}
                    disabled={!input.trim()}
                  >
                    Send ↵
                  </button>
                )}
              </div>
            </div>
          </>
        ) : (
          <div className="empty-state">
            <div className="empty-state-icon">🐟</div>
            <div className="empty-state-title">Welcome to Pisci</div>
            <div className="empty-state-desc">
              Start a new chat to begin working with your AI assistant
            </div>
            <button className="btn btn-primary" onClick={handleNewSession}>
              New Chat
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

function MessageContent({ content }: { content: string }) {
  // Simple markdown-like rendering
  const lines = content.split("\n");
  return (
    <>
      {lines.map((line, i) => {
        if (line.startsWith("```")) {
          return null; // handled below
        }
        return (
          <span key={i}>
            {line}
            {i < lines.length - 1 && <br />}
          </span>
        );
      })}
    </>
  );
}
