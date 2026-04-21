//! Host-agnostic single-turn agent runner for headless CLI invocations.
//!
//! [`run_pisci_turn`] owns the kernel-side orchestration that was previously
//! tangled up in `pisci-desktop`'s `commands/chat.rs` + `desktop_app.rs`:
//!
//!   1. Resolves provider / model / API key from [`Settings`].
//!   2. Opens / creates a DB session, appends the user prompt.
//!   3. Builds an LLM client, policy gate, harness config, and agent loop.
//!   4. Drives the loop through an `mpsc` channel, collecting streamed text
//!      and forwarding every [`AgentEvent`] to the caller-supplied
//!      [`EventSink`] (CLI host prints NDJSON; desktop host can re-emit the
//!      events to Tauri).
//!   5. Persists new messages to the DB and emits a final `Done` / `Error`
//!      event.
//!
//! Scope: single-agent pisci mode. Pool orchestration and Koi delegation
//! stay on the desktop crate because they depend on desktop-specific
//! coordination (Tauri windows, pool scheduler, per-koi notification rx).
//! Hosts that want to invoke `openpisci-headless run` with `mode: pool`
//! should fall back to the existing desktop CLI.
//!
//! This module is the shared core behind both:
//!   - `openpisci-headless run` (in `pisci-cli`) — full kernel path,
//!     no Tauri, no AppState.
//!   - future desktop refactor that routes pisci-mode headless runs
//!     through this helper instead of booting a Tauri app in headless.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::sync::{mpsc, Mutex};

use crate::agent::harness::config::{CompactionSettings, HarnessConfig};
use crate::agent::messages::AgentEvent;
use crate::agent::tool::{ToolContext, ToolRegistry, ToolSettings};
use crate::llm::{self, ContentBlock, LlmMessage, MessageContent};
use crate::policy::gate::PolicyGate;
use crate::store::db::Database;
use crate::store::settings::Settings;

use pisci_core::host::{
    EventSink, HeadlessCliMode, HeadlessCliRequest, HeadlessCliResponse, ToolRegistryHandle,
};

/// Shared handles for the kernel-level persistence layer (SQLite DB +
/// `config.json` settings).
pub type KernelState = (Arc<Mutex<Database>>, Arc<Mutex<Settings>>);

/// Owned dependencies for [`run_pisci_turn`]. Hosts typically build this from
/// their own state (desktop: `AppState`; CLI: `CliHost`).
pub struct HeadlessDeps {
    pub db: Arc<Mutex<Database>>,
    pub settings: Arc<Mutex<Settings>>,
    pub tools: ToolRegistry,
    pub event_sink: Arc<dyn EventSink>,
    /// Absolute timeout for the whole turn. Defaults to 10 minutes when
    /// [`HeadlessCliRequest::task_timeout_secs`] is `None`.
    pub default_timeout: Duration,
}

impl HeadlessDeps {
    pub fn new(
        db: Arc<Mutex<Database>>,
        settings: Arc<Mutex<Settings>>,
        tools: ToolRegistry,
        event_sink: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            db,
            settings,
            tools,
            event_sink,
            default_timeout: Duration::from_secs(600),
        }
    }
}

/// Open (or create) the kernel-level state needed to run a headless turn.
///
/// Creates `app_data_dir` if missing, opens `pisci.db` and loads
/// `config.json`. Returned handles are ready to feed into [`HeadlessDeps`].
pub fn open_kernel_state(app_data_dir: &Path) -> Result<KernelState> {
    std::fs::create_dir_all(app_data_dir)
        .with_context(|| format!("failed to create app_data_dir {}", app_data_dir.display()))?;
    let db_path = app_data_dir.join("pisci.db");
    let db = Database::open(&db_path)
        .with_context(|| format!("failed to open DB at {}", db_path.display()))?;
    let config_path = app_data_dir.join("config.json");
    let settings = Settings::load(&config_path)
        .with_context(|| format!("failed to load {}", config_path.display()))?;
    Ok((Arc::new(Mutex::new(db)), Arc::new(Mutex::new(settings))))
}

/// Populate `handle` with all kernel-neutral tools according to the supplied
/// config. This is a convenience wrapper around
/// [`crate::tools::register_neutral_tools`] so host binaries that only need
/// neutral tools do not have to import the tool module path.
pub fn register_default_cli_tools(
    handle: &mut ToolRegistryHandle,
    db: Arc<Mutex<Database>>,
    settings: Arc<Mutex<Settings>>,
) {
    let cfg = crate::tools::NeutralToolsConfig {
        db: Some(db),
        settings: Some(settings),
        builtin_tool_enabled: None,
        user_tools_dir: None,
        event_sink: None,
        plan_store: None,
        pool_event_sink: None,
        subagent_runtime: None,
        coordinator_config: Default::default(),
    };
    crate::tools::register_neutral_tools(handle, &cfg);
}

/// Run a single pisci-mode turn against `request` using `deps`. The
/// function is synchronous from the caller's perspective: it returns only
/// after the agent loop has emitted `Done` (or timed out / errored).
///
/// On success, `response_text` is the concatenation of all streamed
/// assistant text deltas; on error the function propagates the cause.
pub async fn run_pisci_turn(
    request: HeadlessCliRequest,
    deps: HeadlessDeps,
) -> Result<HeadlessCliResponse> {
    if !matches!(request.mode, HeadlessCliMode::Pisci) {
        return Err(anyhow!(
            "run_pisci_turn only supports mode=pisci (got {:?}). Use the \
             desktop openpisci binary for pool mode.",
            request.mode
        ));
    }

    let HeadlessDeps {
        db,
        settings,
        tools,
        event_sink,
        default_timeout,
    } = deps;

    // ── Settings snapshot ──────────────────────────────────────────────
    let (
        provider,
        model,
        api_key,
        base_url,
        settings_workspace,
        max_tokens,
        context_window,
        read_timeout,
        policy_mode,
        tool_rate_limit_per_minute,
        allow_outside_workspace,
        vision_enabled,
        auto_compact_threshold,
        fallback_models,
        compaction,
        tool_settings,
    ) = {
        let s = settings.lock().await;
        (
            s.provider.clone(),
            s.model.clone(),
            s.active_api_key().to_string(),
            s.custom_base_url.clone(),
            s.workspace_root.clone(),
            s.max_tokens.max(1024),
            s.context_window,
            s.llm_read_timeout_secs.max(30),
            s.policy_mode.clone(),
            s.tool_rate_limit_per_minute,
            s.allow_outside_workspace,
            s.vision_enabled,
            s.auto_compact_input_tokens_threshold,
            s.fallback_models.clone(),
            CompactionSettings::from_settings(&s),
            Arc::new(ToolSettings::from_settings(&s)),
        )
    };

    if api_key.is_empty() {
        return Err(anyhow!(
            "no API key configured for provider '{}'. Populate config.json \
             or set the provider's environment variable before running \
             openpisci-headless.",
            provider
        ));
    }

    // ── Session resolution ─────────────────────────────────────────────
    let workspace_root = request
        .workspace
        .clone()
        .filter(|w| !w.trim().is_empty())
        .unwrap_or(settings_workspace);
    let session_id = match request.session_id.clone().filter(|s| !s.is_empty()) {
        Some(id) => id,
        None => {
            let title = request.session_title.as_deref();
            let db = db.lock().await;
            db.create_session_with_source(title, "cli")
                .context("failed to create session")?
                .id
        }
    };

    // Always persist the incoming user message so future turns see a
    // consistent DB. This mirrors chat_send's behaviour.
    {
        let db = db.lock().await;
        db.append_message(&session_id, "user", &request.prompt)
            .context("failed to append user message")?;
        let _ = db.update_session_status(&session_id, "running");
    }

    // Load prior messages for context continuity (capped by a reasonable
    // upper bound — the harness's own compaction layer trims further).
    let mut llm_messages: Vec<LlmMessage> = {
        let db = db.lock().await;
        db.get_messages_latest(&session_id, 500)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|m| {
                let role = m.role;
                let content = m.content;
                if role == "user" || role == "assistant" {
                    Some(LlmMessage {
                        role,
                        content: MessageContent::Blocks(vec![ContentBlock::Text { text: content }]),
                    })
                } else {
                    None
                }
            })
            .collect()
    };
    if llm_messages.is_empty() {
        // Fresh session — at minimum feed the current prompt.
        llm_messages.push(LlmMessage {
            role: "user".into(),
            content: MessageContent::text(&request.prompt),
        });
    }

    // ── LLM client + harness ───────────────────────────────────────────
    let client = llm::build_client_with_timeout(
        &provider,
        &api_key,
        if base_url.is_empty() {
            None
        } else {
            Some(&base_url)
        },
        read_timeout,
    );

    let system_prompt = default_headless_system_prompt(
        &workspace_root,
        allow_outside_workspace,
        request.extra_system_context.as_deref(),
    );

    let policy = Arc::new(PolicyGate::with_profile_and_flags(
        &workspace_root,
        &policy_mode,
        tool_rate_limit_per_minute,
        allow_outside_workspace,
    ));

    let harness = HarnessConfig::for_scheduler(
        model.clone(),
        fallback_models,
        Arc::new(tools),
        policy,
        system_prompt,
        max_tokens,
        context_window,
        Some(vision_enabled),
        auto_compact_threshold,
        compaction,
        db.clone(),
    );
    let agent = harness.into_agent_loop(client, None, None);

    // ── Tool context ───────────────────────────────────────────────────
    let cancel = Arc::new(AtomicBool::new(false));
    let workspace_buf = PathBuf::from(&workspace_root);
    let max_iterations = {
        let s = settings.lock().await;
        s.max_iterations
    };
    let ctx = ToolContext {
        session_id: session_id.clone(),
        workspace_root: workspace_buf,
        bypass_permissions: true, // no UI to prompt
        settings: tool_settings,
        max_iterations: Some(max_iterations),
        memory_owner_id: "pisci".to_string(),
        pool_session_id: None,
        cancel: cancel.clone(),
    };

    // ── Event bridge ───────────────────────────────────────────────────
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(1024);
    let collector_sink = event_sink.clone();
    let collector_session = session_id.clone();
    let collector = tokio::spawn(async move {
        let mut text = String::new();
        let mut errored: Option<String> = None;
        while let Some(event) = rx.recv().await {
            // Stream to host event sink (CliHost prints NDJSON).
            if let Ok(payload) = serde_json::to_value(&event) {
                collector_sink.emit_session(&collector_session, "agent_event", payload);
            }
            match event {
                AgentEvent::TextDelta { delta } => {
                    text.push_str(&delta);
                }
                AgentEvent::Error { message } => {
                    errored = Some(message);
                }
                AgentEvent::Done { .. } => break,
                _ => {}
            }
        }
        (text, errored)
    });

    // ── Drive the loop ─────────────────────────────────────────────────
    let timeout = match request.task_timeout_secs {
        Some(s) if s > 0 => Duration::from_secs(u64::from(s)),
        _ => default_timeout,
    };
    let run_fut = agent.run(llm_messages, tx, cancel.clone(), ctx);
    let run_res = tokio::time::timeout(timeout, run_fut).await;

    let (ok, new_messages, error_msg): (bool, Vec<LlmMessage>, Option<String>) = match run_res {
        Ok(Ok((msgs, _total_in, _total_out))) => (true, msgs, None),
        Ok(Err(e)) => (false, Vec::new(), Some(format!("agent error: {e}"))),
        Err(_) => {
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            (
                false,
                Vec::new(),
                Some(format!("timed out after {}s", timeout.as_secs())),
            )
        }
    };

    let (streamed_text, stream_error) = collector.await.unwrap_or_default();

    // Persist new messages to DB so subsequent turns see them.
    if ok && !new_messages.is_empty() {
        let db = db.lock().await;
        for msg in &new_messages {
            let role = &msg.role;
            let text = msg.content.as_text();
            let _ = db.append_message(&session_id, role, &text);
        }
    }

    // Emit a final Done / Error to the sink so CLI consumers see a tidy
    // end-of-stream marker even if the mpsc channel closed early.
    if let Some(err) = error_msg.as_deref().or(stream_error.as_deref()) {
        event_sink.emit_session(
            &session_id,
            "agent_final",
            serde_json::json!({"ok": false, "error": err}),
        );
    } else {
        event_sink.emit_session(&session_id, "agent_final", serde_json::json!({"ok": true}));
    }
    let _ = std::io::stdout().flush();

    let response_text = if !streamed_text.is_empty() {
        streamed_text
    } else {
        // Fallback: the last assistant-authored text block.
        new_messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant")
            .map(|m| m.content.as_text())
            .unwrap_or_default()
    };

    if let Some(err) = error_msg {
        return Err(anyhow!(err));
    }

    Ok(HeadlessCliResponse {
        ok,
        mode: HeadlessCliMode::Pisci.as_str().to_string(),
        session_id,
        pool_id: None,
        response_text,
        disabled_tools: Vec::new(),
        pool_wait: None,
    })
}

fn default_headless_system_prompt(
    workspace_root: &str,
    allow_outside: bool,
    extra_context: Option<&str>,
) -> String {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let workspace_line = if workspace_root.trim().is_empty() {
        String::new()
    } else {
        let note = if allow_outside {
            " (access outside this directory is also permitted when needed)"
        } else {
            " (file operations are restricted to this directory)"
        };
        format!("\nWorkspace: `{workspace_root}`{note}")
    };
    let extras = extra_context.map(str::trim).filter(|s| !s.is_empty());
    let mut body = format!(
        "You are Pisci running in headless CLI mode.\n\
         Today's date: {today}{workspace_line}\n\n\
         ## Tool usage\n\
         - Prefer `file_list` / `file_read` / `file_search` to explore the workspace.\n\
         - Use `file_write` and `file_edit` for changes; `file_diff` to preview edits.\n\
         - Use `shell` for commands and `code_run` for build / test flows.\n\
         - Keep replies concise. Stop as soon as the requested task is done.\n"
    );
    if let Some(extra) = extras {
        body.push_str("\n## Extra context from caller\n");
        body.push_str(extra);
        body.push('\n');
    }
    body
}
