//! End-to-end integration tests for the `openpisci-headless` binary.
//!
//! These tests launch the compiled binary via Cargo's
//! `CARGO_BIN_EXE_openpisci-headless` hook and exercise the public CLI
//! surface that downstream hosts and the Linux CI pipeline rely on:
//!
//! * `version` and `--help` smoke tests.
//! * `capabilities` schema (JSON structure + required fields).
//! * `run` argument validation (missing prompt, unknown flags).
//! * `rpc` subcommand protocol smoke (shutdown roundtrip, invalid frame).
//!
//! The tests intentionally do NOT require a live LLM API key — they target
//! the kernel/CLI wiring, not the agent loop. A separate opt-in smoke test
//! (gated on `OPENPISCI_TEST_API_KEY`) covers the full end-to-end turn.

use std::path::PathBuf;
use std::process::Command;

fn bin_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_openpisci-headless"))
}

fn run(args: &[&str]) -> (std::process::Output, String, String) {
    let output = Command::new(bin_path())
        .args(args)
        .output()
        .expect("failed to execute openpisci-headless binary");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (output, stdout, stderr)
}

#[test]
fn version_prints_kernel_version() {
    let (out, stdout, _stderr) = run(&["version"]);
    assert!(out.status.success(), "version should succeed");
    assert!(
        stdout.contains("openpisci-headless"),
        "stdout missing banner: {stdout}"
    );
}

#[test]
fn help_prints_usage_banner() {
    let (out, _stdout, stderr) = run(&["--help"]);
    assert!(out.status.success(), "help should exit 0");
    assert!(
        stderr.contains("Usage") && stderr.contains("run") && stderr.contains("capabilities"),
        "unexpected help banner: {stderr}"
    );
}

#[test]
fn capabilities_emits_expected_schema() {
    let (out, stdout, stderr) = run(&["capabilities"]);
    assert!(out.status.success(), "capabilities failed: stderr={stderr}");

    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("capabilities output must be valid JSON");

    assert_eq!(value["headless"], serde_json::Value::Bool(true));
    assert_eq!(value["host"], "cli");
    assert_eq!(value["mode"], "pisci");
    assert!(
        value["kernel_version"].is_string(),
        "kernel_version missing: {value}"
    );
    assert!(
        value["disabled_tools"].is_array(),
        "disabled_tools must be an array"
    );

    let disabled = value["disabled_tools"].as_array().unwrap();
    let names: Vec<&str> = disabled.iter().filter_map(|v| v["name"].as_str()).collect();
    for required in ["browser", "call_fish", "call_koi", "chat_ui"] {
        assert!(
            names.contains(&required),
            "expected disabled tool `{required}` in {names:?}"
        );
    }
    // Pool / plan tools live in pisci-kernel and must NOT be flagged as
    // disabled for the CLI host.
    for newly_enabled in ["plan_todo", "pool_org", "pool_chat"] {
        assert!(
            !names.contains(&newly_enabled),
            "{newly_enabled} should be enabled for openpisci-headless, still listed in {names:?}"
        );
    }
}

#[test]
fn capabilities_pool_mode_reports_mode_tag() {
    // Phase 2 onwards, pool is supported — the capabilities payload just
    // reports the selected mode without marking `<run>` unsupported.
    let (out, stdout, _stderr) = run(&["capabilities", "--mode", "pool"]);
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["mode"], "pool");
    let names: Vec<String> = value["disabled_tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["name"].as_str().map(str::to_string))
        .collect();
    assert!(
        !names.iter().any(|n| n == "<run>"),
        "<run> should no longer be flagged unsupported; got {names:?}"
    );
}

#[test]
fn run_requires_prompt() {
    let (out, _stdout, stderr) = run(&["run"]);
    assert!(!out.status.success(), "missing prompt should fail");
    assert!(
        stderr.contains("Missing prompt"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn run_rejects_unknown_flag() {
    let (out, _stdout, stderr) = run(&["run", "--prompt", "x", "--does-not-exist", "1"]);
    assert!(!out.status.success());
    assert!(
        stderr.contains("Unknown flag"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn unknown_subcommand_errors_out() {
    let (out, _stdout, stderr) = run(&["not-a-command"]);
    assert!(!out.status.success());
    assert!(stderr.contains("unknown subcommand"));
}

#[test]
fn rpc_shutdown_roundtrip() {
    // Drive the child by piping `shutdown` on stdin and asserting the
    // child replies with a `null` result, then exits cleanly. This
    // validates the JSON-RPC framing used by `SubprocessSubagentRuntime`
    // without needing a real Koi turn.
    use std::io::{Read, Write};
    use std::process::Stdio;

    let mut child = Command::new(bin_path())
        .arg("rpc")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rpc child");

    {
        let stdin = child.stdin.as_mut().expect("child stdin");
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"shutdown\"}\n")
            .expect("write shutdown");
        stdin.flush().ok();
    }

    let mut stdout = String::new();
    child
        .stdout
        .as_mut()
        .unwrap()
        .read_to_string(&mut stdout)
        .expect("read child stdout");
    let status = child.wait().expect("child exit");
    assert!(
        status.success(),
        "rpc child should exit 0 after shutdown, got {status:?}, stdout=`{stdout}`"
    );
    // The response line must be valid JSON with `"id":1` and a null
    // result. `trim` to tolerate trailing whitespace; `lines().next()`
    // because extra log lines may trail on stderr not stdout.
    let line = stdout.lines().next().unwrap_or("").trim();
    let value: serde_json::Value =
        serde_json::from_str(line).unwrap_or_else(|e| panic!("bad rpc response `{line}`: {e}"));
    assert_eq!(value["id"], 1);
    assert!(
        value.get("result").is_some(),
        "response missing result: {value}"
    );
}

#[test]
fn rpc_unknown_method_returns_jsonrpc_error() {
    // The rpc loop must keep running after an unknown-method error and
    // must not crash. We send one invalid method, one shutdown, and
    // expect two response lines.
    use std::io::{Read, Write};
    use std::process::Stdio;

    let mut child = Command::new(bin_path())
        .arg("rpc")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rpc child");
    {
        let stdin = child.stdin.as_mut().unwrap();
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":10,\"method\":\"no.such.method\"}\n")
            .unwrap();
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":11,\"method\":\"shutdown\"}\n")
            .unwrap();
        stdin.flush().ok();
    }
    let mut stdout = String::new();
    child
        .stdout
        .as_mut()
        .unwrap()
        .read_to_string(&mut stdout)
        .expect("read stdout");
    child.wait().expect("exit");

    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines.len() >= 2,
        "expected two response lines, got {lines:?}"
    );
    let first: serde_json::Value = serde_json::from_str(lines[0].trim()).unwrap();
    assert_eq!(first["id"], 10);
    assert_eq!(first["error"]["code"], -32601);
    let second: serde_json::Value = serde_json::from_str(lines[1].trim()).unwrap();
    assert_eq!(second["id"], 11);
    assert!(second.get("result").is_some());
}

#[test]
fn run_smoke_reports_missing_api_key_by_default() {
    // Don't accidentally pull in the user's real key — explicitly blank.
    let tmp = tempdir().expect("tmp dir");
    let cfg = tmp.path().join("data");
    std::fs::create_dir_all(&cfg).unwrap();

    let out = Command::new(bin_path())
        .env_remove("OPENPISCI_TEST_API_KEY")
        .env("OPENPISCI_CONFIG_DIR", &cfg)
        .args([
            "run",
            "--prompt",
            "quick ping",
            "--task-timeout-secs",
            "5",
            "--config-dir",
        ])
        .arg(&cfg)
        .output()
        .expect("spawn binary");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    // Without an API key, the binary must exit non-zero and print an
    // actionable error. It must not panic or emit stack traces.
    assert!(!out.status.success(), "expected failure without API key");
    assert!(
        stderr.contains("API key") || stderr.contains("api_key") || stderr.contains("provider"),
        "stderr should mention missing credentials: {stderr}"
    );
    assert!(!stderr.contains("panicked at"), "binary panicked: {stderr}");
}

// ── Opt-in real-LLM end-to-end ──────────────────────────────────────────
//
// The following test drives a full kernel turn against a real LLM
// provider so Linux CI can verify the whole chain (args parser → kernel
// state → HeadlessDeps → run_pisci_turn → event sink → response JSON).
//
// It is **gated** behind `OPENPISCI_TEST_API_KEY` because:
//   * we never want local `cargo test` to spend credits silently;
//   * without a key the binary would fail early and the test would be
//     a functional duplicate of `run_smoke_reports_missing_api_key_by_default`.
//
// Optional knobs:
//   * `OPENPISCI_TEST_PROVIDER` (default `anthropic`)
//   * `OPENPISCI_TEST_MODEL`    (default `claude-haiku-4-5`)
//   * `OPENPISCI_TEST_BASE_URL` (default empty; lets users point at an
//     internal proxy / Bedrock-compatible endpoint)
//   * `OPENPISCI_TEST_PROMPT`   (default `What is 2+2? Reply with just the number.`)
//   * `OPENPISCI_TEST_EXPECT`   (default `4`)

fn api_key_env_for(provider: &str) -> &'static str {
    match provider {
        "openai" | "custom" => "openai_api_key",
        "deepseek" => "deepseek_api_key",
        "qwen" | "tongyi" => "qwen_api_key",
        "minimax" => "minimax_api_key",
        "zhipu" => "zhipu_api_key",
        "kimi" | "moonshot" => "kimi_api_key",
        _ => "anthropic_api_key",
    }
}

#[test]
fn e2e_run_returns_answer_with_real_api_key() {
    let api_key = match std::env::var("OPENPISCI_TEST_API_KEY") {
        Ok(key) if !key.trim().is_empty() => key,
        _ => {
            eprintln!(
                "skipping e2e_run_returns_answer_with_real_api_key — set \
                 OPENPISCI_TEST_API_KEY to enable (and optionally \
                 OPENPISCI_TEST_PROVIDER/MODEL/BASE_URL)."
            );
            return;
        }
    };

    let provider = std::env::var("OPENPISCI_TEST_PROVIDER")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "anthropic".to_string());
    let model = std::env::var("OPENPISCI_TEST_MODEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "claude-haiku-4-5".to_string());
    let base_url = std::env::var("OPENPISCI_TEST_BASE_URL").unwrap_or_default();
    let prompt = std::env::var("OPENPISCI_TEST_PROMPT")
        .unwrap_or_else(|_| "What is 2+2? Reply with just the number.".to_string());
    let expect = std::env::var("OPENPISCI_TEST_EXPECT").unwrap_or_else(|_| "4".to_string());

    let tmp = tempdir().expect("tmp dir");
    let cfg = tmp.path().join("data");
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();

    let api_key_field = api_key_env_for(&provider);
    let config_json = serde_json::json!({
        "provider": provider,
        "model": model,
        "custom_base_url": base_url,
        api_key_field: api_key,
        "workspace_root": workspace.to_string_lossy(),
        "max_iterations": 3,
        "llm_read_timeout_secs": 60,
        "auto_compact_input_tokens_threshold": 200_000,
        // Small-sized turn — no need for fallbacks or long contexts.
        "fallback_models": [],
    });
    std::fs::write(
        cfg.join("config.json"),
        serde_json::to_string_pretty(&config_json).unwrap(),
    )
    .expect("write config.json");

    let output_path = tmp.path().join("response.json");

    let out = Command::new(bin_path())
        .env("OPENPISCI_CONFIG_DIR", &cfg)
        .args([
            "run",
            "--prompt",
            &prompt,
            "--task-timeout-secs",
            "90",
            "--config-dir",
        ])
        .arg(&cfg)
        .args(["--workspace"])
        .arg(&workspace)
        .args(["--output"])
        .arg(&output_path)
        .output()
        .expect("spawn binary");

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "openpisci-headless run failed\nstderr={stderr}"
    );

    let response_text = std::fs::read_to_string(&output_path)
        .unwrap_or_else(|e| panic!("response.json missing ({e}); stderr=\n{stderr}"));
    let response: serde_json::Value =
        serde_json::from_str(&response_text).expect("response.json must be valid JSON");

    assert_eq!(
        response["ok"],
        serde_json::Value::Bool(true),
        "run reported ok=false: {response}"
    );
    assert_eq!(response["mode"], "pisci");
    let text = response["response_text"].as_str().unwrap_or("");
    assert!(
        text.contains(&expect),
        "response_text `{text}` does not contain expected `{expect}`"
    );
}

// Minimal temp-dir helper (std doesn't ship one). Sufficient for these
// tests — no cleanup on panic, which is fine for CI.
mod tempdir_impl {
    use std::fs;
    use std::path::{Path, PathBuf};

    pub struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    pub fn tempdir() -> std::io::Result<TempDir> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("openpisci-cli-it-{nanos}"));
        fs::create_dir_all(&path)?;
        Ok(TempDir { path })
    }
}

use tempdir_impl::tempdir;
