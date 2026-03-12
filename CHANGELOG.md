# Changelog

All notable changes to Pisci Desktop are documented here.
This project follows [Semantic Versioning](https://semver.org/) and
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) conventions.

---

## [Unreleased]

### Documentation
- **Multi-agent architecture docs**: README (Chinese and English) now explains the
  roles and boundaries of Pisci, Koi, and Fish, plus the structure of the Pond
  workspace and the collaboration lifecycle.

### Changed
- **Heartbeat guardrails**: Pisci heartbeat now treats follow-up signals without
  active todos as a coordination stall, and no longer treats "no todo" as
  sufficient evidence to emit `HEARTBEAT_OK`.
- **Multi-agent verification**: collaboration regressions are now covered by the
  expanded in-app multi-agent integration suite, including heartbeat guardrails
  and stale-state recovery cases.

### Added
- **Skill installation**: Install community Anthropic-spec skills from URLs or
  local paths; `install_skill` / `uninstall_skill` Tauri commands.
- **IM Gateway expansion**: Slack, Discord, Microsoft Teams, Matrix, and generic
  webhook outbound channels with a unified `Channel` trait.
- **WeCom local-relay inbox**: poll a local JSONL file written by an external
  relay service for inbound WeCom messages.
- **Email tooling**: `smtp_send`, `imap_fetch`, `imap_search` via `lettre` and
  the `imap` crate.
- **Agent checkpoints**: persist agent loop state (messages + iteration) to
  SQLite after every step; automatically resume from the last checkpoint on
  crash.
- **Vector + hybrid memory search**: cosine similarity, FTS5 keyword search, and
  a weighted hybrid merge.
- **Policy Gate enhancements**: `PolicyMode` (Strict / Balanced / Dev), redact
  secrets in audit logs, rate-limit field.
- **Prompt-injection detection v2**: encoding-bypass detection (Base64, ROT-13,
  Unicode zero-width), density heuristic, per-pattern risk score, severity
  buckets.
- **Scheduled task status**: real-time `running` / `success` / `failed` badges
  in the Scheduler UI, Tauri events `task_status_<id>`, retry logic with
  exponential back-off.
- **Browser download management**: `download_file`, `list_downloads`,
  `wait_download` CDP-based tools.
- **Auto-updater**: `tauri-plugin-updater` + `tauri-plugin-process` wired up;
  update endpoint configurable in `tauri.conf.json`.
- **CI pipeline**: `.github/workflows/ci.yml` — lint → test → build → package.
- **Release gate**: `scripts/smoke-test.ps1` runs all checks locally before
  shipping.
- **Frontend tests**: vitest + happy-dom test suite covering all `tauri.ts` API
  methods (22 tests).
- **Rust unit tests**: 29 tests across `policy/gate`, `security/injection`,
  `memory/vector`.

### Changed
- `ScheduledTask` struct now includes `last_run_status`.
- `PolicyGate::check_user_input` integrates injection scoring.
- Scheduler `execute_task` emits Tauri events and retries up to 3 times.
- `browser.rs` replaced `unwrap()` serialisation calls with safe `js_str`
  helper.
- `web_search.rs` replaced `Selector::parse(...).unwrap()` with error
  propagation.

### Fixed
- `cargo check` ownership error in concurrent read-only tool batching.
- `mailparse` header API usage in `email.rs`.
- Regex raw-string literals in `policy/gate.rs` (unknown-token compile error).

---

## [0.1.0] — 2025-12-01

### Added
- Initial Tauri 2 scaffold (React + TypeScript frontend, Rust backend).
- Agent loop with Claude / OpenAI / DeepSeek / Qwen LLM backends.
- Core Windows tooling: PowerShell, UIA, COM, screen capture, DPI helpers.
- Browser automation via CDP (`chromiumoxide`).
- SQLite store (sessions, messages, memories, scheduled tasks, audit log).
- Cron scheduler with `tokio-cron-scheduler`.
- Basic skills loader (`SKILL.md` YAML frontmatter).
- IM gateways: Feishu, WeCom, DingTalk, Telegram (outbound + polling).
- Settings UI with per-provider API key management.
- Tray icon and system-notification support.
