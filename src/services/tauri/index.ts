/**
 * Tauri IPC service barrel.
 *
 * This file replaced the old monolithic `src/services/tauri.ts`. Each
 * domain now lives in its own sibling file and mirrors the Rust-side
 * `src-tauri/src/commands/{chat,pool,config,platform}` directories:
 *
 *   - `./chat`     — sessions, chat turns, scheduler, IM gateway, WeChat
 *                    login, Fish listing, collaboration-trial harness
 *   - `./pool`     — Koi, Chat Pool, Board, and `PoolEvent` stream
 *   - `./config`   — settings, memory, skills (+ ClawHub), user tools,
 *                    builtin tools, MCP servers, audit log
 *   - `./platform` — runtime/VM probes, window/overlay, permission &
 *                    interactive prompts, openPath
 *
 * All consumers keep importing from `"../services/tauri"` — module
 * resolution picks up this `index.ts` automatically, so no call sites
 * had to change when the file was expanded into a directory.
 */

export * from "./chat";
export * from "./pool";
export * from "./config";
export * from "./platform";
