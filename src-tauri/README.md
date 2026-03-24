# OpenPisci — Tauri 后端（Rust）

本目录为 **Tauri 2** 桌面壳与 **Rust** 业务逻辑（Agent、工具、存储、LLM 客户端等）。产品说明、架构与功能请见仓库根目录 [**README.md**](../README.md)；英文见 [**README_EN.md**](../README_EN.md)。

## 环境要求

- **Rust**（stable，`rustup` 安装）
- 前端与完整构建还需 **Node.js 20+**（在仓库根目录执行 `npm ci`）

## 常用命令（在仓库根目录执行）

| 命令 | 说明 |
|------|------|
| `npm run tauri dev` | 开发模式：Vite + `cargo run` |
| `npm run tauri build` | 生产构建：前端打包 + 安装包（NSIS/MSI 等） |
| `cd src-tauri && cargo test --lib` | 仅运行 Rust 单元测试 |
| `cd src-tauri && cargo clippy --lib -- -D warnings` | Clippy（与 CI 一致） |

## 目录结构（概要）

| 路径 | 说明 |
|------|------|
| `src/main.rs` | 入口、`tauri::Builder`、插件注册 |
| `src/lib.rs` | 库入口、命令与状态注册 |
| `src/agent/` | Agent 循环、工具上下文、压缩与 checkpoint |
| `src/commands/` | Tauri 命令（聊天、会话、设置等） |
| `src/tools/` | 内置工具（shell、浏览器、Koi/Fish 调用等） |
| `src/llm/` | LLM 客户端与 token 估算 |
| `src/store/` | SQLite、设置与会话存储 |
| `Cargo.toml` / `tauri.conf.json` | 依赖与 Tauri 配置 |

## CI

GitHub Actions 工作流见 [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)（前端检查、Rust `fmt`/`clippy`/测试、Windows 上 Tauri 打包等）。
