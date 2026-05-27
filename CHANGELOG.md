# Changelog

All notable changes to Pisci Desktop are documented here.
This project follows [Semantic Versioning](https://semver.org/) and
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) conventions.

---

## [0.8.10] - 2026-05-28

### Fixed
- **Vision-model screenshot-analyze loop**: when driving desktop automation through a vision model (WeChat/QQ input, screen-control tasks), the agent occasionally gets stuck in a "describe-then-verify" loop — `move_mouse → take_screenshot → screen_analyze → move → screenshot → analyze …` — repeatedly re-describing the scene without ever committing to the target action (click, type, press Enter). Generic loop detectors (same-input repeat, no-progress streak, ping-pong) all miss this because each iteration uses fresh coordinates / fresh screenshots / different analysis prompts, so hashes differ. Added a dedicated **VisionLoop density detector** in `pisci-kernel/src/agent/loop_.rs`: if 3 or more of the last 8 tool calls are screenshot / vision-analyze (matched by substring against `screenshot`, `screen_analyze`, `screen_capture`, `vision_analyze`, `vision_context`), a Warning-level nudge is injected telling the model to stop over-verifying and commit to a concrete action sequence (click input → type text → press Enter, all in one turn, no more screenshots). A cooldown of 3 real-action turns prevents the nudge from firing on every single iteration. The screenshot tool itself is never *blocked* (only nudged) — legitimate multi-step vision work still proceeds, just with periodic prompts to keep the agent on-task.

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
- **Pisci CLI assistant panel**: new 🤖 button in the right-side icon strip (above the terminal toggle) opens a CLI-style chat in the same bottom slot as the terminal. Designed for users unfamiliar with shell commands — ask in plain language ("build the project", "git status", "find TODOs in src/") and Pisci runs the corresponding actions through its standard tool stack. Implementation: `src/components/Pond/IDE/AssistantPanel.tsx` lazily creates a per-project `Pisci CLI — {project}` session bound to the project workspace, streams `agent_event_*` `text_delta` to a monospaced log, and surfaces tool start/end + errors as muted lines. Terminal and assistant are mutually exclusive in the bottom slot; toggling one closes the other. Localized via `ide.assistant`, `ide.assistantTitle`, `ide.assistantHint`, `ide.assistantInputPlaceholder`, `ide.assistantSend`, `ide.assistantClear`.

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
- **@!Pisci no response in chat room**: `coordinator::handle_mention` only records `@!Pisci` as a board todo (because Pisci is not a Koi and `activate_pending_todos` skips `db.get_koi("pisci") == None`); execution then waited for the periodic heartbeat timer (up to `heartbeat_interval_mins`). Added an immediate `dispatch_heartbeat` fan-out from `send_pool_message` so `@!Pisci` mentions are processed right away.
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
- **@!Pisci mention dispatch**: sending `@!Pisci` in a new project now correctly creates a board todo owned by the Pisci coordinator; previously `parse_mention_targets` only iterated DB Kois and Pisci (not being a Koi) was silently ignored.
- **Koi stuck in "busy" state**: when Pisci manually completes a Koi's todo via `pool_org`, the Koi's status is now reset to "idle" instead of remaining permanently busy.
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
  - New `pisci-desktop` `lsp` module: `LspManager` spawns and lifecycle-manages per-project+language LSP processes (rust-analyzer, typescript-language-server, pyright, clangd) auto-detected from `PATH`. Each session gets its own WebSocket bridge that frames LSP JSON-RPC for the front-end.
  - Monaco IDE wires to the bridge through `monaco-languageclient` + `vscode-ws-jsonrpc`, delivering diagnostics, hover, completion, go-to-definition, references, document symbols, and rename inside the embedded editor.
  - New Tauri commands `ide_lsp_list_languages` / `ide_lsp_start` / `ide_lsp_stop`.
- **`lsp` agent tool**: actions `diagnostics`, `hover`, `complete`, `definition`, `references`, `rename`. The agent can now navigate and understand code without grepping.
- **`read_lints` agent tool** (Cursor's ReadLints parity): batch multi-file diagnostics with `severity` filter (`error` / `warning` / `all`) and tunable `wait_ms`. Use it after edits to verify code quickly via the running LSP servers.

---

## [0.8.2] - 2026-05-25

### Fixed
- **Windows console popup on every IDE Search / enterprise capability check**: `rg` (search) and `npx`/`npx.cmd` (enterprise node check) were spawned without `CREATE_NO_WINDOW`, causing a blue console window to flash on every keystroke in the Search panel and every time the enterprise settings page was opened. Both sites now use the new centralised spawn helper.

### Added
- **`pisci_kernel::proc::{tokio_command, std_command}` — centralised popup-safe spawn helpers**: a single module now applies `CREATE_NO_WINDOW = 0x0800_0000` on Windows for every child process spawned by the application. All call sites across `pisci-kernel` and `pisci-desktop` (~50 sites) have been migrated.
- **`clippy::disallowed_methods` workspace lint**: `src-tauri/clippy.toml` bans `tokio::process::Command::new` and `std::process::Command::new` with a clear error message. This makes a missing-`CREATE_NO_WINDOW` mistake structurally impossible going forward and will be caught in CI. Build scripts and integration-test binaries are properly opted out via `#[allow(clippy::disallowed_methods)]`.
- **Frontend file-change refresh debounce (250 ms)**: `IDE/index.tsx` now coalesces `ide-file-changed` events with a 250 ms trailing-edge debounce. A save-burst of 50 files from Koi agents now triggers one pair of `loadFileTree + loadGitStatus` instead of 50, eliminating the process storm that previously caused the agent to appear to be looping.



### Fixed
- **Vision image re-injection loop: same screenshot re-processed every iteration causing LLM to output identical content**: `inject_selected_context` injects selected vision artifacts into `req_messages` (a per-call local variable). Since `req_messages` is discarded after each LLM call, the persistent `messages` vector never received the vision analysis text. On every subsequent iteration, the same selected images were re-injected, the vision delegate (or main model) produced the same description, and the main LLM saw an identical context — causing it to output the same response repeatedly. Fixed by calling `vision::clear_selection()` immediately after images have been consumed by `inject_selected_context`, for both the vision-delegate path and the main-model-as-vision path. Agents can re-select via `vision_context(action="select")` if they need to examine an image again.
- **Todo reminder injection loop: agent loops indefinitely when it has unfinished todos but emits no tool calls**: when the LLM returned a text-only response with unfinished plan todos, the loop injected a reminder message and continued — but there was no upper bound, so if the model kept producing text without tool calls the reminder was injected forever. Added `TODO_REMINDER_MAX = 3` cap: after 3 consecutive reminder injections the loop exits normally.
- **Vision validation test image rejected by Qwen/Alibaba models**: the 1×1 transparent PNG used to probe vision support was rejected by Qwen3.6-plus with "image length and width do not meet model restrictions" (minimum 10×10). Replaced the test image with the project's own pisci icon (512×512, embedded via `include_bytes!`). Also added an `is_image_size_error` guard so image-dimension rejection is correctly interpreted as "model supports vision" rather than "model does not support vision".
- **`tauri.conf.json` version not bumped to match release tag**: Tauri reads `version` from `tauri.conf.json` to name build artefacts (e.g. `OpenPisci_0.7.35_amd64.deb`). This field was left at 0.7.35 when the v0.7.36 tag was cut, so all artefact filenames showed the wrong version. Now kept in sync with `package.json` and `Cargo.toml`.

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
- **Cancelled streaming replies now keep partial assistant text**: if a streaming LLM response is cancelled after some tokens have already arrived, Pisci now preserves that partial assistant output in session history instead of dropping it, and a regression test covers this path.

## [0.7.22] - 2026-05-21

### Fixed
- **Release pipeline rustfmt regression**: formatted the WeChat iLink media gateway changes to match `cargo fmt --check`, unblocking CI and the tag-based release build for the real-media WeChat fix shipped in v0.7.21.

## [0.7.21] - 2026-05-21

### Fixed
- **WeChat inbound media now arrives as real files**: image, voice, file, and video messages from the iLink gateway are now downloaded from the WeChat CDN, decrypted with AES-128-ECB, and forwarded into Pisci with real attachment bytes instead of placeholder notifications.
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
