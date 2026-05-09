# Changelog

All notable changes to Pisci Desktop are documented here.
This project follows [Semantic Versioning](https://semver.org/) and
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) conventions.

---

## [0.7.18] - 2026-05-07

### Fixed
- **WeChat image attachment "expired or cleared"**: the `image_item` payload
  was missing the `"len"` field (raw file size in bytes). The `file_item`
  already carried `len` and `mid_size` after v0.7.15, but `image_item` only
  had `mid_size`. WeChat clients use `len` to validate the decrypted image
  size before rendering; without it the client shows "image expired or cleared".
  Fixed in [wechat.rs](src-tauri/src/gateway/wechat.rs) `build_media_message_item`.

---

## [0.7.17] - 2026-05-07

### Fixed
- **IM agent repeats stale reply / context never updates (clock-skew bug)**:
  the message store ordered conversation history with `ORDER BY created_at DESC`.
  When the system clock briefly jumped forward (timezone confusion or NTP
  correction), messages persisted during that window received future-dated
  timestamps. After the clock returned to normal, every newly-inserted user
  message had an *earlier* `created_at` than those stuck messages, so SQL
  sorting permanently kept the stale future-dated turn at the "latest"
  position. This caused two cascading failures in IM sessions:
  1. The `already_inserted` dedup check (which compares against the latest
     row by `created_at`) always saw a stuck assistant message and re-inserted
     the same user message twice per turn.
  2. `build_session_message_context_from_db` loaded conversation history with
     the new user message ranked older than the stuck turn, so the agent
     received a context where the latest user message was an empty
     tool-results carrier — and replied by repeating the same stale assistant
     turn no matter what the user actually wrote.
  
  Fixed by switching `get_messages_latest`, `get_messages`, and
  `get_messages_older` in [db.rs](src-tauri/pisci-kernel/src/store/db.rs) to
  sort by `rowid` (SQLite insert order) instead of `created_at`. `rowid` is
  monotonically increasing and immune to clock drift, so insert order is the
  source of truth for "newest message" regardless of system time anomalies.

---

## [0.7.16] - 2026-05-07

### Fixed
- **WeChat duplicate agent runs (queue race condition)**: the gateway-level
  dedup cache (`WechatState::seen_messages`) prevents most duplicate message
  deliveries from iLink, but when iLink re-delivers the same `message_id`
  very rapidly (within the same `getupdates` batch or within milliseconds),
  multiple copies can slip through before the first one is marked. These
  duplicates then enter the IM message queue and are processed sequentially
  by the queue drain loop, causing the agent to run multiple times with the
  same context and emit identical replies.
  
  Added a second layer of defense: the queue-mode processing task now
  tracks processed message IDs in a local `HashSet<String>` and skips any
  queued message whose ID has already been processed in the current session
  run. This catches duplicates that bypassed the gateway dedup due to timing.

---

## [0.7.15] - 2026-05-07

### Fixed
- **WeChat file attachments cannot be opened ("文件过大")**: the iLink file
  message payload was missing the `mid_size` (encrypted file size) field and
  sent `len` as a string instead of a number. WeChat clients need `mid_size`
  to properly download and decrypt the file. Fixed:
  - Change `"len"` from `String` to `Number` (`uploaded.raw_size`)
  - Add `"mid_size"` field (`uploaded.encrypted_size`) matching the
    `image_item` structure

### Changed
- **Scheduled task conversation persistence**: agent conversations from
  scheduled tasks are now saved to the database under session
  `sched_{task_id}` so users can inspect what the agent did (e.g. whether
  `im_send_message` was called and whether it succeeded).
- **Scheduled task diagnostic logging**: the final assistant message is now
  logged (first 200 chars) to help diagnose silent failures where the agent
  ran but did not produce expected results.
- **Scheduled task API key error handling**: missing API key now emits an
  error event and updates the task run status to "error" instead of silently
  skipping.

---

## [0.7.14] - 2026-05-07

### Fixed
- **Linux GTK `<select>` dropdown unreadable text**: on Linux (Webkit2GTK),
  native `<select>` rendering forces a light background, making the light
  `--text-primary` color invisible. Applied `appearance: none` + custom
  chevron SVG to `.board-filter-select` (the three filter dropdowns on the
  Pond Kanban board toolbar) so it respects the dark theme colors, and
  added explicit `option` styling.

---

## [0.7.13] - 2026-05-07

### Fixed
- **WeChat IM duplicate replies**: the iLink gateway now deduplicates inbound
  messages by `message_id` (5-minute TTL) so the agent is not re-run on
  re-deliveries from `getupdates`. Previously, when iLink replayed the same
  `message_id` after a transient network/cursor hiccup, the agent would run
  again with an essentially identical context and emit the same reply to the
  user — making it look like the bot was stuck in a loop, replying with the
  same text regardless of what the user asked next.
- **Stable `message_id` parsing**: `weixin_message_to_inbound` now accepts
  both numeric and string forms of `message_id` (iLink has been observed to
  serialize it as a string in some payload variants). Falling back to a
  random UUID for a known id would have defeated the new dedup cache, so
  the UUID fallback is now reserved only for payloads that truly carry no id.

### Added
- `WechatState::seen_messages` cache and `mark_message_fresh` helper, wired
  into both the direct long-poll path (`listen_ilink_updates`) and the
  local HTTP plugin fallback (`handle_getupdates`).
- Tests `mark_message_fresh_rejects_duplicate_ids` and
  `extracts_stable_msg_id_from_string_form_message_id`.

---

## [0.7.12] - 2026-05-07

### Fixed
- **WeChat voice messages now deliver ASR transcript**: iLink's `getupdates`
  payload carries a server-side transcript in `voice_item.text`. Previously
  the WeChat gateway discarded this field and handed the agent a fake
  `wechat_voice_<id>.bin` filename + `wechat://message/<id>` URL with no
  bytes, causing the agent to waste turns trying to `find` / `ls` a file
  that was never written to disk. The gateway now inlines the transcript
  as `[语音消息] <transcript>` and skips the media placeholder entirely
  when a transcript is present.
  (Non-WeChat IM channels have not been audited for the same defect and
  are deferred to a later release.)

### Added
- Helper `extract_wechat_voice_text` in `gateway/wechat.rs` that pulls
  `voice_item.text` / `audio_item.text` / `speech_item.text` defensively.
- Test `inlines_wechat_voice_transcript_when_provided` covering the new
  transcript path; renamed the legacy test to
  `preserves_wechat_voice_message_placeholder_when_no_transcript`.

---

## [0.7.11] - 2026-05-05

### Fixed
- **WeChat IM duplicate reply**: fixed reply routing in `run_im_agent_and_send_reply` to use
  `msg.reply_target` directly instead of the overridden DB binding, preventing cascading
  reply-target conflicts when multiple IM messages arrive in quick succession.
- **WeChat IM message ordering**: fixed `im_session_updated` handler to clear `frozenBubble`
  before starting a new agent run, preventing stale collapsed bubbles from a previous turn
  from appearing in the middle of the message list when displayed via
  `setMessagesWithFrozen`.

---

## [0.7.10] - 2026-05-05

### Fixed
- **CI**: cross-platform compilation and clippy fixes.
- **Loop detection**: raised loop-detection thresholds to prevent premature tool blocking.

### Changed
- `desktop_automation` / `uia` code formatting (`cargo fmt`).
- `system_info` platform-specific refactoring.

---

## [0.7.9] - 2026-05-05

### Fixed
- **UIA precision drag test coordinate accuracy**: the agent now receives exact
  ball/target physical-screen coordinates from the frontend via IPC (computed
  from `innerPosition()` + `getBoundingClientRect()` × `devicePixelRatio`).
  The drag is executed in a single `desktop_automation` / `uia` tool call with
  no screenshot, no OCR, no grid estimation. Vision-based fallback retained.
- **UIA test layout stability**: arena is now fixed-width (800px) and centered;
  tool-call live log and result panel are width-contained
  (`overflow-x:hidden`, `box-sizing:border-box`) so they cannot shift the arena's
  screen position during a running test.
- **Linux (VMware+Xorg) mouse control**: new `xi_helpers.c` native helper
  (`pisci-xi-helper`) uses `XIWarpPointer` on the master pointer (device id=2)
  plus `XTestFakeMotionEvent` to deliver events reliably. `move_mouse` /
  `drag` now execute a 20-step smooth motion matching Windows UIA behavior,
  and events reach WebKit correctly even though the visible cursor stays put
  under VMware.
- **IM send auto-resolve**: `im_send_message` now automatically resolves the
  IM binding from the current `session_id` when no explicit `binding_key` or
  `channel`+`recipient` is provided, so IM-driven replies don't need explicit
  addressing parameters.
- Minor borrow fix in `pisci-kernel::agent::loop_` cancellation path.

### Changed
- `screen_capture` default `grid_spacing` is now 100 (was 200); label interval
  auto-adjusts to every 2nd line when spacing is under 200px to avoid overlap.
- Ball and target in the UIA test panel display screen-absolute coordinate
  labels for debugging and verification.

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
