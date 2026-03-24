# PisciDesktop vs OpenClaw Capability Matrix

This matrix is the executable baseline for parity tracking.

Status values:
- `implemented`: available and usable
- `partial`: available but missing key scenarios
- `planned`: accepted in roadmap, not yet shipped
- `not_supported`: out of scope for now

_Last updated: 2026-03-24_

---

## Channels

| Capability | OpenClaw | PisciDesktop | Notes | Module |
|---|---|---|---|---|
| Telegram | implemented | implemented | Long-poll `getUpdates`, `sendMessage`, `getMe` | `src-tauri/src/gateway/telegram.rs` |
| Feishu / Lark | implemented | implemented | WS Protobuf frames, `im.message.receive_v1`, send text/image/file | `src-tauri/src/gateway/feishu.rs` |
| DingTalk | implemented | implemented | OAuth token, `batchSend`, Stream WS + ACK | `src-tauri/src/gateway/dingtalk.rs` |
| WeChat (iLink relay) | implemented | partial | Local iLink HTTP relay; outbound works; plugin-side `sendmessage` / upload are stubs | `src-tauri/src/gateway/wechat.rs` |
| WeCom inbound + outbound | implemented | partial | `gettoken`, send text, connection check; inbound requires `inbox_file` JSONL relay | `src-tauri/src/gateway/wecom.rs` |
| Slack | implemented | partial | Outbound Webhook POST works; inbound is a sleep-loop placeholder | `src-tauri/src/gateway/slack.rs` |
| Discord | implemented | partial | Outbound Webhook POST works; inbound is a sleep-loop placeholder | `src-tauri/src/gateway/discord.rs` |
| Microsoft Teams | implemented | partial | Outbound Teams Webhook works; inbound not implemented | `src-tauri/src/gateway/teams.rs` |
| Matrix | implemented | partial | Outbound `PUT .../send/m.room.message` works; inbound sync not implemented | `src-tauri/src/gateway/matrix.rs` |
| Generic webhook channel | implemented | partial | Outbound POST + optional Bearer works; inbound not enabled | `src-tauri/src/gateway/webhook.rs` |

---

## Automation & Triggers

| Capability | OpenClaw | PisciDesktop | Notes | Module |
|---|---|---|---|---|
| Cron jobs | implemented | implemented | `tokio-cron-scheduler`, 5→7 segment conversion, add/remove at runtime | `src-tauri/src/scheduler/cron.rs` |
| Webhook / event trigger | implemented | partial | `trigger_task_by_event` command exists with payload injection; no inbound HTTP listener yet | `src-tauri/src/commands/scheduler.rs` |
| Email trigger | implemented | planned | API stub exists; no IMAP-triggered scheduling wired up | `src-tauri/src/commands/scheduler.rs` |
| Retry policy | implemented | partial | LLM calls have exponential backoff + transient-error retry; task-level retry not configurable | `src-tauri/src/agent/loop_.rs` |
| Resume after restart | implemented | implemented | Checkpoint written every iteration; loaded and consumed on next run | `src-tauri/src/agent/loop_.rs` |

---

## Windows Tooling

| Capability | OpenClaw | PisciDesktop | Notes | Module |
|---|---|---|---|---|
| Browser automation | implemented | implemented | Playwright-style CDP control | `src-tauri/src/tools/browser.rs` |
| Desktop UI automation | implemented | implemented | UIA tree walk, click, type, find | `src-tauri/src/tools/uia.rs` |
| Screen capture | implemented | implemented | Full / region capture | `src-tauri/src/tools/screen.rs` |
| COM / clipboard / shell bridge | implemented | implemented | `com_invoke`, `com_tool`, `shell`, `elevate` (UAC) | `src-tauri/src/tools/com_invoke.rs` |
| Office automation | partial | implemented | Word/Excel/Outlook via COM; Outlook local COM read/send | `src-tauri/src/tools/office.rs` |
| Download orchestration | implemented | partial | Browser download works; no dedicated download manager | `src-tauri/src/tools/browser.rs` |
| UAC elevation | not_supported | implemented | Auto-detect permission errors, retry via ShellExecuteW runas | `src-tauri/src/tools/elevate.rs` |
| PDF manipulation | not_supported | partial | Read text/info, split, watermark, fill form, annotate; encrypt is copy-only; merge/convert need Ghostscript | `src-tauri/src/tools/pdf.rs` |
| SSH remote execution | not_supported | implemented | `russh` session pool, password/key auth, `exec` with output truncation | `src-tauri/src/tools/ssh.rs` |
| Code execution (sandbox) | not_supported | implemented | Async exec with cwd/timeout/env, failure heuristics | `src-tauri/src/tools/code_run.rs` |
| Web search | not_supported | implemented | Parallel DDG / Bing / Baidu / 360, dedup + merge | `src-tauri/src/tools/web_search.rs` |
| Vision / multimodal context | not_supported | implemented | Session-scoped image list, add/select/clear, vision model dispatch | `src-tauri/src/tools/vision_context.rs` |
| WMI queries | not_supported | implemented | PowerShell WMI bridge | `src-tauri/src/tools/wmi_tool.rs` |
| MCP tool proxy | not_supported | implemented | stdio/SSE transport, initialize, tools/list, tools/call | `src-tauri/src/tools/mcp.rs` |

---

## Email

| Capability | OpenClaw | PisciDesktop | Notes | Module |
|---|---|---|---|---|
| SMTP send | implemented | implemented | `lettre` SMTP with auth | `src-tauri/src/tools/email.rs` |
| IMAP fetch / search | implemented | implemented | `imap` crate, fetch, search, header parse | `src-tauri/src/tools/email.rs` |
| Outlook local COM | not_supported | implemented | Read inbox, send, search via COM | `src-tauri/src/tools/office.rs` |

---

## Skills

| Capability | OpenClaw | PisciDesktop | Notes | Module |
|---|---|---|---|---|
| Built-in skills | implemented | implemented | Seed skills bundled at startup | `src-tauri/src/skills/loader.rs` |
| Workspace skills | implemented | implemented | Scan `SKILL.md`, YAML frontmatter, platform/`where` compatibility checks | `src-tauri/src/skills/loader.rs` |
| Managed / registry skills | implemented | planned | No remote registry; local directory only | `src-tauri/src/skills/loader.rs` |
| Skill permissions | implemented | planned | No per-skill permission gates yet | `src-tauri/src/skills/loader.rs` |

---

## Security & Governance

| Capability | OpenClaw | PisciDesktop | Notes | Module |
|---|---|---|---|---|
| Policy gate | implemented | implemented | Path / command / URL / UIA / COM / tool-call checks with allow/deny/confirm | `src-tauri/src/policy/gate.rs` |
| Approval flow | implemented | implemented | Per-tool `needs_confirmation`, user confirm/deny via frontend event | `src-tauri/src/agent/loop_.rs` |
| Prompt injection detection | implemented | implemented | Multi-rule regex scoring, severity levels, base64 heuristics, unit tests | `src-tauri/src/security/injection.rs` |
| Secret encryption at rest | not_supported | implemented | ChaCha20-Poly1305 with local 32-byte key | `src-tauri/src/security/secrets.rs` |
| Audit log | implemented | implemented | All messages + tool calls persisted to SQLite | `src-tauri/src/store/db.rs` |
| Tool rate limit | implemented | partial | `tool_rate_limit_per_minute` field exists in `PolicyConfig`; enforcement not wired into gate checks | `src-tauri/src/policy/gate.rs` |
| Redaction | implemented | planned | No PII/secret redaction in stored messages | `src-tauri/src/store/db.rs` |

---

## Session Routing

| Capability | OpenClaw | PisciDesktop | Notes | Module |
|---|---|---|---|---|
| Channel → session mapping | implemented | implemented | Gateway events mapped to IM sessions by sender ID | `src-tauri/src/lib.rs` |
| Group routing policies | implemented | partial | Basic source-based routing; no configurable group policies | `src-tauri/src/gateway/mod.rs` |
| Multi-agent routing (Koi / Fish) | implemented | implemented | `call_koi` (persistent), `call_fish` (stateless), pool_chat @mention routing | `src-tauri/src/tools/call_koi.rs` |

---

## Multi-Agent Collaboration (PisciDesktop-specific)

_These capabilities exist in PisciDesktop but have no direct OpenClaw equivalent._

| Capability | Status | Notes | Module |
|---|---|---|---|
| Pisci / Koi / Fish three-tier architecture | implemented | Persistent Koi agents with independent memory, todos and identity | `src-tauri/src/tools/call_koi.rs` |
| Pool Chat (@mention coordination) | implemented | Koi collaborate via `pool_chat`; @mention triggers async agent runs | `src-tauri/src/tools/pool_chat.rs` |
| Kanban / todo board | implemented | `koi_todos` with `todo→in_progress→done/blocked/cancelled` lifecycle | `src-tauri/src/tools/plan_todo.rs` |
| Pool project management | implemented | `pool_org` create/pause/resume/archive with `org_spec` | `src-tauri/src/tools/pool_org.rs` |
| Heartbeat / inbox patrol | implemented | Configurable cron heartbeat, Pisci Inbox session | `src-tauri/src/scheduler/cron.rs` |
| Loop detection | implemented | GenericRepeat / KnownPollNoProgress / PingPong / GlobalCircuitBreaker | `src-tauri/src/agent/loop_.rs` |
| Context compaction | implemented | Level-1 tool-result trim + Level-2 LLM summarisation with proactive trigger | `src-tauri/src/agent/loop_.rs` |
| Real-time message persistence | implemented | Every agent message written to DB immediately (not batch on run end) | `src-tauri/src/agent/loop_.rs` |
