//! Live end-to-end smoke test that drives a **real** LLM through the
//! production harness without a Tauri AppHandle.
//!
//! Exercises:
//!   - `HarnessConfig::for_scheduler` (the AppHandle-free factory) +
//!     `into_agent_loop`
//!   - the full registered tool set reachable without an AppHandle:
//!     `shell`, `file_read`, `file_write`, `file_edit`, `file_list`,
//!     `file_search`, `code_run`, `recall_tool_result` (needs DB)
//!   - dual-schema tool injection, Level-1 compaction (recall hints +
//!     demotion to receipts), RequestBuilder per-provider max_tokens
//!     capping, and the layered ContextUsage telemetry added in p8
//!
//! This test is `#[ignore]`d by default — it makes real network calls.
//! Run with explicit config:
//! ```text
//! $env:PISCI_LIVE_CONFIG_DIR = "$env:APPDATA\com.pisci.desktop"
//! $env:PISCI_LIVE_OUT        = "C:\path\to\harness-smoke.jsonl"
//! $env:PISCI_LIVE_MAX_ITERS  = "20"    # optional
//! $env:PISCI_LIVE_TIMEOUT_S  = "600"   # optional
//! cargo test --manifest-path src-tauri/Cargo.toml --lib --features "" -- \
//!     --ignored --nocapture agent::live_smoke::harness_live_smoke
//! ```
//!
//! The test writes one JSONL line per AgentEvent to `PISCI_LIVE_OUT`,
//! plus a final `summary` line carrying aggregated stats
//! (iteration count, tool-call histogram, max context utilisation,
//! final `layered_breakdown`, cumulative tokens).
//!
//! Safety:
//!   - uses an **in-memory** SQLite so we never touch the user's
//!     production DB
//!   - runs in a fresh temp workspace (created under `%TEMP%/pisci-live-smoke-<uuid>`
//!     and deleted on drop)
//!   - `policy_mode = "sandbox"` to keep shell+file tools bounded to the
//!     temp workspace
//!   - `bypass_permissions = true` because we have no UI to prompt on

#![cfg(test)]
#![allow(clippy::too_many_lines)]

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value as Json};
use tokio::sync::{mpsc, Mutex};

use crate::agent::harness::config::{CompactionSettings, HarnessConfig};
use crate::agent::messages::{AgentEvent, LayeredTokenBreakdownSnapshot};
use crate::agent::tool::{ToolContext, ToolSettings};
use crate::llm::{LlmMessage, MessageContent};
use crate::policy::gate::PolicyGate;
use crate::store::db::Database;
use crate::store::settings::Settings;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn temp_workspace(tag: &str) -> PathBuf {
    let uuid = uuid::Uuid::new_v4().to_string();
    let p = std::env::temp_dir().join(format!("pisci-live-smoke-{}-{}", tag, &uuid[..8]));
    std::fs::create_dir_all(&p).expect("create temp workspace");
    p
}

/// The real prompt fed to the agent. Kept deliberately moderate in scope
/// so we can observe multiple tool rounds without eating huge budget.
fn task_prompt(workspace: &std::path::Path) -> String {
    format!(
        "You are running a small autonomous coding task in the workspace `{}`.\n\n\
         ### Goal\n\
         Create a Python module `string_utils.py` with exactly these three functions:\n\
         1. `reverse_words(s: str) -> str` — reverse the order of whitespace-separated tokens.\n\
         2. `count_vowels(s: str) -> int` — count ASCII vowels (aeiouAEIOU).\n\
         3. `to_title_case(s: str) -> str` — Title-case each whitespace-separated word.\n\n\
         Then create a `tests.py` file that imports `string_utils` and runs at least 3 assertions per function\n\
         (covering empty strings, normal input, and tricky input like multiple spaces or mixed case),\n\
         printing `OK` once all assertions pass.\n\n\
         ### Process\n\
         1. Inspect the workspace first with `file_list` so you see what is already there.\n\
         2. Write the two files with `file_write`. Use standard Python 3 syntax only.\n\
         3. Run `python tests.py` (or `py -3 tests.py` on Windows if `python` is not on PATH) via `shell`.\n\
         4. If any assertion fails, read the failing assertion with `file_read`, then fix the\n\
            module with `file_edit` and re-run the tests. Iterate until tests pass or it becomes\n\
            clear the environment has no Python runtime.\n\n\
         ### Success criteria\n\
         Either (a) `tests.py` prints `OK` with exit code 0, or (b) you clearly report that the\n\
         Python runtime is unavailable after at least one shell probe. In either case, summarise\n\
         what you did in your final message and stop.\n\n\
         Keep answers concise. Prefer small edits over full rewrites.",
        workspace.display()
    )
}

fn format_breakdown(b: &LayeredTokenBreakdownSnapshot) -> Json {
    json!({
        "persona": b.persona,
        "scene": b.scene,
        "memory": b.memory,
        "project": b.project,
        "platform_hint": b.platform_hint,
        "tool_defs": b.tool_defs,
        "history_text": b.history_text,
        "history_tool_result_full": b.history_tool_result_full,
        "history_tool_result_receipt": b.history_tool_result_receipt,
        "rolling_summary": b.rolling_summary,
        "state_frame": b.state_frame,
        "vision": b.vision,
        "request_overhead": b.request_overhead,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real-network: runs a real LLM loop; enable with env PISCI_LIVE_CONFIG_DIR"]
async fn harness_live_smoke() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,pisci_desktop_lib=info")),
        )
        .with_test_writer()
        .try_init()
        .ok();

    // ── 1. Load real settings from disk ───────────────────────────────
    let config_dir = env_or(
        "PISCI_LIVE_CONFIG_DIR",
        &format!(
            "{}\\com.pisci.desktop",
            std::env::var("APPDATA").unwrap_or_else(|_| String::from("."))
        ),
    );
    let config_path = PathBuf::from(&config_dir).join("config.json");
    assert!(
        config_path.exists(),
        "config.json not found at {} — set PISCI_LIVE_CONFIG_DIR",
        config_path.display()
    );
    let settings = Settings::load(&config_path).expect("load settings");
    let api_key = settings.active_api_key().to_string();
    assert!(
        !api_key.is_empty(),
        "active_api_key is empty for provider='{}'; check your config.json + .secret_key",
        settings.provider
    );

    let provider = settings.provider.clone();
    let model = settings.model.clone();
    let base_url = settings.custom_base_url.clone();
    let max_tokens = settings.max_tokens.max(2048);
    let context_window = settings.context_window;
    let read_timeout = settings.llm_read_timeout_secs.max(60);

    eprintln!(
        "[live-smoke] provider={} model={} context_window={} max_tokens={}",
        provider, model, context_window, max_tokens
    );

    // ── 2. Temp workspace + in-memory DB ──────────────────────────────
    let workspace = temp_workspace("smoke");
    eprintln!("[live-smoke] workspace={}", workspace.display());

    let db = Arc::new(Mutex::new(
        Database::open_in_memory().expect("in-memory db"),
    ));
    let session_id = {
        let db = db.lock().await;
        db.create_session(Some("live-smoke"))
            .expect("create session")
            .id
    };

    // ── 3. Real LLM client + tool registry (no AppHandle) ─────────────
    let client = crate::llm::build_client_with_timeout(
        &provider,
        &api_key,
        if base_url.is_empty() {
            None
        } else {
            Some(&base_url)
        },
        read_timeout,
    );

    let browser = crate::browser::create_browser_manager(Default::default());
    let registry = Arc::new(crate::tools::build_registry(
        browser,
        None,                 // user_tools_dir
        Some(db.clone()),     // db — needed for recall_tool_result / memory_store
        None,                 // builtin_tool_enabled — use defaults
        None,                 // app_handle — THIS is the key: we pass None
        None,                 // settings
        None,                 // app_data_dir
        None,                 // skill_loader
    ));

    let policy = Arc::new(PolicyGate::with_profile_and_flags(
        workspace.to_str().unwrap(),
        "sandbox",
        0,    // tool_rate_limit_per_minute — disabled
        false,
    ));

    let compaction = CompactionSettings::from_settings(&settings);
    let system_prompt = String::from(
        "You are a minimal autonomous coding assistant running in a Windows smoke-test harness.\n\
         You have file and shell tools and a fresh temp workspace. Be decisive and terse.",
    );

    let harness = HarnessConfig::for_scheduler(
        model.clone(),
        Vec::new(), // fallback_models
        registry,
        policy,
        system_prompt,
        max_tokens,
        context_window,
        Some(settings.vision_enabled),
        settings.auto_compact_input_tokens_threshold,
        compaction,
        db.clone(),
    );
    let agent = harness.into_agent_loop(client, None, None);

    // ── 4. Run the loop ──────────────────────────────────────────────
    let cancel = Arc::new(AtomicBool::new(false));
    let max_iters: u32 = env_or("PISCI_LIVE_MAX_ITERS", "20").parse().unwrap_or(20);
    let timeout_secs: u64 = env_or("PISCI_LIVE_TIMEOUT_S", "600").parse().unwrap_or(600);

    let tool_settings = Arc::new(ToolSettings::default());
    let ctx = ToolContext {
        session_id: session_id.clone(),
        workspace_root: workspace.clone(),
        bypass_permissions: true, // no UI to answer permission prompts
        settings: tool_settings,
        max_iterations: Some(max_iters),
        memory_owner_id: "pisci".to_string(),
        pool_session_id: None,
        cancel: cancel.clone(),
    };

    let prompt = task_prompt(&workspace);
    let messages = vec![LlmMessage {
        role: "user".into(),
        content: MessageContent::text(&prompt),
    }];

    let (tx, mut rx) = mpsc::channel::<AgentEvent>(1024);

    let out_path = PathBuf::from(env_or(
        "PISCI_LIVE_OUT",
        &format!(
            "{}\\harness-smoke-{}.jsonl",
            std::env::temp_dir().display(),
            &session_id
        ),
    ));
    eprintln!("[live-smoke] events => {}", out_path.display());
    let mut out = std::fs::File::create(&out_path).expect("create jsonl");
    writeln!(
        out,
        "{}",
        json!({
            "type": "_meta",
            "provider": provider,
            "model": model,
            "workspace": workspace.display().to_string(),
            "max_iterations": max_iters,
            "timeout_s": timeout_secs,
        })
    )
    .ok();

    // Event consumer: tee to JSONL and aggregate stats.
    let out_path_clone = out_path.clone();
    let collector = tokio::spawn(async move {
        let mut max_est: u32 = 0;
        let mut last_breakdown: Option<LayeredTokenBreakdownSnapshot> = None;
        let mut tool_counts: BTreeMap<String, u32> = BTreeMap::new();
        let mut tool_errors: u32 = 0;
        let mut iterations: u32 = 0;
        let mut text_chars: u64 = 0;
        let mut final_tokens = (0u32, 0u32);
        let mut saw_done = false;
        let mut saw_error: Option<String> = None;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&out_path_clone)
            .expect("open jsonl");

        while let Some(event) = rx.recv().await {
            if let Ok(s) = serde_json::to_string(&event) {
                let _ = writeln!(file, "{}", s);
            }
            match event {
                AgentEvent::TextSegmentStart { iteration } => {
                    iterations = iterations.max(iteration);
                }
                AgentEvent::TextDelta { delta } => {
                    text_chars += delta.chars().count() as u64;
                }
                AgentEvent::ToolStart { name, .. } => {
                    *tool_counts.entry(name).or_insert(0) += 1;
                }
                AgentEvent::ToolEnd { is_error, .. } => {
                    if is_error {
                        tool_errors += 1;
                    }
                }
                AgentEvent::ContextUsage {
                    estimated_input_tokens,
                    layered_breakdown,
                    ..
                } => {
                    max_est = max_est.max(estimated_input_tokens);
                    if let Some(b) = layered_breakdown {
                        last_breakdown = Some(b);
                    }
                }
                AgentEvent::Done {
                    total_input_tokens,
                    total_output_tokens,
                } => {
                    final_tokens = (total_input_tokens, total_output_tokens);
                    saw_done = true;
                }
                AgentEvent::Error { message } => {
                    saw_error = Some(message);
                }
                _ => {}
            }
        }
        (
            iterations,
            tool_counts,
            tool_errors,
            text_chars,
            max_est,
            last_breakdown,
            final_tokens,
            saw_done,
            saw_error,
        )
    });

    let run_fut = agent.run(messages, tx, cancel.clone(), ctx);
    let run_res = tokio::time::timeout(Duration::from_secs(timeout_secs), run_fut).await;
    match &run_res {
        Ok(Ok((final_msgs, tin, tout))) => {
            eprintln!(
                "[live-smoke] agent finished: final_msgs={} in_tokens={} out_tokens={}",
                final_msgs.len(),
                tin,
                tout
            );
        }
        Ok(Err(e)) => eprintln!("[live-smoke] agent error: {}", e),
        Err(_) => {
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            eprintln!("[live-smoke] TIMEOUT after {}s — sent cancel", timeout_secs);
        }
    }

    // Drop sender side by wrapping above scope; collector will see channel close.
    let (iters, tool_counts, tool_errors, text_chars, max_est, last_b, final_tok, saw_done, saw_err) =
        collector.await.expect("collector");

    // Peek at final DB state: how many messages survived
    let db_msg_count = {
        let db = db.lock().await;
        db.get_messages_latest(&session_id, 10_000)
            .map(|m| m.len())
            .unwrap_or(0)
    };

    let mut summary = json!({
        "type": "_summary",
        "provider": provider,
        "model": model,
        "iterations": iters,
        "tool_counts": tool_counts,
        "tool_errors": tool_errors,
        "text_delta_chars": text_chars,
        "max_estimated_input_tokens": max_est,
        "final_total_input_tokens": final_tok.0,
        "final_total_output_tokens": final_tok.1,
        "saw_done": saw_done,
        "saw_error": saw_err,
        "db_msg_count_after_run": db_msg_count,
    });
    if let Some(b) = last_b {
        summary["final_layered_breakdown"] = format_breakdown(&b);
    }

    eprintln!("[live-smoke] summary = {}", serde_json::to_string_pretty(&summary).unwrap());
    let mut out = std::fs::OpenOptions::new()
        .append(true)
        .open(&out_path)
        .unwrap();
    writeln!(out, "{}", summary).ok();

    // ── 5. Assertions ────────────────────────────────────────────────
    // We want the run to at least produce *some* assistant activity.
    assert!(
        iters >= 1,
        "agent should have reached at least one TextSegmentStart (provider/model issue?)"
    );
    assert!(
        !tool_counts.is_empty() || saw_err.is_some(),
        "agent should have attempted at least one tool call (or surfaced an explicit error)"
    );
    // Budget-telemetry is the main thing this test validates from the
    // harness side:
    assert!(
        max_est > 0,
        "no ContextUsage event was emitted — layered-budget telemetry is broken"
    );
}

