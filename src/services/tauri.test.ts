import { describe, it, expect, vi, beforeEach } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { settingsApi, sessionsApi, chatApi, skillsApi, schedulerApi, memoryApi, auditApi } from "./tauri";

const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockInvoke.mockReset();
});

// ─── settingsApi ──────────────────────────────────────────────────────────────

describe("settingsApi", () => {
  it("get() calls get_settings", async () => {
    const fakeSettings = { provider: "openai", model: "gpt-4o", language: "zh" };
    mockInvoke.mockResolvedValueOnce(fakeSettings);
    const result = await settingsApi.get();
    expect(mockInvoke).toHaveBeenCalledWith("get_settings");
    expect(result).toEqual(fakeSettings);
  });

  it("save() calls save_settings with updates", async () => {
    const updates = { provider: "anthropic" };
    mockInvoke.mockResolvedValueOnce({ ...updates });
    await settingsApi.save(updates);
    expect(mockInvoke).toHaveBeenCalledWith("save_settings", { updates });
  });

  it("isConfigured() calls is_configured", async () => {
    mockInvoke.mockResolvedValueOnce(true);
    const result = await settingsApi.isConfigured();
    expect(mockInvoke).toHaveBeenCalledWith("is_configured");
    expect(result).toBe(true);
  });
});

// ─── sessionsApi ──────────────────────────────────────────────────────────────

describe("sessionsApi", () => {
  it("list() calls list_sessions with pagination defaults", async () => {
    mockInvoke.mockResolvedValueOnce({ sessions: [], total: 0 });
    await sessionsApi.list();
    expect(mockInvoke).toHaveBeenCalledWith("list_sessions", { limit: 20, offset: 0 });
  });

  it("create() calls create_session with title", async () => {
    mockInvoke.mockResolvedValueOnce({ id: "abc", title: "Test" });
    await sessionsApi.create("Test");
    expect(mockInvoke).toHaveBeenCalledWith("create_session", { title: "Test" });
  });

  it("delete() calls delete_session with sessionId", async () => {
    mockInvoke.mockResolvedValueOnce(null);
    await sessionsApi.delete("abc");
    expect(mockInvoke).toHaveBeenCalledWith("delete_session", { sessionId: "abc" });
  });

  it("rename() calls rename_session", async () => {
    mockInvoke.mockResolvedValueOnce(null);
    await sessionsApi.rename("abc", "New Title");
    expect(mockInvoke).toHaveBeenCalledWith("rename_session", { sessionId: "abc", title: "New Title" });
  });
});

// ─── chatApi ──────────────────────────────────────────────────────────────────

describe("chatApi", () => {
  it("send() calls chat_send with sessionId and content", async () => {
    mockInvoke.mockResolvedValueOnce(null);
    await chatApi.send("sess1", "hello");
    expect(mockInvoke).toHaveBeenCalledWith("chat_send", {
      sessionId: "sess1",
      content: "hello",
      attachment: null,
      clearPlan: true,
    });
  });

  it("cancel() calls chat_cancel with sessionId", async () => {
    mockInvoke.mockResolvedValueOnce(null);
    await chatApi.cancel("sess1");
    expect(mockInvoke).toHaveBeenCalledWith("chat_cancel", { sessionId: "sess1" });
  });
});

// ─── memoryApi ────────────────────────────────────────────────────────────────

describe("memoryApi", () => {
  it("list() calls list_memories", async () => {
    mockInvoke.mockResolvedValueOnce({ memories: [], total: 0 });
    await memoryApi.list();
    expect(mockInvoke).toHaveBeenCalledWith("list_memories");
  });

  it("add() calls add_memory with content", async () => {
    mockInvoke.mockResolvedValueOnce({ id: "m1" });
    await memoryApi.add("remember this");
    expect(mockInvoke).toHaveBeenCalledWith("add_memory", {
      content: "remember this",
      category: undefined,
      confidence: undefined,
    });
  });

  it("delete() calls delete_memory", async () => {
    mockInvoke.mockResolvedValueOnce(null);
    await memoryApi.delete("m1");
    expect(mockInvoke).toHaveBeenCalledWith("delete_memory", { memoryId: "m1" });
  });
});

// ─── skillsApi ────────────────────────────────────────────────────────────────

describe("skillsApi", () => {
  it("list() calls list_skills", async () => {
    mockInvoke.mockResolvedValueOnce({ skills: [], total: 0 });
    await skillsApi.list();
    expect(mockInvoke).toHaveBeenCalledWith("list_skills");
  });

  it("install() calls install_skill with source", async () => {
    mockInvoke.mockResolvedValueOnce({ name: "my-skill" });
    await skillsApi.install("https://example.com/SKILL.md");
    expect(mockInvoke).toHaveBeenCalledWith("install_skill", {
      source: "https://example.com/SKILL.md",
    });
  });

  it("uninstall() calls uninstall_skill with skillName", async () => {
    mockInvoke.mockResolvedValueOnce(null);
    await skillsApi.uninstall("my-skill");
    expect(mockInvoke).toHaveBeenCalledWith("uninstall_skill", { skillName: "my-skill" });
  });

  it("catalog() calls scan_skill_catalog", async () => {
    mockInvoke.mockResolvedValueOnce([]);
    await skillsApi.catalog();
    expect(mockInvoke).toHaveBeenCalledWith("scan_skill_catalog");
  });
});

// ─── schedulerApi ─────────────────────────────────────────────────────────────

describe("schedulerApi", () => {
  it("list() calls list_tasks", async () => {
    mockInvoke.mockResolvedValueOnce({ tasks: [], total: 0 });
    await schedulerApi.list();
    expect(mockInvoke).toHaveBeenCalledWith("list_tasks");
  });

  it("create() calls create_task with params", async () => {
    const params = {
      name: "nightly",
      description: "desc",
      cron_expression: "0 0 * * *",
      task_prompt: "do it",
    };
    mockInvoke.mockResolvedValueOnce({ id: "t1", ...params });
    await schedulerApi.create(params);
    expect(mockInvoke).toHaveBeenCalledWith("create_task", params);
  });

  it("runNow() calls run_task_now with taskId", async () => {
    mockInvoke.mockResolvedValueOnce("ok");
    await schedulerApi.runNow("t1");
    expect(mockInvoke).toHaveBeenCalledWith("run_task_now", { taskId: "t1" });
  });

  it("delete() calls delete_task with taskId", async () => {
    mockInvoke.mockResolvedValueOnce(null);
    await schedulerApi.delete("t1");
    expect(mockInvoke).toHaveBeenCalledWith("delete_task", { taskId: "t1" });
  });
});

// ─── auditApi ─────────────────────────────────────────────────────────────────

describe("auditApi", () => {
  it("list() calls get_audit_log with defaults", async () => {
    mockInvoke.mockResolvedValueOnce([]);
    await auditApi.list();
    expect(mockInvoke).toHaveBeenCalledWith("get_audit_log", {
      sessionId: undefined,
      toolName: undefined,
      limit: 50,
      offset: 0,
    });
  });

  it("clear() calls clear_audit_log", async () => {
    mockInvoke.mockResolvedValueOnce(null);
    await auditApi.clear("sess1");
    expect(mockInvoke).toHaveBeenCalledWith("clear_audit_log", { sessionId: "sess1" });
  });
});
