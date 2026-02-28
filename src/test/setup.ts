import "@testing-library/jest-dom";
import { vi } from "vitest";

// Mock the @tauri-apps/api modules so tests can run in jsdom without a Tauri
// runtime context.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
  emit: vi.fn(),
}));
