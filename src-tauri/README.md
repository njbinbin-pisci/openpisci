# OpenPisci - Tauri Backend (Rust)

This directory contains the **Tauri 2** desktop shell and the **Rust** application logic (Agent runtime, tools, storage, LLM clients, and more). For product overview, architecture, and feature documentation, see the root Chinese doc [**README_CN.md**](../README_CN.md); for the root English doc, see [**README.md**](../README.md).

[中文](./README_CN.md) | English

## Requirements

- **Rust** (stable, install via `rustup`)
- **Node.js 20+** is also required for the frontend and full build flow (run `npm ci` at the repository root)

## Common Commands (run from repository root)

| Command | Description |
|------|------|
| `npm run tauri dev` | Development mode: Vite + `cargo run` |
| `npm run tauri build` | Production build: frontend bundle + installers (NSIS/MSI, etc.) |
| `cd src-tauri && cargo test --lib` | Run Rust unit tests only |
| `cd src-tauri && cargo clippy --lib -- -D warnings` | Run Clippy checks (same baseline as CI) |

## Directory Overview

| Path | Description |
|------|------|
| `src/main.rs` | Entrypoint, `tauri::Builder`, plugin registration |
| `src/lib.rs` | Library entry, command and app-state registration |
| `src/agent/` | Agent loop, tool context, compaction, checkpoints |
| `src/commands/` | Tauri commands (chat, sessions, settings, etc.) |
| `src/tools/` | Built-in tools (shell, browser, Koi/Fish invocation, etc.) |
| `src/llm/` | LLM clients and token estimation |
| `src/store/` | SQLite, settings, and session persistence |
| `Cargo.toml` / `tauri.conf.json` | Dependencies and Tauri configuration |

## CI

See the GitHub Actions workflow at [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) for frontend checks, Rust `fmt` / `clippy` / tests, and Windows Tauri packaging.
