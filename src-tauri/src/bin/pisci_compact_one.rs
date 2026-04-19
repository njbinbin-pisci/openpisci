//! pisci_compact_one — headless one-shot compression CLI.
//!
//! Reads a JSON `BenchRequest` from stdin and writes a JSON `BenchResponse`
//! to stdout. All tracing goes to stderr. Used by the cross-framework
//! compression benchmark (`scripts/bench_compression/run_bench.py`).
//!
//! Usage (PowerShell):
//!   Get-Content sample.json | .\target\debug\pisci_compact_one.exe

use std::io::Read;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();

    // Special subcommand: `--print-runtime` prints the active LLM runtime
    // (provider/model/base_url/api_key) resolved from Pisci's config.json.
    // Used by the cross-framework benchmark harness to route Hermes / Engram
    // / judge calls to the SAME endpoint Pisci is using, for fair comparison.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--print-runtime") {
        let config_path = pisci_desktop_lib::store::settings::Settings::default_config_path();
        match pisci_desktop_lib::store::settings::Settings::load(&config_path) {
            Ok(s) => {
                let provider_base_url = match s.provider.as_str() {
                    "qwen" => "https://dashscope.aliyuncs.com/compatible-mode/v1",
                    "deepseek" => "https://api.deepseek.com/v1",
                    "kimi" => "https://api.moonshot.cn/v1",
                    "zhipu" => "https://open.bigmodel.cn/api/paas/v4",
                    "minimax" => "https://api.minimax.chat/v1",
                    "openai" => "https://api.openai.com/v1",
                    "claude" => "https://api.anthropic.com",
                    _ => "",
                };
                let base_url = if s.custom_base_url.trim().is_empty() {
                    provider_base_url.to_string()
                } else {
                    s.custom_base_url.clone()
                };
                let api_key = s.active_api_key().to_string();
                let rt = serde_json::json!({
                    "provider": s.provider,
                    "model": s.model,
                    "base_url": base_url,
                    "api_key": api_key,
                    "context_window": s.context_window,
                    "max_tokens": s.max_tokens,
                    "llm_read_timeout_secs": s.llm_read_timeout_secs,
                });
                println!("{}", serde_json::to_string(&rt).unwrap());
                return;
            }
            Err(e) => {
                eprintln!("load settings failed: {:#}", e);
                std::process::exit(1);
            }
        }
    }

    let mut raw = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut raw) {
        eprintln!("read stdin failed: {}", e);
        std::process::exit(2);
    }

    let req: pisci_desktop_lib::agent::bench_compact::BenchRequest =
        match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("parse request failed: {}", e);
                std::process::exit(2);
            }
        };

    match pisci_desktop_lib::agent::bench_compact::compact_one(req).await {
        Ok(resp) => {
            let out = serde_json::to_string(&resp).unwrap();
            println!("{}", out);
        }
        Err(e) => {
            eprintln!("compaction failed: {:#}", e);
            std::process::exit(1);
        }
    }
}
