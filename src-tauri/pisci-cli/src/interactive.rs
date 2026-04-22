//! Interactive REPL for `openpisci-headless`.
//!
//! Entered via `openpisci-headless chat` or by running the binary with no
//! arguments. Drives a multi-turn pisci-mode conversation against the
//! local kernel state (same `pisci.db` + `config.json` the desktop app
//! uses), streams assistant text to stdout as it arrives, and forwards
//! tool starts/ends to stderr so they do not pollute the response body.
//!
//! Design constraints:
//!   * Share one tokio runtime for the whole session (cheaper than
//!     rebuilding on every turn and keeps connection pools warm).
//!   * Reuse the same `session_id` across turns so the kernel's own
//!     history / compaction logic kicks in; `:new` starts a fresh one.
//!   * Never auto-approve destructive tools — policy gate and
//!     notifier defaults are unchanged from headless one-shot mode.
//!   * No NDJSON on stdout. This host is meant for human eyes; scripts
//!     should keep using `openpisci-headless run`.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Mutex as StdMutex;

use pisci_core::host::{
    EventSink, HeadlessCliMode, HeadlessCliRequest, HostRuntime, PoolEvent, PoolEventSink,
    ToolRegistryHandle,
};
use pisci_kernel::agent::plan::new_plan_store;
use pisci_kernel::agent::tool::{new_tool_registry_handle, ToolRegistryHandleExt};
use pisci_kernel::headless::{self, HeadlessDeps};
use pisci_kernel::tools::NeutralToolsConfig;
use serde_json::Value;

use crate::host::CliHost;
use crate::runner::resolve_app_data_dir;

/// Human-readable event sink used during an interactive session.
///
/// Streams assistant `text_delta` events straight to stdout and routes
/// tool / error events to stderr. Other events are dropped so the
/// terminal stays readable.
pub struct InteractiveEventSink {
    stdout: StdMutex<io::Stdout>,
    stderr: StdMutex<io::Stderr>,
    verbose_tools: bool,
}

impl InteractiveEventSink {
    pub fn new(verbose_tools: bool) -> Self {
        Self {
            stdout: StdMutex::new(io::stdout()),
            stderr: StdMutex::new(io::stderr()),
            verbose_tools,
        }
    }

    fn write_stdout(&self, text: &str) {
        if let Ok(mut out) = self.stdout.lock() {
            let _ = out.write_all(text.as_bytes());
            let _ = out.flush();
        }
    }

    fn write_stderr_line(&self, msg: &str) {
        if let Ok(mut err) = self.stderr.lock() {
            let _ = writeln!(err, "{msg}");
            let _ = err.flush();
        }
    }
}

impl EventSink for InteractiveEventSink {
    fn emit_session(&self, _session_id: &str, event: &str, payload: Value) {
        if event != "agent_event" {
            return;
        }
        // AgentEvent uses `#[serde(tag = "type", rename_all = "snake_case")]`,
        // so the discriminator lives in the `type` field.
        let Some(kind) = payload.get("type").and_then(Value::as_str) else {
            return;
        };
        match kind {
            "text_delta" => {
                if let Some(delta) = payload.get("delta").and_then(Value::as_str) {
                    self.write_stdout(delta);
                }
            }
            // Mark a new assistant bubble so the user can see when the
            // agent is recycling context mid-turn. Keep it minimal.
            "text_segment_start" if self.verbose_tools => {
                self.write_stderr_line("");
            }
            "tool_start" if self.verbose_tools => {
                let name = payload
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("<tool>");
                self.write_stderr_line(&format!("\n  · 调用工具 → {name}"));
            }
            "tool_end" if self.verbose_tools => {
                let name = payload
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("<tool>");
                let is_error = payload
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let marker = if is_error { "✗" } else { "✓" };
                self.write_stderr_line(&format!("  · {marker} {name}"));
            }
            "error" => {
                let msg = payload
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                self.write_stderr_line(&format!("\n[错误] {msg}"));
            }
            _ => {}
        }
    }

    fn emit_broadcast(&self, _event: &str, _payload: Value) {}
}

impl PoolEventSink for InteractiveEventSink {
    fn emit_pool(&self, _event: &PoolEvent) {
        // Interactive mode is single-agent pisci only — pool events are
        // irrelevant and would be noisy if surfaced.
    }
}

/// Top-level REPL state that survives across turns.
struct ReplState {
    session_id: Option<String>,
    workspace: Option<String>,
    verbose_tools: bool,
    app_data_dir: PathBuf,
}

impl ReplState {
    fn new() -> Self {
        Self {
            session_id: None,
            workspace: None,
            verbose_tools: std::env::var("OPENPISCI_VERBOSE_TOOLS")
                .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
                .unwrap_or(false),
            app_data_dir: resolve_app_data_dir(None),
        }
    }
}

/// Entry point: run the interactive REPL until the user exits (EOF,
/// `:quit`, `:exit`) or a fatal setup error occurs.
///
/// Returns `Ok(())` on normal exit; `Err` only when initial setup
/// (missing API key, DB open failure, etc.) cannot be recovered from.
pub fn run_interactive() -> Result<(), String> {
    let mut state = ReplState::new();
    print_banner(&state);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to start tokio runtime: {e}"))?;

    let stdin = io::stdin();
    let mut stdin_lock = stdin.lock();
    let mut line_buf = String::new();
    loop {
        print_prompt();
        line_buf.clear();
        let read = stdin_lock
            .read_line(&mut line_buf)
            .map_err(|e| format!("stdin read failed: {e}"))?;
        if read == 0 {
            // EOF (Ctrl-Z on Windows, Ctrl-D on *nix).
            eprintln!();
            eprintln!("再见。");
            return Ok(());
        }
        let line = line_buf.trim_end_matches(['\r', '\n']).to_string();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with(':') {
            match handle_command(trimmed, &mut state) {
                CommandOutcome::Continue => continue,
                CommandOutcome::Exit => return Ok(()),
            }
        }

        // Normal prompt → run one pisci turn.
        match runtime.block_on(run_one_turn(&mut state, &line)) {
            Ok(()) => {}
            Err(e) => eprintln!("\n[turn failed] {e}"),
        }
        // Ensure the next prompt starts on a fresh line even if the LLM
        // forgot to emit a trailing newline.
        println!();
    }
}

/// Build headless deps + drive a single turn, streaming to the user.
async fn run_one_turn(state: &mut ReplState, prompt: &str) -> Result<(), String> {
    let host = CliHost::new(state.app_data_dir.clone());
    let interactive_sink = std::sync::Arc::new(InteractiveEventSink::new(state.verbose_tools));

    let (db, settings) = headless::open_kernel_state(&state.app_data_dir)
        .map_err(|e| format!("Failed to open kernel state: {e}"))?;

    let mut handle: ToolRegistryHandle = new_tool_registry_handle();
    let neutral_cfg = NeutralToolsConfig {
        db: Some(db.clone()),
        settings: Some(settings.clone()),
        builtin_tool_enabled: None,
        user_tools_dir: Some(state.app_data_dir.join("user_tools")),
        event_sink: Some(interactive_sink.clone()),
        plan_store: Some(new_plan_store()),
        pool_event_sink: Some(interactive_sink.clone()),
        subagent_runtime: None,
        coordinator_config: Default::default(),
    };
    pisci_kernel::tools::register_neutral_tools(&mut handle, &neutral_cfg);
    host.host_tools().register(&mut handle);
    let registry = handle
        .into_registry()
        .map_err(|_| "internal: registry handle type mismatch".to_string())?;

    let request = HeadlessCliRequest {
        mode: HeadlessCliMode::Pisci,
        prompt: prompt.to_string(),
        workspace: state.workspace.clone(),
        session_id: state.session_id.clone(),
        session_title: Some("interactive".to_string()),
        channel: Some("cli".to_string()),
        extra_system_context: Some(
            "You are attached to a human at a terminal. Keep responses \
             concise and directly actionable; prefer showing the final \
             answer over narrating your tool usage."
                .to_string(),
        ),
        ..Default::default()
    };

    let deps = HeadlessDeps::new(db, settings, registry, interactive_sink.clone());
    let response = headless::run_pisci_turn(request, deps)
        .await
        .map_err(|e| format!("{e}"))?;

    // Remember the session id so subsequent prompts see a continuous
    // conversation. `response.session_id` is always populated by the
    // kernel (either echoed back or freshly created).
    state.session_id = Some(response.session_id);
    Ok(())
}

enum CommandOutcome {
    Continue,
    Exit,
}

fn handle_command(line: &str, state: &mut ReplState) -> CommandOutcome {
    let mut parts = line.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();
    match cmd {
        ":help" | ":h" | ":?" => {
            print_help();
        }
        ":exit" | ":quit" | ":q" => {
            eprintln!("再见。");
            return CommandOutcome::Exit;
        }
        ":new" => {
            state.session_id = None;
            eprintln!("已开启新的会话。");
        }
        ":session" => match rest {
            "" => match &state.session_id {
                Some(id) => eprintln!("当前 session_id = {id}"),
                None => eprintln!("尚未创建 session（下次发送消息时自动创建）。"),
            },
            id => {
                state.session_id = Some(id.to_string());
                eprintln!("已切换到 session_id = {id}");
            }
        },
        ":workspace" | ":cwd" => match rest {
            "" => match &state.workspace {
                Some(w) => eprintln!("workspace = {w}"),
                None => eprintln!("workspace 使用 settings.workspace_root 默认值。"),
            },
            path => {
                state.workspace = Some(path.to_string());
                eprintln!("workspace 设置为 {path}");
            }
        },
        ":verbose" => match rest {
            "" => eprintln!(
                "tool-verbose = {} （:verbose on / :verbose off 切换）",
                state.verbose_tools
            ),
            "on" | "1" | "true" => {
                state.verbose_tools = true;
                eprintln!("已开启工具调用显示。");
            }
            "off" | "0" | "false" => {
                state.verbose_tools = false;
                eprintln!("已关闭工具调用显示。");
            }
            other => eprintln!("无效参数 `{other}`（使用 on / off）。"),
        },
        ":status" => {
            eprintln!("app_data_dir = {}", state.app_data_dir.display());
            eprintln!(
                "session_id  = {}",
                state.session_id.as_deref().unwrap_or("<尚未创建>")
            );
            eprintln!(
                "workspace   = {}",
                state.workspace.as_deref().unwrap_or("<默认 settings>")
            );
            eprintln!("tool-verbose= {}", state.verbose_tools);
        }
        ":clear" => {
            // ANSI clear — harmless on terminals that don't support it
            // (they just render the escape literally once).
            print!("\x1B[2J\x1B[H");
            let _ = io::stdout().flush();
        }
        other => {
            eprintln!("未知命令 `{other}`；输入 :help 查看可用命令。");
        }
    }
    CommandOutcome::Continue
}

fn print_banner(state: &ReplState) {
    println!("OpenPisci headless CLI · {}", pisci_kernel::KERNEL_VERSION);
    println!("交互模式。输入内容即可对话；:help 查看命令，:quit 退出。");
    println!("数据目录：{}", state.app_data_dir.display());
    println!();
}

fn print_prompt() {
    let mut out = io::stdout();
    let _ = out.write_all(b"\xe4\xbd\xa0\xe2\x96\xb8 "); // "你▸ "
    let _ = out.flush();
}

fn print_help() {
    eprintln!(
        "可用命令：\n\
         :help                 显示此帮助\n\
         :status               打印当前状态\n\
         :new                  开启新会话（丢弃历史上下文）\n\
         :session [id]         查看 / 切换 session_id\n\
         :workspace [dir]      查看 / 设置工作目录（影响工具的文件操作范围）\n\
         :verbose [on|off]     是否在每次工具调用时显示名称\n\
         :clear                清屏\n\
         :quit / :exit         退出\n\n\
         直接输入文本即发送给代理；按 Ctrl-Z (Windows) / Ctrl-D (*nix) 亦可退出。"
    );
}
