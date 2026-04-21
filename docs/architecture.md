# OpenPisci Cross-Platform Agent Architecture

OpenPisci is a long-running agent runtime that must work in three very
different environments:

1. **Desktop** — Tauri app on Windows, rich UI, platform tools (UIA, screen
   capture, PowerShell / WMI / COM, browser automation, IM gateways).
2. **Headless CLI** — `openpisci-headless` binary, used by benchmark scripts
   (SWE-lite), CI harnesses and IDE integrations. No UI; reads JSON requests
   from stdin/args, streams NDJSON events to stdout.
3. **Future hosts** — web server deployments, mobile shells, plugin embeds.

The codebase is split into four Rust crates arranged as a Cargo workspace
rooted at `src-tauri/`:

```text
src-tauri/
├── Cargo.toml          # workspace manifest (members + shared deps)
├── pisci-core/         # pure data model + host trait contracts
├── pisci-kernel/       # OS/UI-neutral agent runtime
├── pisci-cli/          # headless host adapter + openpisci-headless bin
└── src/                # pisci-desktop: Tauri host adapter (the UI shell)
```

## Crate responsibilities

### `pisci-core` — contracts only, no I/O

* Stable schema types shared by every crate and by external tools
  (`HeadlessCliRequest`, `HeadlessCliResponse`, `HeadlessContextToggles`,
  `KoiDefinition`, starter-koi specs, etc.).
* The host traits that decouple the kernel from any concrete host:
  * `EventSink` — publish agent events (`emit_session`, `emit_broadcast`).
  * `Notifier` — surface toasts, request yes/no confirmation or a rich
    interactive prompt; async so the desktop host can `.await` a oneshot
    channel backed by the Tauri front-end.
  * `HostTools` — inject platform-specific tools into the kernel's
    `ToolRegistry`.
  * `SecretsStore` — encrypted read/write of API keys and OAuth tokens.
  * `HostRuntime` — aggregate trait bundling the four above plus
    `app_data_dir()`.

`pisci-core` intentionally depends only on `serde`, `serde_json`, `chrono`,
`async-trait` and `anyhow`. No Tokio, no reqwest, no rusqlite, no Tauri.

### `pisci-kernel` — the agent runtime

Owns every piece of OpenPisci that should behave identically on every host:

* `agent/` — the `AgentLoop`, `HarnessConfig` harnesses (main-chat, Koi,
  Fish, debug, scheduler), plan state, compaction v2 kernel, context
  builder, tool dispatcher, vision helpers, message utilities.
* `llm/` — Anthropic/OpenAI/Qwen/DeepSeek/… client adapters on `reqwest`.
* `store/` — SQLite database (`Database`) and encrypted `Settings`.
* `memory/` — long-term memory store + Dream consolidation.
* `policy/` — tool-use policy, denylists, approval rules.
* `scheduler/` — `tokio-cron-scheduler` wrapper used by recurring tasks.
* `security/` — secret encryption (`chacha20poly1305`) + prompt-injection
  heuristics.
* `project_context/` — project-directory file tree summariser.
* `tools/` — **platform-neutral** tool implementations
  (`file_read/write/list/search/diff`, `shell`, `code_run`,
  `process_control`, `web_search`, `email`, `pdf`, `ssh`, `recall_tool`,
  `memory_tool`, `vision_context`, `mcp`, `user_tool`, Windows-only
  `elevate` helper used by the shell tool under
  `#[cfg(target_os = "windows")]`).

The kernel **never** imports Tauri and **never** holds an `AppHandle`. When
it needs to surface an event, prompt the user, look up a secret, or discover
a platform-specific tool, it goes through an `Arc<dyn HostRuntime>` handed
in at construction time.

### `pisci-cli` — headless host adapter

Tiny crate that implements the host traits for a non-interactive
environment:

* `CliEventSink` — NDJSON to stdout (one line per event).
* `CliNotifier` — toasts go to stderr; confirmation/interactive requests
  resolve to the request's `default` (benign fallback).
* `CliHostTools` — no-op (headless runs rely on the kernel's neutral
  tools).
* `CliSecretsStore` — environment variables.
* `CliHost` — bundles the above into `HostRuntime`.
* `openpisci-headless` binary — fully host-agnostic CLI entry point.
  Exposes three subcommands:
  * `capabilities [--mode pisci|pool]` — prints a JSON report of OS, mode,
    kernel version, and the list of disabled (desktop-only) tools.
  * `version` — prints the kernel version string.
  * `run --prompt <text> …` — runs a single `pisci`-mode agent turn
    entirely through the kernel via
    [`pisci_cli::runner::run_pisci_once`], which in turn drives
    [`pisci_kernel::headless::run_pisci_turn`]. It boots a `CliHost`,
    opens the kernel DB/settings under `OPENPISCI_CONFIG_DIR`, registers
    only neutral tools, and streams `AgentEvent`s as NDJSON on stdout.
    `pool` mode is rejected and must use the desktop `openpisci` binary.
* `pisci_cli::runner::run_pisci_once(request)` — shared helper that both
  `openpisci-headless` and the desktop `openpisci` binary dispatch into
  whenever `mode == Pisci`, guaranteeing that a single code path owns
  tool registration, event-sink wiring, timeout semantics, and response
  shape. Heavyweight pool / koi delegation runs still live in
  `pisci-desktop`'s Tauri-backed `run_headless_cli`, because they need
  `AppState` and per-koi notification channels.

### `pisci-desktop` — Tauri host adapter

The pre-existing Tauri app. After the refactor its role is:

* Own the main window, tray, gateways, scheduler bootstrap, scenes and the
  `AppState` that groups them.
* Provide the `DesktopHost` implementation of `HostRuntime`
  (`src/host.rs`):
  * `DesktopEventSink` maps `emit_session/emit_broadcast` onto Tauri's
    event bus.
  * `DesktopNotifier` routes toasts through a `host://toast` event and
    backs `request_confirmation` / `request_interactive` with the
    oneshot-channel maps already kept in `AppState`.
  * `DesktopHostTools` — implements `HostTools::register`, which first
    calls `pisci_kernel::tools::register_neutral_tools` to install the
    shared neutral set and then layers platform-specific tools on top
    (browser, UIA, screen, app_control, plan_todo, chat_ui, call_fish/koi,
    pool_org, pool_chat, PowerShell, WMI, COM, Office, skill_list).
    `DesktopHostTools::build_registry(self)` is the canonical one-shot
    helper: scene / koi / fish / scheduler / debug call sites construct a
    fresh `DesktopHostTools` with the fields they need and materialise a
    populated `ToolRegistry` without touching `ToolRegistryHandle`
    directly.
  * `DesktopSecretsStore` — bridges API-key fields inside `Settings`
    using a synchronous `block_in_place` over the async mutex.
* Keep all Tauri-coupled tools in `src/tools/` (`app_control`, `browser`,
  `call_fish`, `call_koi`, `chat_ui`, `plan_todo`, `pool_chat`, `pool_org`,
  `skill_list`, and the Windows-only `com_invoke`, `com_tool`, `screen`,
  `uia`, `powershell`, `wmi_tool`, `office`, `dpi`).
* Transparently re-export kernel modules via `pub use pisci_kernel::...`
  so legacy `crate::agent::...`, `crate::llm::...`, `crate::store::...`
  call sites inside `pisci-desktop` keep resolving without edits.

## Key design decisions

* **One-time kernel extraction.** Rather than a gradual strangler
  migration, the whole agent runtime moved to `pisci-kernel` in a single
  pass. This keeps the kernel API consistent between crates and avoids
  half-migrated code paths that drift apart over time.
* **Plan state over AppHandle.** The old `AgentLoop::app_handle:
  Option<tauri::AppHandle>` is replaced with a concrete `PlanStateHandle =
  Arc<Mutex<HashMap<String, Vec<PlanTodoItem>>>>`. The kernel therefore
  does not need to know whether a Tauri handle even exists.
* **Public `Database.conn`.** The raw `rusqlite::Connection` inside
  `Database` is `pub` so host crates can run ad-hoc migrations and
  test-harness queries without adding bespoke kernel accessors. The
  idiomatic path is still through `Database` methods.
* **Platform tools stay in the host crate.** Moving browser/UIA/screen
  tools into the kernel would drag `chromiumoxide`, `uiautomation` and
  Windows crates into every headless build. Instead they live next to
  their glue code in `pisci-desktop` and reach the kernel via
  `HostTools::register`.
* **Shared headless schema.** `HeadlessCliRequest` / `Response` and the
  context toggles moved to `pisci-core` so Python benchmark scripts,
  Tauri command handlers, and the CLI crate all deserialize the same
  shape.
* **CI matrix.** The workflow now builds
  `pisci-core + pisci-kernel + pisci-cli` on Ubuntu, macOS, and Windows
  (kernel tests must work on every platform). Only the Tauri bundle and
  desktop clippy stay Windows-only.

## Working with the workspace

```bash
# From src-tauri/

# Kernel-only lint + tests (any OS):
cargo clippy -p pisci-core -p pisci-kernel -p pisci-cli --all-targets -- -D warnings
cargo test   -p pisci-core -p pisci-kernel -p pisci-cli --lib --bins

# Headless binary:
cargo build -p pisci-cli --release --bin openpisci-headless

# Full desktop build (Windows):
cargo build --release -p pisci-desktop
```

`bench_swe_lite` continues to look for `openpisci[.exe]` under
`src-tauri/target/{debug,release}/` — that binary still ships with
`pisci-desktop` because pool orchestration and koi delegation need Tauri
`AppState`. Single-turn `pisci`-mode runs now have a fully independent
kernel path available via `pisci-cli`'s `openpisci-headless run`.

## Future work

* Migrate pool-mode orchestration (`pool_org` / `pool_chat` tools plus
  the koi notification rx) into a kernel-level scheduler so
  `openpisci-headless` can accept `--mode pool` without depending on
  Tauri. Today pool runs must still invoke `pisci-desktop`'s `openpisci`
  binary — the *only* remaining hard dependency of the headless story on
  the desktop crate.
* Route pool-mode UI flows (in-app task board, Koi status) through the
  same NDJSON event contract the CLI uses, so a future headless pool
  runner can share the wire format unchanged.

### History — completed refactor items

The following items from the original roadmap are now shipped and
verified in CI:

* `HostTools::register` / `DesktopHostTools::build_registry` are the
  single path to a populated `ToolRegistry`. The old
  `tools::build_registry` free function (and its multi-arg signature)
  has been retired — scene / koi / fish / scheduler / debug / system
  call sites build a `DesktopHostTools` struct literal directly.
* `pisci-core::host::ToolRegistryHandle` exposes a type-safe downcast
  API (`downcast_ref`, `with_mut`, `into_inner`, `type_name`). The
  kernel adds `ToolRegistryHandleExt` so hosts can `register_tool` /
  `as_registry_mut` / `into_registry` without unsafe casts.
* `openpisci-headless` now runs a true kernel-only agent loop via
  `pisci_kernel::headless::run_pisci_turn` with a `CliHost` driver.
* Desktop `openpisci --mode pisci` no longer boots a Tauri runtime:
  pisci-mode turns short-circuit into `pisci_cli::runner::run_pisci_once`
  so the two headless binaries share exactly one code path. Only
  `--mode pool` still boots `AppState` + Tauri.
* Linux CI gained an opt-in end-to-end LLM smoke test
  (`e2e_run_returns_answer_with_real_api_key`) that skips silently when
  `OPENPISCI_TEST_API_KEY` is absent and drives a full kernel turn via
  the compiled `openpisci-headless` binary when it is configured.
* A dedicated Linux step in `.github/workflows/ci.yml` runs
  `cargo test -p pisci-cli --test headless_cli` to guard the CLI
  surface (capabilities schema, pool rejection, arg validation, version
  banner) against drift.
* `pisci-desktop::headless_cli` is a thin adapter — request /
  response / toggle schemas come from `pisci_core::host`, argument
  parsing comes from `pisci_cli::args`. No duplicate type or parser
  definitions remain in the desktop crate.
* `pisci-desktop 0.7.0` drops several compatibility scaffolds: the
  `tools::build_registry` shim, the `RuntimeToolProfile::Desktop`
  no-op variant, the duplicate `strip_send_markers` in
  `commands/chat.rs`, and the `_unused_imports_placeholder` hook in
  `host.rs` are all gone.
