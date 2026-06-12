# Changelog

All notable changes to Piscis Desktop are documented here.
This project follows [Semantic Versioning](https://semver.org/) and
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) conventions.

---

## [0.8.64] - 2026-06-12

### Changed

- **RobotZ `v0.1.2`**: pin `robotz-automation` / `robotz-browser` by tag (was `master` branch).
- **Browser tool**: snapshot/ref E2E actions, assert/wait DSL, session lock — shared with AgentZ via RobotZ crate.

---

## [0.8.63] - 2026-06-12

### Added
- **Pond IDE source control**: create branch, checkout branch (dirty-worktree confirmation), themed `PromptDialog` and `ConfirmDialog` alert mode.
- **Chat / Overlay**: `web_fetch` tool icon and URL summary in tool-call cards.

### Fixed
- **Git panel**: stage-all / unstage-all / unstage errors now use themed alerts (same as single-file stage).

---

## [0.8.62] - 2026-06-12

### Added
- **Pond inbox / Koi observer**: tool-call cards with full input/output (replaces `🔧 pool_org` / `🔧 tool result` placeholders); `koi_task_*` sessions show Koi name + todo title.
- **`web_fetch` tool**: fetch URL and return plain-text body (kernel `v0.8.58`).
- **`piscis-ide-tools` crate**: shared LSP / `read_lints` tools moved out of the desktop host into the engine repo.

### Fixed
- **Pool coordination (engine `v0.8.58`)**: atomic `try_claim_koi_todo` prevents duplicate dispatch when heartbeat, auto-pickup, and `assign_koi` race; single-turn-per-Koi serialization; board-aware idle restore so `call_koi` and coordinator paths no longer clobber each other's busy status.
- **Shell / tool output**: smarter truncation with head+tail and error-line preservation (kernel `v0.8.58`).
- **Light mode**: pond project org-spec textarea and interactive-card inputs use theme `bg-secondary` instead of a hardcoded dark fallback.

---

## [0.8.61] - 2026-06-11

### Added
- **UI font scale**: Settings → Appearance → 1× / 1.25× / 1.5× / 1.75× / 2× zoom.
- **OpenAI curated skills market** and skill adaptation workflow (install → adapt in main chat).

### Fixed
- **Composer skill list** refreshes from Redux after install.
- **Plan todo snapshots** persist to activity log; Excel chart type constants corrected.

---

## [0.8.58] - 2026-06-10

### Added
- **Anthropic official plugins**: browse `claude-plugins-official` in-app (Skills → Anthropic 官方); git-subdir tarball fetch installs all `skills/*/SKILL.md` bundles with scripts/references/assets.
- **Tauri API**: `claude_plugins_list`, `claude_plugins_detail`, `claude_plugins_install`.
- **`install_from_skill_dir`**: full skill directory install (not only `SKILL.md`); hub-locked under source `claude-plugins-official`.

---

## [0.8.57] - 2026-06-10

### Added
- **Trusted skill evolution**: quadrant storage (`installed/` / `.draft/` / `learned/` / `.archive/`), `skill_manage` tool, background review fork, and Curator maintenance (stale marking, LLM merge, backup/rollback).
- **Skill evolution settings**: `skill_evolution` block in settings JSON (`review_enabled`, curator intervals, stale/archive days, etc.).
- **Skills UI**: Evolution tab with Promote/Discard, lock/unlock, pin, Curator panel, and revision history.
- **Memory hardening**: L2 compaction triggers session consolidation; headless/Koi paths run auto-extract + skill review.
- **Activity log**: per-session skill revision section in Audit Log (engine `v0.8.57`).

### Fixed
- **`.gitignore`**: root `skills/` pattern no longer ignores `src-tauri/src/skills/` source modules.
- **`skill_manage`**: path traversal guard on `write_file` / `remove_file`.

---

## [0.8.56] - 2026-06-06

### Fixed
- **OpenAI 400 `tool_call_id` orphan**: context build now collapses superseded failures before per-id tool pairing sanitize; cancelled or partially completed parallel tool rounds no longer leave assistant `tool_calls` without matching tool responses (kernel `v0.8.55`).

---

## [0.8.55] - 2026-06-08

### Fixed
- **Pond CLI `chat_ui`**: the IDE assistant panel now renders interactive forms and permission prompts inline — `chat_ui` / `chat_ui_patch` / `chat_ui_listen` and shell/file confirmations no longer hang forever waiting for the main Chat tab.
- **IDE file/git refresh after agent edits**: successful `file_write` / `file_edit` emits `ide-file-changed` (same-process writes bypass `notify` on some platforms); Git panel reacts to `gitPanelVersion` bumps; project paths are normalized before comparing watcher events (Windows `\` vs `/`).

---

## [0.8.54] - 2026-06-08

### Changed
- **Explorer**: collapse all / expand all buttons, sticky title bar while scrolling, tighter tree row spacing.

---

## [0.8.53] - 2026-06-07

### Fixed
- **Live permission prefs**: turning off shell/file-write confirmation in Settings now applies immediately to in-flight agent sessions (shared `ConfirmFlagsHandle`), not only new chats.
- **i18n**: Overlay HUD, Fish guide, Settings provider labels, User Tools config, Debug scenario hints, and remaining toast/permission strings.
- **Office/PPTX (Windows)**: PowerPoint COM reads use numeric visibility; added `read_document` / `read_slides` helpers.

---

## [0.8.52] - 2026-06-07

### Fixed
- **Pool project list sync**: listen for `pool_session_created` so projects created from main chat appear in the Pond sidebar immediately; also refresh on tab re-entry and via a new ↻ button (no polling timer).

---

## [0.8.51] - 2026-06-07

### Added
- **Input history (↑/↓)**: restore previously sent text with arrow keys in the main chat composer, pool collab chat, and IDE CLI assistant — each input keeps its own independent history (per session / per pool / per project).

---

## [0.8.50] - 2026-06-07

### Changed
- **Composer skill picker**: hides system built-in skills (Office/File/Web/System/Desktop); they remain available via progressive `skill_list` — only user-installed skills appear for explicit per-turn selection.

---

## [0.8.49] - 2026-06-07

### Fixed
- **Release CI**: `cargo fmt --check` on Windows failed for `chat.rs` (Koi persona changes), which skipped the Tauri bundle job — **v0.8.47 / v0.8.48 GitHub Releases therefore shipped source archives only**. Reformatted; installers publish from this tag.

---

## [0.8.48] - 2026-06-07

### Added
- **`AppDropdown`**: shared popover list component (`components/ui/AppDropdown`) for composer and toolbar filters — click to open, scrollable, optional search.

### Fixed
- **Koi persona persistence**: composer Koi selection stays active across sends until manually cleared.
- **Composer pickers**: workspace / Koi / skill use themed popover menus instead of native `<select>` (fixes GTK white-on-white and hold-to-scroll issues).
- **Pool member picker**: participants ⚙ opens a centered modal via `createPortal` + overlay CSS — no longer renders inline and squeezes the chat pane.

### Changed
- **Board / AuditLog**: Koi and priority / tool filters migrated to `AppDropdown`.

---

## [0.8.47] - 2026-06-07

### Added
- **Main Chat Koi persona**: composer Koi picker switches to persona mode — Koi system prompt, tool subset, per-Koi memory, and optional LLM override via `chat_send`.
- **Composer enhancements**: multi-attachment chips with preview, Ctrl+V image paste, and explicit skill selection dropdown.
- **School page**: sidebar **鱼群** merges Fish library and Koi manager (小鱼 | 锦鲤 tabs); Pond Collab links to Koi tab instead of inline modal.

### Fixed
- **Select styling**: unified form (`select.input`) and toolbar (`select-control`) dropdown theming — fixes Koi editor model dropdown white-on-white on Linux GTK/WebKit.

### Changed
- **Engine pin**: `piscis-core` / `piscis-kernel` pinned to **`v0.8.47`** (KoiPersona scene, per-image vision delegation).

---

## [0.8.46] - 2026-06-07

### Changed
- **RobotZ integration**: desktop automation stays as in-process builtin tools (`robotz-automation` / `robotz-browser`); legacy `robotz` MCP sidecar entries are removed on startup so the MCP tools tab lists only user-configured external servers.
- **MCP editor UI**: transport dropdown uses global `select-control` styling (readable in dark/light themes).
- **Pond Collab**: git activity icon uses the shared VS Code–style SVG (aligned with Pond IDE), replacing the `⑂` character.

---

## [0.8.45] - 2026-06-06

### Fixed
- **WeChat IM file send**: added `wechat` platform caps and clarified `im_send_message(file_path=…)` plus `SEND_FILE:` / `SEND_IMAGE:` markers with cross-platform path examples.
- **Pool i18n**: fixed raw keys (`pool.sendAsPiscisHint`, etc.) showing in the Collab chat UI.
- **Pool chat input**: removed the redundant hint line above the textarea; placeholder now carries the full coordination text with unified `--text-muted` color.

### Changed
- **Global light/dark theme**: sun/moon toggle beside the OpenPiscis title; new `light.css` theme.
- **Unified chrome**: consistent top bars across Tools / Fish / Pond / Chat; shared `--chrome-header-height` and `--topbar-bg`.
- **Compact controls**: global `select-control` (32px), `btn-icon`, and `page-toolbar` for audit filters and chat input actions.
- **Chat input row**: send/stop button height aligned with attach/preview; context-usage ring scaled to 2/3 without an extra rounded-rect wrapper.

---

## [0.8.44] - 2026-06-06

### Changed
- **鱼池 CLI sessions are IDE-only**: the main Chat picker no longer shows **+** for 鱼池CLI; the CLI empty state explains that sessions are created when you send a message in the Pond IDE. The Pond IDE assistant panel adds a **主会话** button to jump to the matching CLI tab in main Chat.

---

## [0.8.43] - 2026-06-06

### Changed
- **Main Chat session picker**: removed the left sidebar; **主会话 / IM / 鱼池CLI** are now top-bar dropdown buttons with search, **Today / Last 7 Days / Earlier** grouping, and lazy-loaded older sessions. Todo / Tools / Artifacts tabs sit on the same row, giving the full width to the conversation.

### Fixed
- **New chat from any tab**: creating a session from **+** or the welcome screen now switches to the main-chat filter first, so the new session is not immediately cleared by the IM/CLI visibility guard.

---

## [0.8.42] - 2026-06-06

### Changed
- **MCP engine upgrade (piscis-engine v0.8.42)**: pins the kernel that adds `headers` on MCP server config for authenticated SSE/HTTP connections, and a new streamable HTTP (`http`) transport that POSTs JSON-RPC to a single endpoint, parses JSON or `text/event-stream`, and tracks `Mcp-Session-Id`.
- **Desktop MCP config**: MCP server config literals in settings now accept the optional `headers` field.

---

## [0.8.41] - 2026-06-06

### Added
- **Project Koi membership**: Koi now explicitly join a project before they can take part in it. The participants panel shows only members; the gear icon opens a picker to add Koi, and each participant has an × to remove it (blocked while it still has active todos).
- **`pool_org` membership actions**: `add_member`, `remove_member`, and `list_members`, so Piscis builds the team before assigning work. `assign_koi` and the kanban board now reject non-members.

### Changed
- **Task assignment is membership-scoped**: the board's “assign to” dropdown and pool `@mention` autocomplete list only project members.
- **No global Koi limit**: removed the 10-Koi cap; the Koi library can grow without bound.

### Migration
- Existing pools are backfilled once: each pool's initial roster is derived from the distinct Koi that already own todos in it. Pools without historical Koi todos start empty.

---

## [0.8.40] - 2026-06-06

### Fixed
- **Chat lazy-load history**: “Load more history” no longer stalls after an agent turn. Pagination now tracks the real DB row offset separately from the collapsed store length (`setMessagesWithFrozen`), so older messages load correctly on scroll or button click.

### Changed
- **Pond IDE / Git activity icon**: Replaced the plain `⑂` character with a VS Code–style source-control SVG and theme-colored nodes (accent / success / warning) to match other activity-bar icons.

---

## [0.8.39] - 2026-06-06

### Fixed
- **Main Chat / heartbeat leak**: Piscis heartbeat and pool coordination sessions (`piscis_heartbeat_global`, `piscis_pool_*`, etc.) no longer load or render in the main Chat area when the sidebar has no user-visible sessions. They remain visible only in the Pond **Piscis coordination** inbox.

---

## [0.8.38] - 2026-06-05

### Changed
- **Engine pin**: `piscis-core` / `piscis-kernel` pinned to **`v0.8.38`** on the correct 0.8.x release line (pluggable loop/contrib, context/memory hooks).
- App version aligned with engine **0.8.38** (`Cargo.toml` / `package.json` / `tauri.conf.json`).

---

## [0.8.37] - 2026-06-04

### Changed
- **Engine pin**: initial pin to pluggable loop/contrib extensions (superseded by 0.8.38 engine tag alignment).

---

## [0.8.26] - 2026-05-31

### Fixed
- **CI / Tauri bundle**: `piscis_compact_one` is now a `[[example]]` (bench-only) instead of `[[bin]]`, so release installers no longer expect a binary that is only built with `bench-compact`.

---

## [0.8.25] - 2026-05-31

### Changed
- **Heartbeat / org_spec**: consume `piscis-engine` with a stricter default heartbeat prompt — `HEARTBEAT_OK` is gated on `pool_org(read)` and explicit org_spec convergence checks (no hardcoded phase rules). Supervisor attention stays active when the board is quiet but global coordination may still be required.
- **Kernel split**: pin `piscis-core` / `piscis-kernel` git dependency to the latest engine commit (`7c04d03`).

### Fixed
- **Pool heartbeat context**: per-pool heartbeat runs now remind Piscis to read org_spec before replying `HEARTBEAT_OK`.
- **IDE Git panel**: clearer commit errors (`commitNeedStaged`, `gitNoProjectDir`); backend validates staged files and writes commit messages via `-F` (fixes silent commit failures).

---

## [0.8.24] - 2026-05-28

### Added
- **`useScrollPrependedHistory` hook**: shared scroll-up pagination (layout restore, clickable load-more, short-viewport auto-fill) for message lists.
- **Pool mention ack**: immediate Piscis `post_status` line when automated `@!Piscis` dispatch runs (system paths).

### Fixed
- **`@!Piscis` pool dispatch**: forced `build_forced_mention_attention` so mention-triggered heartbeats no longer skip when the periodic cursor advanced; mention channel requires visible `pool_org(post_status)` replies.
- **Chat history lazy-load**: prepend scroll restore via `useLayoutEffect`, clickable「加载更多历史」, auto-load when the viewport is shorter than one screen.
- **Piscis coordination console**: show tool-call rows with summaries; fix empty right pane with thousands of messages and a stuck「加载更多」.
- **Collab / ChatPool lazy-load**: same prepend-scroll fixes as main chat.

### Changed
- **Pool chat identity**: humans send as **`sender_id: piscis`** (act as Piscis coordinator); `user` sender rejected; `@!Piscis` self-delegation blocked — use **IDE Piscis CLI** for 1:1 Piscis chat (main session · Pool CLI).
- **Pool `@` autocomplete**: Koi only; hint documents **Ctrl+Enter** send and **Enter** newline.

---

## [0.8.23] - 2026-05-28

### Added
- **Chat UI Protocol v2**: wizard (`mode: "wizard"` + `steps`), layout blocks (`row` / `column` / `card`), display blocks (`image`, `code_preview`, `progress`, `link_list`), `file_picker`, and `data` model with `__data_model__` on submit/action.
- **Non-terminal actions**: button `emit: "action"`; companion tools `chat_ui_patch` (live card updates) and `chat_ui_listen` (resume blocking submit on same `request_id`).
- **Catalog**: `docs/piscis.chat.catalog.json` — allowed block types and agent authoring constraints.

### Changed
- Interactive card renderer and agent prompts updated for v2; v1 cards remain compatible.

---

## [0.8.22] - 2026-05-28

### Fixed
- **IDE Explorer delete**: file/folder delete now passes absolute paths under the project root (same as open/read/write), so `ide_file_action` actually removes the target.
- **Delete confirmation dialog**: shows the real file or folder name instead of the literal `{{name}}` placeholder; failures surface an alert instead of failing silently.

---

## [0.8.21] - 2026-05-29

### Added
- **Chat UI Protocol v1** (`docs/chat-ui-protocol.md`): structured agent↔user interaction spec for the `chat_ui` tool — block types, validation, conditional `show_when`, submit payload (`__meta__`, `__action__`), and authoring guidelines.
- **Extended interactive card renderer**: `date` / `time` / `datetime`, `slider`, `switch`, `textarea` (`text_input` + `multiline`), `tags`, `section`; `email`/`url`/`password` input modes; inline validation (required, min/max, pattern, selection counts).
- **`allow_custom` on select/radio/checkbox**: “Other” option with free-text; submitted value is the user’s string, not a sentinel.
- **Per-session pending cards**, deduplicated history vs live footer, default Submit when no `actions` block; `submit_label` on `ui_definition`.

### Fixed
- **`chat_ui` `request_id` aligned with `tool_use_id`** via `ToolContext.tool_use_id` so submit works after message reload and matches persisted tool calls.
- **Interactive cards no longer leak across sessions** or duplicate at the bottom after `done`.

### Changed
- **`chat_ui` tool schema** and Piscis system prompt reference Protocol v1 and the protocol doc.

---

## [0.8.20] - 2026-05-29

### Fixed
- **CI / clippy**: remove unnecessary `let _ =` on `app.exit(0)` in shutdown backstop (`clippy::let_unit_value`).

---

## [0.8.19] - 2026-05-28

### Fixed
- **Tray / overlay quit no longer hangs the process**: exit now runs a ordered shutdown (cancel agents, stop IM gateway, close browser, destroy PTY terminals, clear file watchers, stop LSP, cancel UIA calibration) before `app.exit`, with a force-exit backstop if teardown stalls.
- **Unified quit path**: tray menu, overlay「退出」, and new `quit_app` command all use the same shutdown helper instead of calling `app.exit` / `plugin:process|exit` directly.

---

## [0.8.18] - 2026-05-28

### Fixed
- **Tab switch no longer discards Chat / Pond state**: main views stay mounted (hidden) after first visit so switching pages does not remount and lose in-progress UI state.
- **`@!piscis` pool mentions now trigger dispatch**: `contains_piscis_mention` recognizes `@!piscis` (previously only matched the `@piscis` substring, so forced-mention scheduling never fired).
- **Koi observation console filtered by current project**: inbox / observation streams only show messages for the active pool session suffix and related todo sessions.
- **Piscis coordination console shows pool traffic again**: coordination feed scoped to `piscis_pool_{id}` instead of an empty mixed filter.
- **Kanban uses the current project only**: removed the redundant project dropdown; board always reflects `filterSessionId`.
- **Collab IDE Explorer right-click menu**: same VS Code–style context menu wiring as the standalone Pond IDE view.
- **Pond CLI tab lists existing CLI sessions**: sessions created with `source: "cli"` surface under「鱼池 CLI」; switching to the tab refreshes the session list; IDE assistant reuses existing `Piscis CLI —` sessions instead of duplicating.

### Changed
- **Heartbeat on by default** (`heartbeat_enabled` default `true`) with an updated default prompt covering inbox/todos, project convention checks (§3), and scheduled maintenance (§4).
- **`pool_org` / `wait_for_koi` treated as ephemeral**: wait exchanges are not persisted and are stripped before subsequent LLM turns, reducing wasted context on coordination waits.
- **Koi managed turns use the full six-layer protocol** on inbox, kanban, and `call_koi` paths (shared `runtime/koi_prompt.rs`); Stop Gate now mandates `complete_todo` as the last action when a todo is active.
- **`koi_execute_todo` template** strengthened with an explicit completion checklist for Koi workers.

### Added
- **`create_session` optional `source`** (e.g. `"cli"`) for classifying Pond CLI sessions end-to-end.
- **`isPondCliSession()`** helper and Redux/chat wiring for CLI session discovery.

---

## [0.8.17] - 2026-05-28

### Added
- **Windows UIA mouse precision calibration** (Debug → UIA Test): full-screen 5-point overlay, user ground-truth clicks, Piscis real-click verification via `uia.click` + `GetCursorPos`, OLS linear fit per monitor, persisted to `uia_calibration.json` with DPI/layout fingerprint invalidation.
- **Calibration applied to UIA mouse paths**: `click`, `double_click`, `right_click`, `hover`, `scroll`, and `drag_drop` automatically apply the active per-monitor transform; internal `_skip_calibration` bypass for the calibration run itself.

### Fixed
- **Windows DPI coordinate drift**: declare Per-Monitor V2 DPI awareness in `windows-app-manifest.xml` so screenshots, UIA bounding rects, `SendInput`, and `GetCursorPos` share the same physical pixel space.

### Changed
- **Non-Windows hosts**: calibration UI is hidden on Linux/macOS (calibration remains Windows-only; `desktop_automation` on other platforms is unchanged).

---

## [0.8.16] - 2026-05-25

### Added
- **Explorer file tree: VS Code–style right-click context menu and keyboard operations**.
  - Right-click any file or folder to open a context menu with: **Open**, **Rename** (F2), **Delete** (Del), **Copy Path**, **Copy Relative Path**, **Reveal in File Manager**, **New File**, **New Folder**.
  - **Ctrl/Cmd+click multi-select** — hold Ctrl (or Cmd on macOS) while clicking to select multiple files/folders. Bulk Delete works on the entire selection.
  - **Delete** key deletes selected files/folders (with a per-type confirmation dialog: single file, folder, or bulk count).
  - **F2** key starts inline rename on the active (highlighted) node.
  - Context menu auto-selects the right-clicked node if it isn't already in the selection (VS Code behavior).
  - Keyboard shortcuts are scoped to the file tree — they only fire when the tree (or a child element) has focus and no inline input is active.
  - The same features are wired in the Collab IDE view.

### Fixed
- **Opening the 2nd/3rd/etc. file no longer shows a spurious dirty dot** (reported as "除打开的第一个文件外，点击打开其他文件，标题都默认加上了修改标记圆点").
  - Root cause: Monaco fires `onChange` with the new content during its internal model update (before React's `useEffect` runs). The previous guard used `lastContentRef` which was only updated in `useEffect` — too late. The `onChange` handler compared new content against the OLD file's content, saw a mismatch, and set `isDirty = true`.
  - Fix: `CodeEditor` now tracks `lastPathRef` and resets `lastContentRef.current` synchronously during render when `tab.path` changes. Monaco's subsequent `onChange` fires with content that matches `lastContentRef.current`, so the tab stays clean.
- **`assign_koi` no longer crashes with a cryptic "FOREIGN KEY constraint failed"** (reported as "尝试通过 assign_koi 将任务分配给 Contrarian，但系统返回了 FOREIGN KEY constraint failed 错误").
  - Root cause: `pool::services::assign_koi` (`src-tauri/piscis-kernel/src/pool/services.rs`) trimmed the caller-supplied `koi_id` and passed it straight into `create_koi_todo`'s `owner_id`, which carries `FOREIGN KEY(owner_id) REFERENCES kois(id)`. If the Koi had been removed (e.g. by the startup `dedup_kois`) or was passed as a display name that didn't match any current id, SQLite rejected the INSERT with a generic FK error instead of a useful diagnostic.
  - Fix: `assign_koi` now calls `resolve_koi_identifier` up-front (same helper used by `execute_managed_koi_turn`), replaces the raw input with the canonical `kois.id`, and bails with an actionable error listing the available Kois when the lookup fails.
- **`@!Piscis` pool mentions no longer crash with "FOREIGN KEY constraint failed"** (reported as "@!Piscis 在聊天室里发送多次都没有任何回应").
  - Root cause: `coordinator::handle_mention` (`src-tauri/piscis-kernel/src/pool/coordinator.rs`) created a `koi_todo` with `owner_id = "piscis"`, but `"piscis"` is a synthetic sentinel that is NOT seeded by `ensure_starter_kois` (only Architect/Coder/Reviewer are). With `PRAGMA foreign_keys=ON` and `FOREIGN KEY(owner_id) REFERENCES kois(id)`, the INSERT failed silently on every mention, so the todo was never persisted and the subsequent `spawn_mention_dispatch` had nothing to act on.
  - Fix: `handle_mention` now calls `db.upsert_koi_with_id("piscis", "Piscis")` (`INSERT OR IGNORE`) inside the same write closure before creating the todo. The upsert is idempotent and cheap, so it's safe on every call.
- **Unused `spawn_immediate_dispatch` helper suppressed** (`src-tauri/src/piscis/heartbeat.rs`).
  - Workspace-wide lint policy denies dead code as errors. The function is superseded by `spawn_mention_dispatch` (pool-scoped); added `#[allow(dead_code)]` and a doc note so the crate still compiles.
- **`screen_capture` / `browser screenshot` can now persist the image to disk** (reported as "Piscis 说截图保存为 artifact 了，但 Artifacts 面板是空的").
  - Root cause: both tools returned the screenshot only as a base64 blob inside the tool result. Piscis has no built-in "decode base64 → write file" action, so there was never a real file on disk for `app_control(action="artifact_submit")` to reference. The system-prompt rule for deliverables tracking told Piscis to submit the image path, but no such path existed.
  - Fix: `screen_capture` now accepts an optional `output_path` parameter (absolute path); when provided, the encoded bytes are written to that path before the tool returns. `browser(action="screenshot")` exposes the same capability via its existing `save_path` field. The result string includes an explicit "Saved to disk: <path>" line (or a WARNING on failure) and a reminder to call `artifact_submit` with that path. The Piscis system prompt's "Deliverables Tracking" rule now spells out this two-step flow and explicitly tells the model: "Never tell the user you have saved a screenshot unless you actually called `screen_capture` with `output_path` and received the 'Saved to disk:' confirmation."
- **Vision-loop detector no longer fires during normal desktop automation** (reported as "在 desktop automation 操作中频繁出现系统提示消息").
  - Root cause: the previous session started refactoring the detector but left three helper functions (`is_vision_tool`, `is_substantive_desktop_action`, `vision_loop_warning`) undefined, causing a compile error. Additionally, the thresholds (Windows=5, Linux=10) were too strict, and the detector did not distinguish between passive observation (`move_mouse` + `screen_capture`) and substantive actions (`click`, `type_text`).
  - Fix: implemented all three missing helpers. Thresholds raised to Windows=8, Linux=15. On Linux/macOS, `desktop_automation(action="move_mouse"|"get_cursor_position")` is treated as a legitimate calibration step — the `move→screenshot→move→screenshot` pattern no longer increments the vision-only streak counter. On Windows, `uia` calls are recognized as substantive actions (UIA can target elements without screenshot verification). `is_substantive_desktop_action` correctly distinguishes click/type/hotkey/drag/scroll/launch_app from observation-only actions.

---

## [0.8.15] - 2026-05-25

### Added
- **Mandatory deliverables-tracking guidance in Piscis's system prompt** (reported as "every session's artifacts list is empty").
  - Added a new `## Deliverables Tracking (Mandatory)` section to the core Piscis system prompt in `src-tauri/src/commands/chat.rs`. Piscis must now call `app_control(action="artifact_submit", ...)` in the SAME turn it produces each tangible output:
    - Files created / modified via `file_write` / `file_edit` → `artifact_type="file"`, absolute path
    - Screenshots (`screenshot`, `browser_screenshot`, `screen_capture`) → `artifact_type="image"`
    - Web resources (reports, published URLs, documentation) → `artifact_type="link"`
    - Prose-only reports / analyses → `artifact_type="report"` with `content_summary`
    - Koi-reported file paths from `pool_chat` → Piscis submits each one as an artifact on the user-facing session
  - Added a "self-check before ending a run" rule: before the final user-facing reply, scan the turn for any file path written, screenshot captured, or URL delivered and submit any that are still missing from the artifacts list.
- **Koi deliverables reporting format** (`src-tauri/piscis-core/src/koi_prompt.rs` — Reconciling step 3a). Koi must now post file outputs as a `Deliverables:` list of absolute paths (one per line) so Piscis can reliably parse them and surface each as an artifact on the user's session.
- The Artifacts panel in the Chat UI (`src/components/Chat/index.tsx` → `ArtifactsPanel`) already supported click-to-open: `openPath(artifact.uri!)` for local files and `<a target="_blank">` for URLs — no UI change needed; the panel now has artifacts to display.
- **Chat session list now classifies sessions into three category tabs**: **Chat / IM / Pond CLI** (i18n: `chat.filterChat` / `chat.filterIM` / `chat.filterCli`).
  - `classifySession()` (`src/components/Chat/index.tsx`) now returns a three-way `SessionKind`: `"chat" | "im" | "cli"`. Pond-CLI sessions (`source === "cli"`, set by `src-tauri/piscis-kernel/src/headless/mod.rs` and `src-tauri/piscis-cli/src/runner.rs`) are routed to their own tab instead of being mixed into IM.
  - The previous "All" tab has been removed. The default filter is now `"chat"`. Internal/system sessions (`piscis_pool`, `heartbeat`, `piscis_inbox_*`, `piscis_heartbeat_*`, `koi_*`, …) remain hidden via `isInternalSession`.
  - Pond-CLI entries show a 🐟 source icon (added to `sourceIcon()`).

### Fixed
- **IDE file save was silently broken** (Ctrl+S did nothing; "Save" from the tab header appeared to do nothing; closing the tab did not warn about unsaved changes).
  - Root cause: Monaco Editor registered `Ctrl+S` via `addCommand(2048|49, () => { /* empty */ })` inside `src/components/Pond/IDE/CodeEditor.tsx`, swallowing the keydown before it reached the IDE's `window` listener — so the keystroke was intercepted but no save was performed.
  - Wired a real `onSave` prop through `IDE → EditorTabs → CodeEditor` and changed the Monaco command to call `onSaveRef.current?.()`. The actual disk write still lives in the IDE layer (`saveFile`) where `tabs` state and `projectDir` are owned.
  - Fixed a stale-closure bug in `saveFile`: it used to capture `tabs` and `projectDir` from the closure, so if the user typed between renders the saved content was the *old* state. Now reads `tabsRef.current` and `projectDirRef.current`, so Ctrl+S always writes the latest buffer.
- **IDE / Collab editor now auto-reloads files modified by agents or external tools** (reported as "agent 在外部修改了文件后，IDE 已打开的文件未刷新内容").
  - Root cause: `src-tauri/src/commands/ide.rs::ide_start_watcher` emitted `evt.path` with OS-native separators (`src\\foo.rs` on Windows), but `tab.path` is stored with `/` (that's how `FileTree` reports node paths and how `openFile` persists them). The `tab.path === evt.path` check therefore silently failed on Windows, so the `ideApi.readFile(...)` reload was never triggered.
  - Backend (`ide_start_watcher`): the emitted `path` payload is now normalized to forward slashes via `rel.replace('\\', "/")` before `app_clone.emit("ide-file-changed", …)`. The `.git` / `node_modules` / `.koi-worktrees` filters already used the normalized form, so behavior is unchanged for those exclusions.
  - Frontend (defensive, survives any future regression): both `src/components/Pond/IDE/index.tsx` and `src/components/Pond/Collab/index.tsx` now run `evt.path.replace(/\\/g, "/")` before comparing with `tab.path`. The same normalized `evtPath` is used when building the full path for `ideApi.readFile`.
- **Closing the app / navigating away with unsaved tabs now triggers the browser's beforeunload prompt** — new `useEffect` in `src/components/Pond/IDE/index.tsx` watches `tabsRef.current` for any `isDirty` tab and sets `e.preventDefault()` + `e.returnValue = ""` on `beforeunload`.
- **Dirty-dot indicator on editor tabs now reliably reflects `isDirty`** state. `tab.isDirty` is set to `true` on every Monaco `onChange` and cleared on successful `ideApi.writeFile`. The dot is rendered inside `<span className="tab-name">` so it stays visible even when the tab isn't hovered (previously it shared the close-button's hover rule and flickered in/out).
- **Right-click context menu on editor tab headers** (VS Code parity):
  - Close — closes the right-clicked tab (with unsaved-changes confirmation if dirty).
  - Close Others — keeps only the right-clicked tab; saves dirty others if the user confirms.
  - Close All — closes every tab; saves any dirty ones first (with a bulk confirmation dialog).
  - Save — enabled only when the right-clicked tab is dirty; triggers the same `saveFile` as Ctrl+S.
  - Close Unsaved — closes every dirty tab; saves them first if the user confirms the bulk prompt.
  - Context menu dismisses on outside click, Escape, or scroll.
- **Tab close now prompts when the tab has unsaved changes** (per-tab `window.confirm` before `removeTab`), so users can no longer lose work by clicking the × on a dirty tab.
- Internationalization: added IDE keys `closeCurrent`, `closeOther`, `closeAll`, `closeUnsaved`, `saveFile`, `unsavedConfirm`, `unsavedBulkConfirm` (en + zh).

---

## [0.8.14] - 2026-05-25

### Fixed
- **Settings persistence gaps that caused toggles to revert after save** (reported as "WeChat IM toggle turns back on after I disable and save").
  - Root cause: the `save_settings` handler in `src-tauri/src/commands/config/settings.rs` only wrote a subset of the fields the frontend sent. Any field not explicitly matched was silently dropped on the backend, so the UI appeared to flip back when the settings were re-read.
  - Added persistence for every remaining Settings field that has a UI control (or is part of the form state):
    - WeChat / iLink Bot: `wechat_enabled`, `wechat_gateway_token`, `wechat_gateway_port`, `wechat_bot_token`, `wechat_base_url`, `wechat_bot_id`.
    - Compaction tiers: `compaction_micro_percent`, `compaction_auto_percent`, `compaction_full_percent`.
    - Agent loop: `max_tool_result_tokens`, `summary_model` (handles both empty-string and explicit-null as "clear override").
    - App behavior: `allow_multiple_instances`.
    - Fallback models: `fallback_models` (array, with blank entries filtered out).
  - This is a complete audit of the `Settings` struct vs. the `save_settings` handler; every field reachable from the UI now round-trips through the backend.

---

## [0.8.13] - 2026-05-25

### Fixed
- **Minimal-mode overlay right edge clipped on Windows at non-100% DPI scales** (125%/150%/175%). Root cause: `enter_minimal_mode` restored the saved `overlay_x/overlay_y` (physical pixels) verbatim, so a position saved at one DPI scale became stale after the scale changed, after a monitor swap, or after a resolution change — typically pushing the overlay's right edge past the monitor's physical right bound. Two-part fix in `src-tauri/src/commands/platform/window.rs`:
  - Added `clamp_overlay_to_work_area()` helper that queries the current primary work area via `SPI_GETWORKAREA` (Windows) / 1920×1080 fallback (other platforms) and clamps the candidate position so the entire overlay stays inside the visible area. Applied in both `enter_minimal_mode` (saved-position restore and first-launch centering) and `enter_unattended_im_mode` (defense in depth).
  - Explicitly call `overlay.set_size(LogicalSize(280, 56))` when the WebView inner size doesn't match the configured size, guarding against WebView2 viewport / window-size mismatches at high-DPI on Windows. This ensures the CSS pill (280×56 logical pixels) always fits within the drawable area.
- First-launch centering math now uses `get_overlay_size()` instead of hardcoded `140` / `80` constants, so the pill is correctly centered at the bottom of the main window regardless of DPI scale or any future config change to the overlay size.

---

## [0.8.12] - 2026-05-28

### Changed
- **Default LLM max output tokens raised from 4096 to 8192.** Applies to:
  - `Settings` form initial state (`src/components/Settings/index.tsx` — first-install default)
  - `Settings` max-tokens input fallback (`?? 8192` instead of `?? 4096`, so an unset field renders as 8192)
  - Rust `default_max_tokens()` in `piscis-kernel/src/store/settings.rs` (backend default when a user's config predates the field)
  - The onboarding wizard saves only `provider/model/policy_mode/api_key`, so new users inherit the backend default — which is now 8192 automatically.
- This bumps `compute_context_budget` from the 64k tier to the 128k tier for users who haven't set `context_window` manually — giving the agent more input room to work with on top of the larger per-response output allowance.

### Fixed
- **CI `cargo fmt --check` failure on `rust-kernel` job** (v0.8.11 tag run #287). The previous `detect()` method signature in `piscis-kernel/src/agent/loop_.rs` was formatted on one line, exceeding `rustfmt`'s 100-char width threshold. Re-formatted with `cargo fmt` so the signature spans multiple lines. This was the only failure on the last CI run; frontend / clippy / tests / headless / desktop-cross jobs were all green.

---

## [0.8.11] - 2026-05-28

### Refactor
- **Collab layout holistic restructure** to a clean VS Code-style 3-layer architecture:
  - `collab-center` (vertical column) → `collab-content-shell` (horizontal row) → `collab-main-view` + `collab-ide-side`.
  - The IDE side panel (explorer / search / git) is now rendered **last** inside the content shell, so it sits to the **right** of the main view, adjacent to the icon strip — matching VS Code / Cursor convention. Previously it was placed to the editor's left, which was a regression introduced during v0.8.8.
  - The bottom panel (terminal / assistant) is a **sibling** of the content shell inside `collab-center`, so it spans both the main view and the side panel — matching VS Code's integrated terminal behavior.
  - Introduced a single `collab-main-view` host node that wraps all five mutually-exclusive views (chat, IDE editor, board, inbox, koiObserver). Future view additions only need to add a new conditional child — the layout structure above/below stays unchanged.
  - Eliminated the `.collab-content-area--with-side` conditional flex-direction hack. `.collab-content-shell` is now unconditionally `flex-direction: row`.
- No behavioral changes: toggle-on-re-click to collapse the side panel, auto-expand on view switch, terminal/assistant mutual exclusion, left-panel resize, terminal resize — all preserved.

---

## [0.8.10] - 2026-05-28

### Fixed
- **Vision-model screenshot-analyze loop**: when driving desktop automation through a vision model (WeChat/QQ input, screen-control tasks), the agent occasionally gets stuck in a "describe-then-verify" loop — `move_mouse → take_screenshot → screen_analyze → move → screenshot → analyze …` — repeatedly re-describing the scene without ever committing to the target action (click, type, press Enter). Generic loop detectors (same-input repeat, no-progress streak, ping-pong) all miss this because each iteration uses fresh coordinates / fresh screenshots / different analysis prompts, so hashes differ. Added a dedicated **VisionLoop density detector** in `piscis-kernel/src/agent/loop_.rs`: if 3 or more of the last 8 tool calls are screenshot / vision-analyze (matched by substring against `screenshot`, `screen_analyze`, `screen_capture`, `vision_analyze`, `vision_context`), a Warning-level nudge is injected telling the model to stop over-verifying and commit to a concrete action sequence (click input → type text → press Enter, all in one turn, no more screenshots). A cooldown of 3 real-action turns prevents the nudge from firing on every single iteration. The screenshot tool itself is never *blocked* (only nudged) — legitimate multi-step vision work still proceeds, just with periodic prompts to keep the agent on-task.

---

## [0.8.9] - 2026-05-28

### Added
- **VS Code-style toggle on explorer/search/git icon strip**: clicking an already-active IDE view icon (explorer / search / git) now collapses the IDE side panel, and clicking again expands it. Switching to a different view from a collapsed state automatically re-expands the side panel. The icon's active highlight mirrors the collapse state — a collapsed IDE view shows as un-highlighted, matching VS Code's activity-bar feedback. Non-IDE views (chat, board, inbox, koiObserver) are unaffected.

---

## [0.8.8] - 2026-05-28

### Fixed
- **Chat-room scroll lands at top when re-entering from another view**: switching `contentView` from `explorer`/`search`/`git`/`board`/`inbox` back to `chat` unmounts and remounts the message scroll container, but the scroll-to-bottom effect only depended on `activeSessionId` and `messages.length` so the new mount stayed at `scrollTop=0`. Added `contentView` to the effect dependency and changed the pin key from session id to `${session}|${contentView}` so each entry into chat re-pins to the bottom.
- **Terminal squeezed by IDE side panel in explorer/search/git views**: the IDE side panel (260 px) sat between `.collab-center` and `.collab-right`, eating horizontal space from the terminal/assistant slot at the bottom of `.collab-center`. Restructured the layout so the IDE side panel now lives *inside* `.collab-content-area` (which becomes `flex-direction: row` via the `--with-side` modifier when an IDE view is active). The bottom panel now spans the full width of `.collab-center` regardless of which view is active.
- **IDE Explorer inline-create UX**: replaced the system `window.prompt()` dialog with a VS Code–style inline text input that appears at the correct depth in the file tree. Inside-directory selection creates inside that directory; file selection creates at sibling level; no selection creates at project root. Enter commits, Escape cancels, the target directory auto-expands, and the input auto-focuses.

### Added
- **Piscis CLI assistant panel**: new 🤖 button in the right-side icon strip (above the terminal toggle) opens a CLI-style chat in the same bottom slot as the terminal. Designed for users unfamiliar with shell commands — ask in plain language ("build the project", "git status", "find TODOs in src/") and Piscis runs the corresponding actions through its standard tool stack. Implementation: `src/components/Pond/IDE/AssistantPanel.tsx` lazily creates a per-project `Piscis CLI — {project}` session bound to the project workspace, streams `agent_event_*` `text_delta` to a monospaced log, and surfaces tool start/end + errors as muted lines. Terminal and assistant are mutually exclusive in the bottom slot; toggling one closes the other. Localized via `ide.assistant`, `ide.assistantTitle`, `ide.assistantHint`, `ide.assistantInputPlaceholder`, `ide.assistantSend`, `ide.assistantClear`.

---

## [0.8.7] - 2026-05-28

### Fixed
- **Right-side icon menu tooltips not internationalized**: tooltips on the Pond/Collab right-side icon strip (chat, explorer, search, git, board, inbox, koiObserver) showed raw i18n keys like `collab.tabChat` because the namespace did not exist. Switched the lookup to the existing `pond.tab*` namespace and added the four missing keys (`tabChat`, `tabExplorer`, `tabSearch`, `tabGit`) to both `zh.ts` and `en.ts`.

### Added
- **IDE Explorer: New File / New Folder buttons**: added two action buttons next to the refresh button in the EXPLORER title bar (similar to VS Code), wired to the existing `ide_file_action` Tauri command. Buttons prompt for a name and create the file or directory at the project root, then refresh the tree. All Explorer header tooltips and the empty-state label are now localized via `ide.refresh`, `ide.newFile`, `ide.newFolder`, `ide.newFilePrompt`, `ide.newFolderPrompt`, `ide.noFiles`.

---

## [0.8.6] - 2026-05-27

### Fixed
- **Chat-room session-switch scroll position**: switching between Collab projects landed at the first message instead of the latest one. Root cause: a single `requestAnimationFrame` after the messages array changed fired before `MessageBubble` had finished progressive markdown / code-block layout, so `scrollHeight` was still smaller than the final value. Replaced with a `ResizeObserver` that pins the container to the bottom for ~600ms while content settles.
- **@!Piscis no response in chat room**: `coordinator::handle_mention` only records `@!Piscis` as a board todo (because Piscis is not a Koi and `activate_pending_todos` skips `db.get_koi("piscis") == None`); execution then waited for the periodic heartbeat timer (up to `heartbeat_interval_mins`). Added an immediate `dispatch_heartbeat` fan-out from `send_pool_message` so `@!Piscis` mentions are processed right away.
- **KoiManager nested scrollbars + flicker**: the wide modal (`.koi-modal--wide`) and its inner `.koi-manager` both had `overflow-y: auto`, producing a barely-scrollable outer scrollbar and a flicker when the wheel switched between them. The modal is now a flex column with `overflow: hidden`; the inner manager owns scroll with `scrollbar-gutter: stable` to prevent layout flicker.

---

## [0.8.5] - 2026-05-27

### Fixed
- **Lazy load scroll locked at 0**: scrolling to the top of IM/chat sessions to load older messages would lock the scrollbar at position 0, preventing both further lazy loads and scroll-back. Root cause: the `onScroll` handler called `requestAnimationFrame` to restore scroll position BEFORE `loadMoreHistory`'s async fetch completed, setting `el.scrollTop = 0` prematurely. Fixed by removing the redundant outer `rAF` — `loadMoreHistory` already handles scroll restoration correctly after messages are prepended.
- **JSON parse errors in package.json and tauri.conf.json**: missing trailing commas after the version field (from the v0.8.4 version bump) caused `npm test` and `cargo build` to fail on CI.

---

## [0.8.4] - 2026-05-26

### Fixed
- **@mention autocomplete trigger**: typing `@` now immediately shows the agent dropdown; previously the empty-string filter was treated as falsy, preventing the list from appearing until additional characters were typed.
- **@mention dropdown keyboard navigation**: up/down arrow keys now scroll the highlighted item into view when it moves outside the visible area.
- **Missing i18n keys**: IDE file preview area now shows localized placeholder text (`ide.openFileHint`, `ide.searchHint`, `ide.gitHint`) instead of raw key strings.
- **@!Piscis mention dispatch**: sending `@!Piscis` in a new project now correctly creates a board todo owned by the Piscis coordinator; previously `parse_mention_targets` only iterated DB Kois and Piscis (not being a Koi) was silently ignored.
- **Koi stuck in "busy" state**: when Piscis manually completes a Koi's todo via `pool_org`, the Koi's status is now reset to "idle" instead of remaining permanently busy.
- **Timezone consistency**: all system prompt time injections now use UTC (`chrono::Utc::now()`) with an explicit "UTC" label, preventing 8-hour offset confusion when the agent compares prompt time with DB timestamps (which are always UTC/RFC 3339).
- **Chat room scroll position on session switch**: switching between project chat rooms now reliably scrolls to the latest messages at the bottom. Replaced fragile `no-deps useEffect` + `initialLoadDoneRef` pattern with a `[activeSessionId, messages.length]` effect guarded by `scrolledSessionRef`.

### Changed
- **Koi prompt template**: now includes `{project_dir}` substitution and explicit guidance about the `kb/` directory vs project directory. `kb/` is clarified as "shared knowledge only" (decisions, patterns, conventions); actual deliverables must be saved to the project workspace, not `kb/`.

### Added
- **Koi Manager dialog**: a gear icon (⚙) next to the "Participants" header in the Collab left panel opens the KoiManager as a modal dialog, restoring Koi configuration access that was lost during the UI restructure.

---

## [0.8.3] - 2026-05-26

### Added
- **First-class Language Server Protocol (LSP) integration**:
  - New `piscis-desktop` `lsp` module: `LspManager` spawns and lifecycle-manages per-project+language LSP processes (rust-analyzer, typescript-language-server, pyright, clangd) auto-detected from `PATH`. Each session gets its own WebSocket bridge that frames LSP JSON-RPC for the front-end.
  - Monaco IDE wires to the bridge through `monaco-languageclient` + `vscode-ws-jsonrpc`, delivering diagnostics, hover, completion, go-to-definition, references, document symbols, and rename inside the embedded editor.
  - New Tauri commands `ide_lsp_list_languages` / `ide_lsp_start` / `ide_lsp_stop`.
- **`lsp` agent tool**: actions `diagnostics`, `hover`, `complete`, `definition`, `references`, `rename`. The agent can now navigate and understand code without grepping.
- **`read_lints` agent tool** (Cursor's ReadLints parity): batch multi-file diagnostics with `severity` filter (`error` / `warning` / `all`) and tunable `wait_ms`. Use it after edits to verify code quickly via the running LSP servers.

---

## [0.8.2] - 2026-05-25

### Fixed
- **Windows console popup on every IDE Search / enterprise capability check**: `rg` (search) and `npx`/`npx.cmd` (enterprise node check) were spawned without `CREATE_NO_WINDOW`, causing a blue console window to flash on every keystroke in the Search panel and every time the enterprise settings page was opened. Both sites now use the new centralised spawn helper.

### Added
- **`piscis_kernel::proc::{tokio_command, std_command}` — centralised popup-safe spawn helpers**: a single module now applies `CREATE_NO_WINDOW = 0x0800_0000` on Windows for every child process spawned by the application. All call sites across `piscis-kernel` and `piscis-desktop` (~50 sites) have been migrated.
- **`clippy::disallowed_methods` workspace lint**: `src-tauri/clippy.toml` bans `tokio::process::Command::new` and `std::process::Command::new` with a clear error message. This makes a missing-`CREATE_NO_WINDOW` mistake structurally impossible going forward and will be caught in CI. Build scripts and integration-test binaries are properly opted out via `#[allow(clippy::disallowed_methods)]`.
- **Frontend file-change refresh debounce (250 ms)**: `IDE/index.tsx` now coalesces `ide-file-changed` events with a 250 ms trailing-edge debounce. A save-burst of 50 files from Koi agents now triggers one pair of `loadFileTree + loadGitStatus` instead of 50, eliminating the process storm that previously caused the agent to appear to be looping.



### Fixed
- **Vision image re-injection loop: same screenshot re-processed every iteration causing LLM to output identical content**: `inject_selected_context` injects selected vision artifacts into `req_messages` (a per-call local variable). Since `req_messages` is discarded after each LLM call, the persistent `messages` vector never received the vision analysis text. On every subsequent iteration, the same selected images were re-injected, the vision delegate (or main model) produced the same description, and the main LLM saw an identical context — causing it to output the same response repeatedly. Fixed by calling `vision::clear_selection()` immediately after images have been consumed by `inject_selected_context`, for both the vision-delegate path and the main-model-as-vision path. Agents can re-select via `vision_context(action="select")` if they need to examine an image again.
- **Todo reminder injection loop: agent loops indefinitely when it has unfinished todos but emits no tool calls**: when the LLM returned a text-only response with unfinished plan todos, the loop injected a reminder message and continued — but there was no upper bound, so if the model kept producing text without tool calls the reminder was injected forever. Added `TODO_REMINDER_MAX = 3` cap: after 3 consecutive reminder injections the loop exits normally.
- **Vision validation test image rejected by Qwen/Alibaba models**: the 1×1 transparent PNG used to probe vision support was rejected by Qwen3.6-plus with "image length and width do not meet model restrictions" (minimum 10×10). Replaced the test image with the project's own piscis icon (512×512, embedded via `include_bytes!`). Also added an `is_image_size_error` guard so image-dimension rejection is correctly interpreted as "model supports vision" rather than "model does not support vision".
- **`tauri.conf.json` version not bumped to match release tag**: Tauri reads `version` from `tauri.conf.json` to name build artefacts (e.g. `OpenPiscis_0.7.35_amd64.deb`). This field was left at 0.7.35 when the v0.7.36 tag was cut, so all artefact filenames showed the wrong version. Now kept in sync with `package.json` and `Cargo.toml`.

### Fixed
- **Vision model delegation broken: separate vision model ignored, "missing model parameter" error**: when a non-vision main model was paired with a separate vision model (e.g. DeepSeek + qwen3.6-plus), the vision model name was never passed to the API — `delegate_vision_analysis()` always used `model: ""` in the request, causing providers to reject it. Additionally, `vision_base_url` was never forwarded to the vision delegate client, breaking custom endpoints (DashScope, etc.). Fixed by:
  - Threading `vision_model` through `HarnessConfig` → `AgentLoop` → `delegate_vision_analysis()` so the actual configured model name reaches the API.
  - Passing `vision_base_url` to `build_client()` when constructing the vision delegate.
  - Adding `vision_model: String` to both `AgentLoop` and `HarnessConfig` structs with builder support.
- **Vision capability detection relied on brittle name-matching**: `model_supports_vision()` used substring checks (`qwen-vl`, `qwen3-vl`, etc.) that missed real multimodal models like `qwen3.6-plus`. While the heuristic is retained as a fallback, the authoritative check is now a **real API call at config save time** — if a configured vision model doesn't actually support images, the save is rejected with a clear error message.
- **Vision logic overrides user intent**: when `vision_use_main_llm=true` and `vision_enabled=false`, the old code still called `model_supports_vision()` to auto-detect vision — silently enabling vision on a text-only model when the user explicitly left it off. Fixed: `vision_capable` now strictly follows the user's `vision_enabled` flag; no silent auto-detection.
- **Updated `model_supports_vision()` patterns**: added `qwen3.6-plus`, `qwen3-plus`, `qwen-omni`, `o4`, `claude-4` to both the kernel-level (`openai.rs`) and command-level (`chat.rs`) heuristics for improved first-guess accuracy.

### Changed
- **Vision model validation at settings save time**: when vision-related fields change, `save_settings` now calls `validate_vision_model()` — a real API call with a minimal test image — to verify the configured model actually supports vision. If validation fails, the settings are not saved and an error is returned to the frontend.
- **Unified `vision_capable` logic** across all 5 call sites (`chat_send`, `run_agent_headless`, `call_fish`, `call_koi`, debug paths): no more scattered inline comments and divergent logic.

## [0.7.25] - 2026-05-21

### Fixed
- **`desktop_automation` PowerShell/cmd windows steal focus on Windows**: every `move_mouse`, `click`, `type_text`, `hotkey`, `list_windows`, `activate_window`, and `launch_app` invocation was spawning a visible console window, stealing focus and potentially obscuring screen captures. Added `CREATE_NO_WINDOW` (0x08000000) flag to the shared `run_cmd()` helper so all Windows subprocess launchers across `desktop_automation` run silently in the background.
- **Vision model 400 error on DashScope / compatible APIs**: when images were present, the OpenAI message converter flushed `pending_vision` as a user message whose content array contained only `image_url` items without a leading `text` item. Some providers (e.g. DashScope compatible-mode) reject this with "Unexpected item type in content." Fixed by prepending a short text placeholder (`[Tool-generated image(s)]`) to any image-only content array.
- **Missing i18n keys in chat workspace dropdown**: the chat input area's workspace directory selector now shows properly translated labels (`workspaceBrowse`, `workspaceLabel`, `workspaceReset`) instead of raw i18n keys.

## [0.7.24] - 2026-05-21

### Fixed
- **Koi duplicate todos (one done, one pending forever)**: `assign_koi` was creating a kanban todo AND then calling `handle_mention`, which unconditionally created a second todo for the `@!` mention. The Koi executed the duplicate while the original stayed "todo" forever. Fixed by dispatching `execute_todo_turn` directly with the pre-created todo instead of routing through `handle_mention`.
- **Koi Observer (观察台) showing empty despite active/completed tasks**: the frontend `isKoiObserverSession` filter only matched `koi_runtime_` and `koi_notify_` session-ID prefixes, but all Koi task sessions actually use `koi_task_` prefix. Added `koi_task_` to the filter so work records are visible.
- **Koi activation prompt now guards against duplicate todo creation**: `koi_activate_for_messages.txt` now instructs Kois to check `pool_org(action="get_todos")` for existing matching todos before creating a new one — a second layer of defense against future duplication bugs.

## [0.7.23] - 2026-05-21

### Fixed
- **Cancelled streaming replies now keep partial assistant text**: if a streaming LLM response is cancelled after some tokens have already arrived, Piscis now preserves that partial assistant output in session history instead of dropping it, and a regression test covers this path.

## [0.7.22] - 2026-05-21

### Fixed
- **Release pipeline rustfmt regression**: formatted the WeChat iLink media gateway changes to match `cargo fmt --check`, unblocking CI and the tag-based release build for the real-media WeChat fix shipped in v0.7.21.

## [0.7.21] - 2026-05-21

### Fixed
- **WeChat inbound media now arrives as real files**: image, voice, file, and video messages from the iLink gateway are now downloaded from the WeChat CDN, decrypted with AES-128-ECB, and forwarded into Piscis with real attachment bytes instead of placeholder notifications.
- **WeChat outbound media compatibility**: outbound CDN media payloads now encode `aes_key` in the iLink SDK's base64-hex format, improving compatibility with WeChat clients that previously showed attachments as expired or already cleared.

## [0.7.19] - 2026-05-10

### Added
- **Explicit IM channel tools for agents**: added `im_channel_list`, `im_channel_connect`, `im_channel_binding_lookup`, and `im_channel_binding_list` so agents can inspect configured channels, connect them on demand, and resolve binding keys without relying on implicit routing.

### Fixed
- **Scheduled-task IM delivery continuity**: successful scheduler and notification sends now create any missing IM session/binding on demand and mirror outbound messages into the target IM conversation history, so follow-up replies keep the right context.
- **Duplicate scheduled job registration after restart**: scheduled tasks now track and replace prior cron job IDs during restore and CRUD updates, preventing one task from being registered multiple times across restarts.
- **Gateway connection badge stale after background connect**: the chat and settings views now refresh their IM channel status when backend gateway state changes, so the UI reflects successful background connections immediately.
- **Chat workspace selector UX**: browsing or resetting a session workspace now keeps the selector display in sync with the just-chosen path, and buffered streaming deltas are flushed before segment boundaries and terminal events.

### Changed
- **Settings copy and defaults**: refreshed provider labels, multi-provider editor text, and language labels in Settings, and inlined the default heartbeat prompt string so it no longer depends on an eager i18n lookup during module initialization.

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
  `get_messages_older` in [db.rs](src-tauri/piscis-kernel/src/store/db.rs) to
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
  (`piscis-xi-helper`) uses `XIWarpPointer` on the master pointer (device id=2)
  plus `XTestFakeMotionEvent` to deliver events reliably. `move_mouse` /
  `drag` now execute a 20-step smooth motion matching Windows UIA behavior,
  and events reach WebKit correctly even though the visible cursor stays put
  under VMware.
- **IM send auto-resolve**: `im_send_message` now automatically resolves the
  IM binding from the current `session_id` when no explicit `binding_key` or
  `channel`+`recipient` is provided, so IM-driven replies don't need explicit
  addressing parameters.
- Minor borrow fix in `piscis-kernel::agent::loop_` cancellation path.

### Changed
- `screen_capture` default `grid_spacing` is now 100 (was 200); label interval
  auto-adjusts to every 2nd line when spacing is under 200px to avoid overlap.
- Ball and target in the UIA test panel display screen-absolute coordinate
  labels for debugging and verification.

## [Unreleased]

### Documentation
- **Multi-agent architecture docs**: README (Chinese and English) now explains the
  roles and boundaries of Piscis, Koi, and Fish, plus the structure of the Pond
  workspace and the collaboration lifecycle.

### Changed
- **Heartbeat guardrails**: Piscis heartbeat now treats follow-up signals without
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
