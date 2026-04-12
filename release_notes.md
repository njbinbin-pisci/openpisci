## v0.5.23 - Release asset upload fix

### Bug Fixes

- **GitHub Actions / release upload**: Fix Windows binary and installer upload paths so tagged builds publish actual downloadable assets to GitHub Release
- **GitHub Actions / release validation**: Make artifact upload and release attachment fail when installer files are missing, instead of silently succeeding

### Previous releases

#### v0.5.22 - Windows startup crash fix and release pipeline stabilisation

### Bug Fixes

- **startup/runtime**: Fix installed Windows builds crashing during startup when background tasks touch `AppState` before registration completes
- **commands/test_runner**: Fix the `replace_todo` resume-path deadlock in Rust tests
- **GitHub Actions / Windows tests**: Embed the required Windows manifest so Rust test binaries no longer fail with `STATUS_ENTRYPOINT_NOT_FOUND`

### Documentation

- **README.md / README_CN.md**: Backfill `v0.5.20` / `v0.5.21` / `v0.5.22` changelog entries and switch the repository default landing page to English
- **src-tauri/README.md / src-tauri/README_CN.md**: Split backend docs into English and Chinese entrypoints with corrected cross-links

### Previous releases

#### v0.5.21 - Layered timeouts and context runtime consolidation
- Added layered task timeout inheritance across task, pool, Koi, and system defaults
- Unified context assembly, rolling-summary compaction controls, and minimal task-spine persistence

#### v0.5.20 - Settings fix and documentation refresh
- Fixed custom LLM providers disappearing after saving settings
- Added screenshots, star prompt, and updated licensing notes
- Known issue: some Windows installers could crash on startup; fixed in `v0.5.22`

#### v0.5.19 - Koi/Pool fixes, parity matrix, backend README

### Bug Fixes

- **koi/runtime**: Fix stalled project unblocking logic when a Koi times out
- **pool_org**: Fix pool project management edge cases

### Documentation

- **docs/openclaw-parity-matrix.md**: Full audit and update of PisciDesktop vs OpenClaw capability matrix; corrected statuses for Slack/Discord/Teams/Matrix (partial), resume-after-restart (implemented), prompt injection (implemented), multi-agent routing (implemented), email (implemented); added new rows for UAC elevation, PDF, SSH, code execution, web search, vision, WMI, MCP, secret encryption; added PisciDesktop-specific multi-agent collaboration section
- **src-tauri/README_CN.md**: Replaced incorrect content with proper OpenPisci Rust backend documentation

### Previous releases

#### v0.5.18
- fix(office): fix Excel chart type and sheet_check logic
- fix(clippy): use next_back() instead of last() on DoubleEndedIterator

#### v0.5.16 - UAC Elevated Execution Fix
- UAC elevated execution now works correctly for native executables (regsvr32, reg, regasm)
- Fixed UTF-8 BOM in result file causing JSON parse failure
- Fixed $LASTEXITCODE not captured for native executables via Start-Process inner script
- 32-bit PowerShell preserved for powershell32 interpreter

#### v0.5.15 - Real-time Message Persistence
- Every agent message written to DB immediately (not batch on run end)
- Prevents message loss on mid-run exits (crash, recompile, process kill)
