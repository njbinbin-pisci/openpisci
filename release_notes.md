## v0.5.19 - Koi/Pool fixes, parity matrix, backend README

### Bug Fixes

- **koi/runtime**: Fix stalled project unblocking logic when a Koi times out
- **pool_org**: Fix pool project management edge cases

### Documentation

- **docs/openclaw-parity-matrix.md**: Full audit and update of PisciDesktop vs OpenClaw capability matrix; corrected statuses for Slack/Discord/Teams/Matrix (partial), resume-after-restart (implemented), prompt injection (implemented), multi-agent routing (implemented), email (implemented); added new rows for UAC elevation, PDF, SSH, code execution, web search, vision, WMI, MCP, secret encryption; added PisciDesktop-specific multi-agent collaboration section
- **src-tauri/README.md**: Replaced incorrect content with proper OpenPisci Rust backend documentation

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
