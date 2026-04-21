pub mod bench_compact;
pub mod compaction;
pub mod harness;
pub mod loop_;
pub mod message_utils;
pub mod messages;
pub mod plan;
pub mod rule_preprocess;
pub mod state_frame;
pub mod summary_worker;
pub mod tool;
pub mod tool_receipt;
pub mod vision;

#[cfg(test)]
mod compaction_eval;

// `live_smoke.rs` is a real-network/desktop-bound integration test — it needs
// the full desktop `build_registry` helper, the Chrome browser manager and
// `tracing_subscriber`. It therefore lives under the `pisci-desktop` crate
// (`src-tauri/src/live_smoke.rs`) after the kernel extraction.
