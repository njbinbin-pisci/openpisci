# PisciDesktop vs OpenClaw Capability Matrix

This matrix is the executable baseline for parity tracking.

Status values:
- `implemented`: available and usable
- `partial`: available but missing key scenarios
- `planned`: accepted in roadmap, not yet shipped
- `not_supported`: out of scope for now

## Channels

| Capability | OpenClaw | PisciDesktop | Module |
|---|---|---|---|
| Telegram | implemented | implemented | `src-tauri/src/gateway/telegram.rs` |
| Feishu / Lark | implemented | implemented | `src-tauri/src/gateway/feishu.rs` |
| DingTalk | implemented | implemented | `src-tauri/src/gateway/dingtalk.rs` |
| WeCom inbound + outbound | implemented | partial | `src-tauri/src/gateway/wecom.rs` |
| Slack | implemented | planned | `src-tauri/src/gateway` |
| Discord | implemented | planned | `src-tauri/src/gateway` |
| Microsoft Teams | implemented | planned | `src-tauri/src/gateway` |
| Matrix | implemented | planned | `src-tauri/src/gateway` |
| Generic webhook channel | implemented | planned | `src-tauri/src/gateway` |

## Automation & Triggers

| Capability | OpenClaw | PisciDesktop | Module |
|---|---|---|---|
| Cron jobs | implemented | implemented | `src-tauri/src/scheduler/cron.rs` |
| Webhook trigger | implemented | planned | `src-tauri/src/commands/scheduler.rs` |
| Email trigger | implemented | planned | `src-tauri/src/commands/scheduler.rs` |
| Retry policy | implemented | partial | `src-tauri/src/commands/scheduler.rs` |
| Resume after restart | implemented | partial | `src-tauri/src/lib.rs` |

## Windows Tooling

| Capability | OpenClaw | PisciDesktop | Module |
|---|---|---|---|
| Browser automation | implemented | implemented | `src-tauri/src/tools/browser.rs` |
| Desktop UI automation | implemented | implemented | `src-tauri/src/tools/uia.rs` |
| Screen capture | implemented | implemented | `src-tauri/src/tools/screen.rs` |
| COM/clipboard/shell bridge | implemented | implemented | `src-tauri/src/tools/com_tool.rs` |
| Office automation | partial | implemented | `src-tauri/src/tools/office.rs` |
| Download orchestration | implemented | partial | `src-tauri/src/tools/browser.rs` |

## Email

| Capability | OpenClaw | PisciDesktop | Module |
|---|---|---|---|
| SMTP send | implemented | planned | `src-tauri/src/tools/email.rs` |
| IMAP fetch/search | implemented | planned | `src-tauri/src/tools/email.rs` |
| Outlook local COM | not_supported | implemented | `src-tauri/src/tools/office.rs` |

## Skills

| Capability | OpenClaw | PisciDesktop | Module |
|---|---|---|---|
| Built-in skills | implemented | implemented | `src-tauri/src/skills/loader.rs` |
| Workspace skills | implemented | partial | `src-tauri/src/skills/loader.rs` |
| Managed/registry skills | implemented | planned | `src-tauri/src/skills/loader.rs` |
| Skill permissions | implemented | planned | `src-tauri/src/skills/loader.rs` |

## Security & Governance

| Capability | OpenClaw | PisciDesktop | Module |
|---|---|---|---|
| Policy gate | implemented | implemented | `src-tauri/src/policy/gate.rs` |
| Approval flow | implemented | implemented | `src-tauri/src/agent/loop_.rs` |
| Prompt injection detection | implemented | partial | `src-tauri/src/security/injection.rs` |
| Audit log | implemented | implemented | `src-tauri/src/store/db.rs` |
| Rate limit / quotas | implemented | planned | `src-tauri/src/policy/gate.rs` |
| Redaction | implemented | planned | `src-tauri/src/store/db.rs` |

## Session Routing

| Capability | OpenClaw | PisciDesktop | Module |
|---|---|---|---|
| Channel -> session mapping | implemented | implemented | `src-tauri/src/lib.rs` |
| Group routing policies | implemented | partial | `src-tauri/src/gateway` |
| Multi-agent routing | implemented | planned | `src-tauri/src/lib.rs` |
